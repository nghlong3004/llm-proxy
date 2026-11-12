# Kiến trúc llm-proxy

## Luồng request

```text
Claude Code / coding agent
  → server HTTP → protocols::Anthropic → domain::GenerationRequest
  → server::routing → accounts::Store → providers::ProviderRegistry
  → providers::codex → authentication refresh → Codex HTTP/SSE
  ← domain::GenerationEvent ← Codex translator
  ← protocols::Anthropic (SSE hoặc buffered Messages)
```

Claude Code sở hữu file, shell, tool và agent loop. Proxy không chạy coding
agent thứ hai và không thực thi tool. Request đang xử lý giữ account đã lease;
request sau đọc registry mới nên thêm/xóa/login không cần restart server.

## Ranh giới và design patterns

- **Adapter** `ProtocolAdapter`: decode request, encode response/event và cả lỗi.
  Schema Anthropic chỉ ở `protocols/anthropic.rs`. Message `system` trong history
  được giữ đúng thứ tự dưới `domain::Role::System`; Codex chuyển sang `developer`.
- **Strategy** `Provider`: generate từ contract trung lập. `ProviderRegistry`
  chọn implementation theo tên; Codex là provider thực tế đầu tiên.
- **Policy** `server/routing.rs`: sticky account, failover, commitment và shutdown;
  độc lập với wire schema Anthropic/Codex.
- **Repository** `accounts::Store`: đóng gói registry/credential, khóa và atomic I/O.

`domain/` không import Axum, Anthropic schema hay Codex wire types. Streaming và
non-streaming cùng dùng một EventStream; accumulator chỉ gom event trung lập.
Integration test `strategies.rs` thay cả provider lẫn protocol, gồm HTTP/SSE lỗi.
Chưa cần plugin runtime hay DI framework; trait object + constructor là đủ.

## Bản đồ source

```text
src/
  main.rs                   Tokio entrypoint
  lib.rs                    module exports
  cli/mod.rs                Clap resource/action commands và wiring
  config.rs                 TOML, model registry/alias, endpoint, log level
  domain/
    generation.rs           request/content/event/output trung lập
    error.rs                FailureKind, ProxyError; không raw upstream body
    accumulator.rs          event → buffered output
  protocols/
    mod.rs                  ProtocolAdapter
    anthropic.rs            Anthropic Messages, SSE, error encoding
    sse.rs                  encode SSE frame
  providers/
    mod.rs                  Provider Strategy + registry
    codex/
      client.rs             authenticated upstream, bounded reads
      request.rs            domain request → native Responses
      stream.rs             native event → domain event, lifecycle/budgets
      sse.rs                fragmented UTF-8/SSE decoder
      quota.rs              hard quota classifier/reset windows
      schema.rs             schema compatibility port trực tiếp từ raine
      reasoning.rs          signed account/generation/model envelope
      search.rs             hosted/explicit WebSearch bridge
  accounts/store.rs         private storage, identity, tickets, sticky selection
  authentication/
    oauth.rs                browser/device login + refresh single flight
    callback.rs             PKCE callback validation
    tokens.rs               OAuth token/identity extraction
  server/
    mod.rs                  HTTP routes, permits, SSE/buffered delivery
    routing.rs              account policy, dispatch, safe failover
  logging/
    mod.rs                  safe metadata, request lifecycle, worker
    rotation.rs             size rotation; active + two archives
packaging/systemd/          user service; SIGINT shutdown, restart policy
tests/                      behavior tests; fake accounts/upstreams only
docs/
  architecture.md           ranh giới và invariants
  conventions.md            tên module, CLI/config và nguồn tham khảo
  implementation-progress.md trạng thái tiếp nối công việc
  superpowers/plans/        kế hoạch đã thống nhất
```

## State và concurrency

Mặc định `$XDG_STATE_HOME/llm-proxy` hoặc `~/.local/state/llm-proxy`;
`--state-dir` / `LLM_PROXY_STATE_DIR` để ghi đè.

- `accounts.json`: account theo thứ tự thêm, active theo provider, generation,
  quota/status và login-operation sequence/tombstone.
- `credentials/<UUID>-<generation>.json`: credential riêng từng lần login;
  publish registry sau khi ghi credential để tránh snapshot nửa chừng.
- `registry.lock`: khóa read-modify-write giữa process; không giữ qua network await.
- `locks/<UUID>.lock`: refresh single flight, kể cả khác process.
- `reasoning.key`: HMAC key riêng; `config.toml`: cấu hình không chứa credential.
- `logs/proxy.jsonl`, `proxy.1.jsonl`, `proxy.2.jsonl`: metadata, khoảng 10 MiB/file.

Thư mục `0700`, file riêng tư `0600`; atomic replace + fsync. File I/O/locks và
CPU token estimation dùng spawn_blocking. Log dùng worker riêng, queue giới hạn
1024 record; quá tải bỏ log thay vì chặn generation. Validation errors có `error_code` tĩnh để phân biệt nguyên nhân mà không log
error.message có thể chứa input. Lỗi native giữ upstream_status và tên trường
trong allowlist; không giữ message/detail thô. upstream_attempt ghi model đã resolve
và effort native, không chỉ alias của client. Không lưu prompt, token,
OAuth code, tool arguments/results, encrypted reasoning, session ID hay raw body.

Login đặt ticket trước network; completion cũ không thể ghi đè login mới hoặc
hồi sinh account đã xóa. Refresh/quota mang generation cũ không sửa login mới.
Quota đồng thời giữ reset dài nhất và không ghi đè trạng thái cần login lại.
Xóa unpublish account trước khi xóa credential.

## Failover và continuation

Giữ account active đến hard quota hoặc credential bị thu hồi. Chọn tiếp theo
thứ tự thêm; account vừa reset không giành lại account active còn dùng tốt.
HTTP 429 đơn lẻ, lỗi model/quyền, mạng/5xx không được coi là hết quota.
Các cửa sổ quota cùng hết phải cùng reset; không biết reset thì cooldown 60 giây.

Trước message/content event có thể chuyển account với lịch sử portable. Sau khi
đã phát output, trả lỗi; không tự replay text/tool hay trộn hai phản hồi. Lỗi mạng
không rõ đã xử lý cũng không replay. Giới hạn 64 lần dispatch, không retry vô hạn.

Encrypted reasoning chỉ replay khi HMAC và account/generation/model khớp.
Đổi account giữ text/tool/result/summary và bỏ native state không tương thích.
Không dùng previous_response_id/native server compaction; Claude Code quản lý
/compact bằng tóm tắt portable.

32 generation permits; request 32 MiB; frame SSE 8 MiB; output/retained data
16 MiB; connect 15 giây, header/read idle 300 giây, heartbeat 15 giây.
Disconnect/drop body hủy upstream và trả permit. Ctrl+C phát shutdown signal.
Token count hiện dùng estimator Codex local; provider mới cần estimator riêng
khi triển khai thật. Route mới cho protocol khác cần tự đăng ký ở HTTP router.

## Chạy nền và cấu hình client trên máy user

`packaging/systemd/llm-proxy.service` quản lý chính binary foreground `serve`,
không có daemon/PID-file implementation riêng. ExecStart dùng binary đã install
trong ~/.local/bin; service áp dụng cùng state directory mặc định. SIGINT khớp
shutdown handler hiện tại. Unit enabled theo default.target của user manager;
không thay đổi system-wide linger hoặc WSL boot config.

Bốn env trỏ gateway được merge vào ~/.claude/settings.json, thuộc cấu hình client,
không phải server config. Child process llm-proxy không thể export biến ngược vào
shell cha. Không thay credential Codex CLI/Claude; token `local` chỉ dành listener.

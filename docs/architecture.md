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
- **Strategy** `RequestRoutingPolicy`: chọn immutable `RequestRoute { provider,
  model }` cho mỗi generation, từ neutral `RequestPurpose`. Implementation
  `ConfiguredRequestRoutingPolicy` chỉ đọc config, không biết Codex/JEV; inject
  qua AppState như Provider/ProtocolAdapter. Sau này provider JEV dùng seam cũ,
  không cần nhánh riêng trong HTTP handler.
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
    request_policy.rs       replaceable per-request provider/model Strategy
    routing.rs              account policy, dispatch, safe failover
  logging/
    mod.rs                  safe metadata, request lifecycle, worker
    rotation.rs             size rotation; active + two archives
packaging/systemd/          user service; SIGINT shutdown, restart policy
Dockerfile                  Rust build stage → non-root Debian runtime
compose.yaml                loopback host port + private named state volume
.dockerignore               allowlisted source-only build context
tests/                      behavior tests; fake accounts/upstreams only
docs/
  architecture.md           ranh giới và invariants
  conventions.md            tên module, CLI/config và nguồn tham khảo
  implementation-progress.md trạng thái tiếp nối công việc
  superpowers/plans/        kế hoạch đã thống nhất
```

## Client-side action review

Anthropic Adapter có pure hook `request_purpose(&Value)`: chỉ non-streaming,
không tools, exact classifier prefix ở đầu system string/một system text block
mới nhận `ActionReview`. Prompt/schema recognition dừng ở protocol boundary;
`domain::RequestPurpose` chỉ có Conversation/ActionReview. Adapter khác có
implementation mặc định Conversation. Hook generation-only; count_tokens không
nhận diện hoặc apply route classifier. Đây là heuristic routing, không cấp quyền.

Server validate model gốc, evaluate policy đúng một lần, validate tuyến hiệu dụng,
chọn account/provider theo route qua mọi lần failover. Default conversation giữ
precedence lỗi empty-account cũ; review và provider override được kiểm tra provider
trước select. Route không mutate Config/default provider hay affinity toàn cục.
`[auto_review]` provider/model optional, từng field kế thừa độc lập. Explicit model
validate qua existing registry; unknown provider bị từ chối ở CLI startup và runtime,
không fallback. Model string giữ suffix service tier; response Start được remap
về model client khi override. Log tách requested/routed/native model bằng request ID.

Classifier giữ transcript/schema/effort, nhưng session_id=None và Codex translator
không replay encrypted reasoning kể cả signature hợp lệ; summary vẫn là text.
Không sửa/parse model output thành verdict hay chạy tools. Interface Provider và
RequestContext giữ nguyên. Fake provider/policy integration test chứng minh OCP/DIP;
chưa triển khai JEV vì chưa có API/auth contract. Model khác không được coi là
classifier an toàn tương đương Claude chỉ vì wire test pass.

Client opt-in `CLAUDE_CODE_AUTO_MODE_SERVER=0` dùng classifier requests riêng; proxy
không implement/fabricate server safeguards protocol. Không tự sửa settings/service
user; live Auto/Codex acceptance cần kiểm tra riêng được user cho phép.

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
Xóa unpublish account trước khi xóa credential. Re-login publish generation mới
bền vững trước khi xóa credential files cũ dưới registry lock; lease đã lấy giữ
credential trong RAM. Cleanup lỗi chỉ log metadata và retry ở login sau, không
báo rollback giả cho login đã publish. Account removed/changed trước dispatch
được phân biệt rõ với storage corruption để reselect an toàn, không replay lỗi
storage/network chung.

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

32 admission permits dùng chung cho generation và token count, lấy trước khi đọc
JSON; overload trả 503 ngay, body-read timeout 30 giây. Request 32 MiB; frame SSE
8 MiB; output/retained data 16 MiB, gồm initial block/tool identifiers. Buffered
response giữ permit đến khi body drop/đọc xong; count_tokens và blocking storage
preparation giữ shared permit đến khi job thật kết thúc dù HTTP waiter bị cancel.
Provider có budget riêng 32 blocking tokenizer jobs, slot nằm trong closure nên
cancel generation không tạo queue tokenizer vô hạn. Preflight account được tái dùng cho dispatch đầu,
không select/read hai lượt; provider vẫn revalidate generation/credential.

Connect 15 giây, upstream header/read idle 300 giây, heartbeat 15 giây.
Disconnect/drop body hủy inference. OAuth refresh đã dispatch là transaction riêng,
bounded/tracked (64 task slots, prune handles đã xong), giữ per-account lock qua
parsing/persistence dù client mất kết nối; shutdown hủy
inference rồi drain refresh trước khi runtime exit. Không bảo đảm lossless trước
SIGKILL, transport ambiguity hoặc disk failure. Token count hiện dùng estimator
Codex local; provider mới cần estimator riêng khi triển khai thật. Route mới cho
protocol khác cần tự đăng ký ở HTTP router. Giới hạn byte/concurrency không phải
RAM cap tuyệt đối: request/output/JSON copies cùng tồn tại có thể cần hàng trăm
MiB hoặc hơn ở workload near-limit.

## Chạy nền và cấu hình client trên máy user

`packaging/systemd/llm-proxy.service` quản lý chính binary foreground `serve`,
không có daemon/PID-file implementation riêng. ExecStart dùng binary đã install
trong ~/.local/bin; service áp dụng cùng state directory mặc định. SIGINT khớp
shutdown handler hiện tại. Unit enabled theo default.target của user manager;
không thay đổi system-wide linger hoặc WSL boot config.

Bốn env trỏ gateway được merge vào ~/.claude/settings.json, thuộc cấu hình client,
không phải server config. Child process llm-proxy không thể export biến ngược vào
shell cha. Không thay credential Codex CLI/Claude; token `local` chỉ dành listener.

## Docker packaging

Multi-stage image build Rust stable trong container; runtime Debian slim chỉ chứa
binary, CA/runtime libraries và LICENSE. Binary là ENTRYPOINT; arguments của
`serve` nằm ở CMD/Compose để `accounts` thay command trực tiếp được.
Runtime UID/GID `10001:10001`, state tại `/var/lib/llm-proxy/state`. Named volume
mount parent `/var/lib/llm-proxy`, copy-up thư mục con đã chown/chmod `0700` trong
image; không có root entrypoint sửa quyền runtime. Storage tiếp tục dùng đúng
atomic I/O/locking/private permissions của native CLI.

`listen_address: IpAddr` mặc định `127.0.0.1`; CLI override cho container là
`0.0.0.0`, không đổi native default. Compose publish
`127.0.0.1:${LLM_PROXY_PORT:-18765}:18765`; network/container không tin cậy vẫn phải
bị cách ly vì listener không có client auth. `LLM_PROXY_MODEL` chọn model override;
không tự import host state/config. Device-code OAuth dùng cùng account storage,
không publish browser callback port. Config/account mutations dùng cùng named
volume; recreate giữ state, còn `down -v` xóa nó.

Docker/Compose dùng SIGINT và 45 giây shutdown để chứa OAuth timeout 30 giây cùng
guarded persistence, khớp Ctrl+C handler; không thêm
daemon hoặc PID management. `.dockerignore` chỉ cho phép source/manifest/license
cần build. Smoke phải dùng project/volume riêng và random host port, không đụng
service user hoặc credential thật; `/health` ready=false là bình thường khi chưa
login, không phải process unhealthy.

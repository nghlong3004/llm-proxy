# Kiến trúc llm-proxy

## Luồng và ranh giới

```text
Claude Code → server HTTP → ProtocolAdapter → domain::GenerationRequest
  → RequestRoutingPolicy → accounts::Store → ProviderRegistry → Codex HTTP/SSE
Claude Code ← ProtocolAdapter ← domain::GenerationEvent ← Codex translator
```

Claude Code sở hữu filesystem/shell/tools/agent loop; proxy không chạy tool.
Streaming và buffered dùng cùng EventStream, accumulator chỉ gom event trung lập.

| Module | Trách nhiệm |
| --- | --- |
| `main.rs`, `cli/` | Entrypoint, Clap và wiring; không business logic |
| `config.rs` | TOML, model/alias registry, endpoints |
| `domain/` | Request/content/event/error/accumulator; không Axum hay wire schema |
| `protocols/` | Adapter decode/encode request, response, SSE và lỗi Anthropic |
| `providers/` | `Provider` Strategy + registry |
| `providers/codex/` | Client, translation, schema/search/reasoning, bounded SSE/quota |
| `accounts/store.rs` | Repository: identity, private storage, tickets, sticky selection |
| `authentication/` | Browser/device OAuth, callback/PKCE, refresh single flight |
| `server/` | HTTP admission/delivery/cancel; `routing.rs` failover; `request_policy.rs` route Strategy |
| `logging/` | Metadata JSONL, bounded worker, rotation |
| `tests/` | Behavior/replaceable Strategy tests, synthetic accounts/upstreams |

Trait objects + constructors đủ cho Adapter/Strategy injection; chưa cần plugin/DI
framework. History `system` text giữ nguyên vị trí, Codex map thành `developer`.

## Action-review routing

- `ProtocolAdapter::request_purpose(&Value)` nhận `ActionReview` khi non-streaming,
  không tools, exact classifier prefix ở đầu system string/text block. Không scan
  user text; đây là routing hint, không xác thực/quyền. Adapter khác mặc định
  `Conversation`; `count_tokens` không áp dụng policy này.
- `RequestRoutingPolicy` chọn immutable `{ provider, model }` một lần/request từ
  neutral purpose. Validate model gốc rồi route hiệu dụng; failover giữ cùng route.
  Conversation mặc định giữ precedence lỗi no-account cũ; review/provider override
  validate provider trước select. Không mutate config/default provider/affinity.
- `[auto_review]` mặc định model `gpt-6-luna`, provider kế thừa `default_provider`;
  model/provider explicit vẫn override qua registry. Alias như `model = "sonnet"`
  theo mapping hiện hành; suffix service tier được giữ. Không hardcode Luna trong
  Provider. Unknown model/provider trả lỗi, không fallback ngầm.
- Giữ transcript/schema/effort; bỏ session/cache identity và encrypted-reasoning
  replay (giữ summary text). Response Start giữ model client đã yêu cầu; log phân
  biệt requested/routed/native model theo request ID. Không parse output thành
  verdict hoặc tự tạo allow. `Provider`/`RequestContext` không đổi interface.
- Client dùng `CLAUDE_CODE_AUTO_MODE_SERVER=0`; không implement server safeguards.
  JEV chờ API/auth contract, chưa đăng ký provider. Mock wire/fail-closed pass không
  chứng minh safety tương đương Claude, entitlement hay tỷ lệ tiết kiệm quota thật.

## State, identity và refresh

State: `$XDG_STATE_HOME/llm-proxy` hoặc `~/.local/state/llm-proxy`; override bằng
`--state-dir`/`LLM_PROXY_STATE_DIR`. Thư mục `0700`, file `0600`, atomic replace + fsync.

- `accounts.json`: insertion order, active/provider, quota/status/generation, login
  sequence/tombstone. Identity gồm user + workspace. `credentials/<UUID>-<generation>.json`
  publish trước registry; request đã lease giữ credential trong RAM.
- `registry.lock`: cross-process read/modify/write, không giữ qua network await.
  `locks/<UUID>.lock`: refresh single flight. Blocking I/O/locks dùng `spawn_blocking`.
- Login ticket trước OAuth chặn completion cũ ghi đè/resurrect. Refresh/quota cũ không
  sửa generation mới; quota đồng thời giữ reset dài nhất và NeedsLogin. Remove
  unpublish trước xóa file; re-login publish bền vững rồi dọn generation cũ dưới lock.
  Cleanup failure không rollback giả; log metadata/retry ở login sau.
- `reasoning.key`: HMAC envelope chỉ replay khi account/generation/model khớp. Đổi
  account giữ text/tool/result/summary, bỏ native state. Không `previous_response_id`
  hoặc native compaction; Claude Code `/compact` tạo history portable.
- Refresh đã dispatch là transaction riêng: giữ account lock qua parse/persistence,
  tracked/bounded 64 slots, prune completed handles. Disconnect chỉ hủy inference;
  graceful shutdown drain refresh. SIGKILL/disk failure/transport ambiguity vẫn có rủi ro.

## Routing, bounds và cancellation

Giữ active đến hard quota/definite credential rejection; account reset không giành
lại active tốt. Chọn theo insertion order, tối đa 64 dispatch. Generic 429, model/
permission, mạng/5xx không phải quota; cửa sổ quota phải cùng reset, thiếu reset dùng
cooldown 60s. Removed/changed trước dispatch được reselect, không gộp storage errors.
**Không replay sau message/content event hoặc khi không rõ upstream đã xử lý.**

| Bound | Giá trị / ownership |
| --- | --- |
| Admission | 32 permits chung generation/count, trước JSON; overload 503 |
| Request/SSE/output-retained | 32 MiB / 8 MiB / 16 MiB, tính initial tool metadata |
| Body upload/connect/upstream idle | 30s / 15s / 300s; SSE heartbeat 15s |
| Blocking tokenizer | Budget riêng 32 jobs/provider; slot nằm trong closure |

Buffered response giữ permit đến body drop/đọc xong; blocking storage/count jobs giữ
permit đến khi job thật xong dù HTTP waiter cancel. Drop/disconnect hủy inference;
preflight account dùng lại cho dispatch đầu, provider vẫn revalidate generation.
Các bound không phải RAM cap: input/JSON/output copies có thể đồng thời rất lớn.

## Logging và deployment

`logs/proxy.jsonl` + hai archives, khoảng 10 MiB/file; worker queue 1024, đầy thì bỏ
record (không phải audit log). Chỉ proxy metadata, static error_code, native status
và allowlisted parameter; không raw error/body, prompt, token, OAuth code, tool data,
encrypted reasoning hay session ID. Health/models/list không tạo access log.

Systemd quản lý foreground `serve` từ `~/.local/bin`, không daemon/PID riêng; config
client thuộc `~/.claude/settings.json`, không phải server env. Cập nhật binary cần
restart có chủ đích. User manager/WSL lifecycle áp dụng; không tự bật linger.

Docker multi-stage → Debian slim, binary/CA/runtime libs/`LICENSE`, UID/GID 10001.
Named volume mount `/var/lib/llm-proxy`, copy-up state con `0700`; không root chown
entrypoint hay import host credentials. Native bind mặc định `127.0.0.1`; container
explicit `0.0.0.0`, Compose publish **host loopback** vì listener không client auth.
Device OAuth không cần callback port; `down -v` xóa state. SIGINT/grace 45s chứa
OAuth timeout 30s + persistence. Build context allowlist; smoke dùng project/state
synthetic/port riêng. Health ready=false khi chưa login không có nghĩa process hỏng.

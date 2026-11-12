# Tiến độ / context — 2026-11-12

Kế hoạch: `docs/superpowers/plans/2026-11-12-llm-proxy-v1.md`.
Đọc thêm `architecture.md`, `conventions.md`, `AGENTS.md` trước khi sửa.

## Trạng thái hiện tại

Đã triển khai CLI, config/model registry, private account storage, browser/device
OAuth + refresh, sticky quota failover, Codex HTTP/SSE, Anthropic streaming/buffered,
tool/ảnh/reasoning/structured output/search và portable history. Chưa nghiệm thu
bằng account subscription thật và Claude Code thật; mock không chứng minh được
entitlement/lane hoặc toàn bộ agent loop của dịch vụ thật.

Theo yêu cầu mới, đổi tên `core` → `domain`, `ingress` → `protocols`, `backend`
→ `providers`, `auth` → `authentication`, Codex nằm trong `providers/codex`.
CLI canonical và config theo `docs/conventions.md`. State/TOML cũ được đọc bằng
serde aliases; CLI chưa phát hành nên không giữ flag cũ. Domain là contract do
project thiết kế; tham khảo native protocol của raine, account constraints của
ZeroSub; không chép “core” từ hai repo. Giữ MIT attribution.

Log mới: JSONL metadata ở state/logs, active + hai archives, khoảng 10 MiB/file,
worker queue 1024. Chỉ mốc request/account attempt/failover/completion/error/cancel
và server lifecycle; không prompt/token/tool data/reasoning/session/chunk/heartbeat.
`log_level` trong TOML, dependency logging tắt. Đây không phải audit log bảo đảm
không mất record khi quá tải. Việc list/health/models không tạo access log.

## Review độc lập và regression

Reviewer `/root/final_review` đã báo sáu vấn đề Important; không Critical.
Đã sửa theo test regression:

1. Login ticket trước OAuth, sequence + identity tombstone chặn completion cũ
   ghi đè login mới hoặc hồi sinh account đã xóa. Account suite 8 tests đã pass.
2. Tool arguments phải hợp lệ trước Finish; chặn delta sau block đóng; không
   phát thêm args/signature khi terminal payload lặp lại.
3. Budget trước khi giữ reasoning/citations, cap index/count, tính các bản sao.
4. Khôi phục reasoning summary từ summary_text.done/terminal-only, không lặp delta.
5. Refusal events/terminal refusal được trả thành text thay vì mất nội dung.
   Stream suite 12 tests đã pass cho các mục 2–5.
6. ProtocolAdapter sở hữu encode_error/encode_stream_error; domain error không
   phụ thuộc Axum/Anthropic. Strategy suite 3 tests vừa pass.

Log regression: RED thiếu module được quan sát; GREEN 2 tests: xoay file không
cắt record, đúng quyền riêng tư và flush JSON trên shutdown worker.
Bổ sung regression quota reports đồng thời: RED đã tái hiện reset bị rút ngắn;
fix giữ max reset và NeedsLogin: GREEN, account suite 9 tests pass.

## Kiểm tra đã có / còn phải chạy

Snapshot sau đổi tên/log mới nhất đã pass fmt và strict Clippy
(`--all-targets --all-features --locked -- -D warnings`). Full tests với
`--include-ignored`: **52 pass, 0 fail, 0 ignored**:

| Suite | Pass |
| --- | ---: |
| Unit: auth/config/log privacy/lifecycle | 7 |
| Accounts: identity/concurrency/selection | 9 |
| CLI/config naming | 3 |
| HTTP/OAuth localhost | 4 |
| Log rotation/worker flush | 2 |
| SSE framing/quota | 4 |
| Server policy/cancellation | 5 |
| Strategy/Adapter replacement + errors | 3 |
| Native stream lifecycle/budgets | 12 |
| Request/reasoning translation | 3 |

HTTP composed test chứng minh request Anthropic → Codex client thật → mock
account A hard quota → B success; lượt sau giữ B; chạy cả buffered và SSE.
Incoming client token không được forward. Tất cả token/account đều giả.

`CARGO_HOME=/tmp/llm-proxy-cargo ./scripts/check-local.sh` hoàn tất, exit 0:
fmt, strict Clippy, 52 tests gồm localhost và release build đều pass.
Release build: `Finished release profile [optimized]` (3m06s).
Log output: `/tmp/llm-proxy-final-checks.log`.

Binary release smoke đã pass, exit 0, qua script `/tmp/llm-proxy-smoke.py`;
output `/tmp/llm-proxy-release-smoke.log`:

- Khởi động với CLI canonical và state tạm, config log_level=info.
- Health/models/token count hoạt động; no-account trả 503 cho cả yêu cầu
  stream/buffered, model không hợp lệ trả 400, các lỗi có x-request-id riêng.
- Với RUST_LOG=trace, đúng 5 record: startup, 3 request failures, shutdown.
  Các lần health/model/count không tạo log thừa; không log dependency thô.
- Không xuất hiện marker prompt, bearer hoặc model lỗi trong file.
- Thư mục log 0700/file 0600; SIGINT exit 0 và worker flush đầy đủ.
- Không dùng account/credential thật, không gọi dịch vụ inference bên ngoài.

## Cài CLI trên máy hiện tại

User làm rõ chỉ muốn gọi `llm-proxy` từ PATH thay cho đường dẫn target/release;
không yêu cầu menu hoặc đổi hành vi khi thiếu subcommand. Không sửa CLI.
Đã chạy `CARGO_HOME=/tmp/llm-proxy-cargo cargo install --path . --locked --offline
--root /home/nghlong3004/.local`, exit 0. Binary tại
`/home/nghlong3004/.local/bin/llm-proxy`, thư mục đã có trong PATH.
Đã kiểm tra từ `/tmp`: command -v, --version (0.1.0), accounts login --help đều
pass. Chưa đăng nhập account thật; không đọc credential. Khi sửa source, cần
chạy lại cargo install để cập nhật binary đã cài.

## Sửa HTTP 400 khi Claude Code gửi system message trong history

User báo InvalidRequest 400 ngay 0ms, chưa có upstream_attempt. Đã tái hiện
bằng Claude Code 2.1.322, cấu hình tạm không credential thật: request chứa
messages role [user, system]; response báo Message role must be user or assistant.
Không lưu prompt/request body; chỉ kiểm tra role/type/schema và lỗi validation.

Root cause: Anthropic adapter giới hạn user/assistant, thiếu in-history system
message mà Claude Code gửi. Fix thêm Role::System trung lập, chỉ nhận text,
Codex dịch sang developer ngay đúng vị trí history ở cả full/Lite lane;
không hoist vào global system và không hạ thành user text. Tham khảo mapping
system → developer của raine (revision đã ghi trong THIRD_PARTY_NOTICES).

Regression translation: RED 2 tests tái hiện trước fix, GREEN 5 tests sau fix;
unit suite 7 tests pass. Thêm error_code tĩnh cho validation/quota/transport,
không ghi error.message có thể chứa input; log privacy regression đã pass.
Claude Code thật đã nhận PROXY_OK qua mock sau role fix. Smoke còn phát hiện
lỗi compaction heuristic: system prompt bình thường có cả “summariz” và “context
window”, khiến medium/high bị hạ thành low. Đã xác nhận raw effort medium/high
vào gateway và low ra native mock; không lưu prompt. Regression RED tái hiện;
đã bỏ heuristic keyword rộng, chỉ giữ explicit metadata và marker compact
cụ thể của Claude Code. Translation suite hiện 7 tests GREEN, giữ thứ tự system
message, không downgrade coding bình thường, vẫn cap compaction thật xuống low.
Full check `CARGO_HOME=/tmp/llm-proxy-cargo ./scripts/check-local.sh`: exit 0,
fmt + strict Clippy + 56 tests (0 fail, 0 ignored) + release build pass.
Output `/tmp/llm-proxy-400-final-checks.log`.
Release smoke qua Claude Code 2.1.322 thật + localhost Codex mock: PASS cho đúng
lệnh user (không --effort, client gửi medium) và --effort high. Native nhận
medium/high tương ứng, model gpt-6.1-sol, roles [developer, user, developer],
client nhận PROXY_OK. Output `/tmp/llm-proxy-400-client-final.log`.
Chỉ kiểm tra client + protocol; không chứng minh quyền model/compatibility
với dịch vụ subscription thật. Không dùng credential/quota thật.
Đã cargo install --force vào /home/nghlong3004/.local, exit 0;
output `/tmp/llm-proxy-400-install.log`. cmp xác nhận binary đã cài giống hệt
release vừa test; --version chạy được từ /tmp. User cần Ctrl+C server cũ rồi
chạy lại lệnh serve hiện tại; không cần login lại. Không dùng account/quota thật; server user cần restart
sau khi thay binary. Môi trường hiện đã có Git metadata (toàn bộ source untracked);
không git init, commit hoặc push tự động.

## Đối chiếu wire contract với source raine sau lỗi native 400

User báo request qua ingress rồi bị Codex từ chối sau ~1.8s. Log cũ gộp native
400/404/422 thành InvalidRequest 400, không đủ xác định lý do thật. Chưa đọc
credential hay tự tiêu quota để kiểm tra phiên thật.

Theo yêu cầu user, fetch lại main của raine: vẫn là fe107180f7a69c6d63dd4421a6567cf641c1f103.
Đã build/chạy chính translator + header builder upstream bằng helper chỉ dùng
synthetic input/StoredAuth giả. Xuất sáu expected payload/headers vào
`tests/fixtures/codex/raine-contract.json`; helper và provenance giữ cùng thư mục.
Không dùng output do code của mình tạo làm expected fixture.

Bốn golden contract tests RED trước fix, GREEN sau fix: Lite/full medium/high,
in-history system, tool schema và structured output, protocol headers.
Đã port trực tiếp hai schema helpers (MIT) thay vì tự viết lại. Sửa các sai khác:
Lite reasoning.context=all_turns; strip pattern chỉ ở schema-bearing keywords;
normalize required khi strict structured output; bỏ tool_choice auto mặc định;
thêm x-codex-beta-features và x-codex-window-id theo upstream.
Full-lane originator/user-agent giữ tên ứng dụng llm-proxy; fixture test normalize
đúng hai trường branding đó. Lite so sánh nguyên vẹn. Chưa có kết luận rằng một
sai khác cụ thể chính là nguyên nhân native error của user.

Lỗi upstream giữ status gốc và parameter trong allowlist, mã chẩn đoán tĩnh;
không giữ/log raw message/detail/body. Regression RED generic invalid_request,
GREEN sáu protocol tests; unknown code/param/message không giữ dữ liệu echo.
Sáu HTTP tests GREEN, gồm native Lite request phải khớp nguyên payload translator
tham khảo và case 422 giữ diagnostic an toàn. upstream_attempt INFO nay ở native
client, ghi model đã resolve + effort/lane; router generic attempt chỉ ở DEBUG.

Full checks exit 0: fmt, strict Clippy, **64 tests pass (0 fail/ignored)**,
release build pass (40.71s). Log `/tmp/llm-proxy-reference-final-checks.log`.
Release Claude Code 2.1.322 smoke với upstream mock siết điều kiện Lite context
và beta header: PASS, client nhận PROXY_OK, medium/high giữ nguyên.
Log `/tmp/llm-proxy-reference-client-smoke.log`. Binary CLI/privacy/shutdown
smoke PASS: `/tmp/llm-proxy-reference-cli-smoke.log`.
Đã reinstall global bằng cargo install --force, exit 0;
`/tmp/llm-proxy-reference-install.log`, cmp binary đã cài khớp release vừa test;
--version chạy được ngoài project. Source/helper/fixture không chứa credential thật.
Native subscription thật vẫn cần user restart server và gửi lại request;
không báo đã sửa chắc chắn lỗi dịch vụ chỉ dựa trên mock/fixture.

## Chạy nền + Claude Code chỉ cần một lệnh

User yêu cầu background và lưu mặc định bốn env gateway để chỉ cần gõ claude.
Máy WSL có systemd 259 và user manager running. Thêm unit ở
`packaging/systemd/llm-proxy.service`, đã systemd-analyze --user verify pass.
Cài vào ~/.config/systemd/user, daemon-reload và enable --now; model hiện tại
là gpt-6.1-sol, binary ~/.local/bin/llm-proxy. KillSignal=SIGINT đúng handler,
Restart=on-failure, restart loop có giới hạn. Không thêm daemon Rust riêng.

Trước chuyển đã kiểm tra log metadata: 0 generation đang xử lý. Process
foreground cũ được SIGINT và thoát bình thường. Service mới chạy dưới user manager,
không gắn terminal. Kiểm tra thực tế: is-active=active, is-enabled=enabled,
SubState=running, /health status=ok, ready=true, usable_accounts=1.
Không gửi request inference hay sử dụng quota để kiểm tra.

Đã merge 4 env vào /home/nghlong3004/.claude/settings.json: BASE_URL localhost18765,
AUTH_TOKEN=local, DISABLE_NONESSENTIAL_TRAFFIC=1, DISABLE_NONSTREAMING_FALLBACK=1.
Atomic replace private, kiểm tra JSON và mọi key/env khác giữ nguyên. Bản gốc
được backup một lần ở ~/.claude/settings.json.before-llm-proxy (0600), ngoài repo;
không in nội dung settings/credential. Phiên Claude mới dùng config user này.

Không sửa Rust; không cần build/reinstall binary cho thay đổi service/client config.
Không đổi Linger (hiện no); service theo lifecycle user manager, WSL shutdown
vẫn kết thúc process. README và architecture đã cập nhật hướng dẫn start/stop/logs
và persistent client env. Sau cập nhật binary về sau cần restart user service để
process đang chạy nạp binary mới; không chạy thêm foreground trên cùng cổng.

## Hướng dẫn máy Ubuntu mới từ GitHub

User hỏi cài trên Ubuntu khác sau khi nối GitHub. Đã xác nhận origin là
`git@github.com:nghlong3004/llm-proxy.git`, branch main; local chưa có HEAD,
`git ls-files`=0 và `git ls-remote origin HEAD` không có ref. Source vẫn untracked:
chỉ gắn remote chưa publish code. Không tự add/commit/push theo AGENTS.md.

Thêm `docs/ubuntu-setup.md`, link từ README: apt prerequisites + Rust stable,
clone/build/install ~/.local/bin, PATH, Claude Code native installer, login lại
trên máy mới (device-code cho headless), systemd user, optional linger cho SSH,
merge client env một lần, daily claude, cập nhật binary rồi restart service.
Không sync credential qua Git. Hướng dẫn giả định proxy/Claude Code cùng máy.

Đã đối chiếu installer Rust và Claude Code với tài liệu chính thức. Kiểm tra
bash -n cho toàn bộ khối shell PASS; chạy nguyên Python snippet trong temp dirs
PASS: fresh settings, giữ other keys/env, rerun, backup/replace 0600, malformed JSON,
non-object/env sai kiểu và symlink đều không bị ghi đè. systemd-analyze --user
verify cho packaged unit PASS. Không sửa Rust, không reinstall hay đổi service
đang chạy ở máy hiện tại. Chưa chạy end-to-end trên Ubuntu VM sạch.

## Initial commit và .gitignore

User đã yêu cầu rõ add + commit toàn bộ project, chưa yêu cầu push. Bổ sung
.gitignore cho .env.*, config.toml local, account registry/credentials, locks,
reasoning key và logs khi state nằm trong checkout. Cargo.lock, config example,
source, docs và synthetic reference fixtures vẫn được đưa vào Git.

Kiểm tra git check-ignore cho build/state/env và allowlist source/lock/examples
PASS. Scan addable files không thấy private-key block, GitHub/OpenAI token hoặc
JWT literal có dạng credential; fixture chỉ dùng mock/synthetic dữ liệu.
Git author identity đã có, không tự đổi name/email.

Pre-commit full checks mới nhất: fmt, strict Clippy, 64 tests (0 fail/ignored)
và release build đều PASS, exit 0. Output /tmp/llm-proxy-precommit-checks.log.
Initial commit gom source/lockfile/tests/docs/CI/systemd; không có credential,
build artifacts hoặc runtime log; không push trong mốc này. Hash xem git log.
Native subscription acceptance và clean Ubuntu VM acceptance vẫn chưa xác nhận.

## Công việc tiếp theo

Nghiệm thu manual còn chờ: đăng nhập **account riêng của proxy** qua browser hoặc
device-code, rồi Claude Code thật đọc/sửa file, chạy test/subagent, ảnh, WebSearch
và /compact. Không đọc credential Codex CLI sẵn có để tự làm bước này. Mock quota
đã chứng minh failover; không cố tiêu quota thật. Khi có kết quả manual, ghi lại
model/lane và hành vi pass/fail ở đây; không lưu token hay nội dung hội thoại.

Các mốc tự động v1 đã kiểm tra; live Claude Code/subscription chưa được xác nhận.

## Môi trường và lệnh tiếp nối

Rust/Cargo 1.99.1, edition 2024, stable toolchain. Dependency stable mới nhất đã
fetch, Cargo.lock được giữ. tracing-appender 0.2.5 bổ sung cho worker log.

```sh
CARGO_HOME=/tmp/llm-proxy-cargo cargo test --offline --locked -- --include-ignored
CARGO_HOME=/tmp/llm-proxy-cargo ./scripts/check-local.sh
```

Môi trường trước cấm DNS/socket, user đã tải cache và chạy mock ngoài sandbox.
Quyền hiện tại đã cho network/localhost; có thể tự chạy kiểm tra, không cần hỏi
user fetch/chạy hộ nữa. Không đọc auth.json/credential account thật để tự nghiệm thu.
Ở mốc đầu workspace chưa có `.git`; hiện user đã khởi tạo Git. Không tự git init/push/publish.

## Giới hạn v1

Chỉ Anthropic Messages và Codex provider thực tế. Strategy/Adapter thứ hai mới
được chứng minh bằng fake integration test. Không chạy tools trong proxy.
Không native server compaction, previous_response_id, ảnh URL/audio/document,
universal plugin runtime hoặc UI. Token count local là ước lượng.

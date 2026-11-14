# llm-proxy

Proxy local viết bằng Rust, cho phép **Claude Code gọi Codex bằng subscription
ChatGPT**, đăng nhập nhiều account và chuyển sang account tiếp theo khi hết quota.
Claude Code vẫn thực thi tool, sửa file, chạy lệnh và điều phối subagent.

```text
Coding agent → Protocol Adapter → domain request → AccountPool → Provider Strategy
Coding agent ← Protocol Adapter ← domain events  ←             ← Codex
```

V1 có adapter Anthropic Messages và backend Codex. Các interface được tách để thêm
agent/giao thức hoặc backend khác; phiên bản này chưa có backend khác được đăng ký.

Máy Ubuntu mới: xem [hướng dẫn cài từ GitHub](docs/ubuntu-setup.md), gồm Rust,
Claude Code, login, chạy nền và cấu hình để chỉ cần gõ `claude`.

## Chạy bằng Docker — không cần cài Rust

Cần Docker Engine + Docker Compose. Sau khi clone repository:

```sh
docker compose up -d --build
docker compose exec proxy llm-proxy accounts login --name personal --login-method device-code
docker compose exec proxy llm-proxy accounts list
```

Rust chỉ nằm trong build stage; runtime image chạy binary bằng user non-root.
Đây là build image local từ source, chưa có prebuilt image được publish. Lần build
đầu cần mạng để tải base image và dependency. Device-code login in URL/code để bạn
mở trong trình duyệt trên host; không cần Rust, callback port hay credential Codex CLI.
Không copy credential host vào container.

Proxy được publish tại `http://127.0.0.1:18765`; dùng cấu hình Claude Code ở phần
[Kết nối Claude Code](#kết-nối-claude-code) bên dưới. Nếu systemd/foreground proxy
đang chiếm cổng này, chọn cổng khác cho Docker, không chạy hai instance cùng cổng:

```sh
LLM_PROXY_PORT=18766 LLM_PROXY_MODEL=gpt-6.1-sol docker compose up -d --build
# Khi đó ANTHROPIC_BASE_URL=http://127.0.0.1:18766
```

Giữ cùng giá trị `LLM_PROXY_PORT`/`LLM_PROXY_MODEL` ở các lần `up` tiếp theo; có thể
đặt trong file `.env` local (không commit). Model phải thuộc quyền account của bạn;
đổi biến model rồi chạy lại `up -d` để recreate container. Docker dùng model policy
mặc định của ứng dụng; config trên host không được tự copy vào volume.

```sh
docker compose logs -f proxy
docker compose exec proxy llm-proxy accounts list --output-format json
docker compose exec proxy llm-proxy accounts remove ACCOUNT_ID
docker compose stop
# Start lại container đã tạo:
docker compose start
# Cập nhật sau khi pull source; giữ các biến port/model như lần trước:
docker compose up -d --build
# Xóa container/network, GIỮ account và config:
docker compose down
```

Account/config/log lưu trong named volume của Compose, tại
`/var/lib/llm-proxy/state` trong container; không mất khi recreate/update. Volume
mới được chuẩn bị cho UID/GID `10001:10001`, thư mục state `0700`, file private
`0600`. Không dùng `volume.nocopy`; volume/bind mount tự tạo sai ownership không
được tự sửa bằng root entrypoint. **`docker compose down -v` xóa cả volume, account
và credential; phải đăng nhập lại.** Không đổi Compose project name nếu muốn dùng
lại volume cũ.

Nếu cần cấu hình lane/alias, ghi config example vào volume bằng chính user runtime,
sau đó restart container (CLI model/port/address vẫn ghi đè TOML):

```sh
docker compose exec -T proxy sh -c 'umask 077; tee "$LLM_PROXY_STATE_DIR/config.toml" >/dev/null' < config.example.toml
docker compose restart proxy
```

Container bind `0.0.0.0` bên trong nhưng Compose chỉ publish loopback của host.
API **chưa có client authentication**: không đổi publish sang `0.0.0.0`, không dùng
host networking hoặc nối container không tin cậy vào cùng network. Các container
cùng network vẫn có thể gọi proxy và sử dụng quota. Docker stop dùng SIGINT để
hủy upstream và flush log; không cần thêm daemon trong container.

## Cài và build bằng Rust

Cần Rust stable mới nhất (tối thiểu 1.99), C compiler/linker và `pkg-config`.
`rust-toolchain.toml` chọn stable + rustfmt/Clippy; `Cargo.lock` giữ phiên bản crate
đã kiểm tra. Edition 2024 là edition ngôn ngữ, không phải năm hiện tại.

```sh
cargo build --release --locked
# Hoặc cài binary vào ~/.local/bin (đường dẫn dùng bởi user service):
cargo install --path . --locked --root "$HOME/.local"
```

Các ví dụ bên dưới dùng `llm-proxy`. Nếu chưa install, thay bằng
`./target/release/llm-proxy`, hoặc dùng `cargo run --locked -- ...`.

## Đăng nhập và chạy

```sh
llm-proxy accounts login --name personal
llm-proxy accounts login --name work
llm-proxy accounts list
llm-proxy serve --default-model gpt-6.1-sol
```

Mỗi lần `login` in URL và thử mở trình duyệt; callback tại `127.0.0.1:1455`.
Dùng cửa sổ riêng tư hoặc chọn đúng account trong trình duyệt khi thêm account
khác. Không cần đăng xuất Codex CLI. Đăng nhập lại cùng user/workspace cập nhật
account cũ; hai người cùng workspace vẫn được lưu thành hai account.

Nếu callback không dùng được:

```sh
llm-proxy accounts login --name work --login-method device-code
```

Device login có thể cần được bật trong ChatGPT security/workspace settings.
Proxy sở hữu credential riêng và không đọc/ghi `~/.codex/auth.json`.

Mặc định server nghe tại `http://127.0.0.1:18765`. `--listen-address`/`listen_address`
cho phép bind IP khác, chủ yếu dùng `0.0.0.0` bên trong container; không expose API
ra mạng công cộng. Listener không xác thực client; `ANTHROPIC_AUTH_TOKEN` chỉ đáp ứng cấu hình client,
không thay thế token ChatGPT. Chương trình local truy cập được listener có thể
sử dụng quota của các account đã đăng nhập.

## Chạy nền bằng systemd user (Linux/WSL)

Nếu máy có `systemd --user`, cài binary vào `~/.local/bin` như trên rồi cài unit:

```sh
install -D -m 644 packaging/systemd/llm-proxy.service ~/.config/systemd/user/llm-proxy.service
systemctl --user daemon-reload
systemctl --user enable --now llm-proxy
```

Dừng proxy đang chạy trong terminal trước khi start service để giải phóng cổng.
Service chạy độc lập với terminal, tự bật khi user manager khởi động và tự restart
khi process lỗi. Unit mẫu dùng `gpt-6.1-sol`; sửa model/flag bằng override qua
`systemctl --user edit llm-proxy`, rồi `systemctl --user restart llm-proxy`.
Không chạy hai instance foreground/service trên cùng cổng.

```sh
systemctl --user status llm-proxy --no-pager
systemctl --user stop llm-proxy
systemctl --user start llm-proxy
systemctl --user restart llm-proxy
journalctl --user -u llm-proxy -f
# Bỏ tự khởi động và dừng:
systemctl --user disable --now llm-proxy
```

Unit dùng SIGINT để proxy hủy upstream/flush log đúng như Ctrl+C; không cần tự
fork/daemonize trong Rust. JSONL riêng của proxy vẫn xoay vòng trong state/logs.
Đây là user service: chịu lifecycle của user manager; WSL shutdown/tắt máy sẽ
kết thúc nó. Không tự bật system-wide linger cho các service khác của user.

## Kết nối Claude Code

Chạy proxy ở terminal thứ nhất. Ở terminal thứ hai, trong project muốn code:

```sh
ANTHROPIC_BASE_URL=http://127.0.0.1:18765 \
ANTHROPIC_AUTH_TOKEN=local \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK=1 \
claude --model sonnet
```

Để các phiên mới chỉ cần gõ `claude`, **gộp** bốn biến vào `env` của
`~/.claude/settings.json` (giữ nguyên các key/biến khác nếu file đã có):

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:18765",
    "ANTHROPIC_AUTH_TOKEN": "local",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK": "1"
  }
}
```

Mở lại Claude Code để áp dụng. Với service đang chạy, chỉ cần vào project rồi
`claude`; không cần export hay chép lại chuỗi biến mỗi lần. Cấu hình này đặt proxy
làm mặc định cho mọi project của user; cấu hình riêng của project vẫn có thể ghi đè.
Nếu đã stop service, start lại trước khi dùng Claude Code qua proxy.

`sonnet`, `haiku`, `opus`, `fable` và các tên `claude-<family>-...` chưa được map
riêng đều đi tới model mặc định. Vì vậy cả request chính và request nền của Claude
Code đều dùng Codex; không cần tài khoản Anthropic cho luồng inference này.

Chọn model mà account của bạn có quyền dùng. Tên xuất hiện trong `/v1/models`
không bảo đảm account được cấp quyền. Hậu tố `[1m]` chỉ được chuẩn hóa ở đầu vào;
proxy không làm tăng context window của model.

## Auto mode và classifier request

Claude Code có thể gửi request phụ để kiểm tra một hành động trước khi thực thi.
Để dùng classifier phía client qua proxy này, mở **phiên mới** với cấu hình gateway
như phần trên và thêm biến sau vào **process Claude Code**, không phải proxy/container:

```sh
CLAUDE_CODE_AUTO_MODE_SERVER=0 claude --model sonnet --permission-mode auto
```

Đây là [cấu hình được Claude Code hướng dẫn](https://code.claude.com/docs/en/auto-mode-classifier-billing.md#make-the-session-eligible)
để dùng các request classifier riêng thay cho server-side review; **không bỏ kiểm
tra quyền**. Request phụ dùng quota/token và tăng latency. Proxy không hỗ trợ
server-side `safeguards`/`safeguard_results`, không giả verdict allow; lỗi vẫn về
Claude Code để hỏi/chặn theo chính sách client. Không tự chỉnh settings user.

Có thể cấu hình route riêng trong state `config.toml` rồi restart proxy:

```toml
[auto_review]
provider = "codex"
model = "gpt-6.1-sol"
```

Cả hai key đều optional: thiếu provider kế thừa `default_provider`, thiếu model
kế thừa model/alias client yêu cầu. Không tự chọn model rẻ/yếu hơn hoặc hạ effort.
Override phải trỏ provider đã đăng ký và model hợp lệ trong registry; sai cấu hình
trả lỗi, không âm thầm dùng model khác. Main conversation và token counting không
bị đổi route. Alias/model gốc vẫn được giữ trong response cho client.

Nhận diện dùng exact system-prefix của client, request non-streaming và không
khai báo tools; không scan user text hay suy diễn từ keyword. Đây là route hint,
không phải xác thực. Review giữ transcript/schema/effort nhưng không dùng session
cache key hoặc replay encrypted reasoning từ conversation. Proxy chỉ chuyển tiếp
kết quả model, không tự thực thi hành động hoặc parse thành quyền cho phép.

**Strategy pattern / SOLID:** `RequestRoutingPolicy` chọn provider/model;
`ProtocolAdapter` nhận diện loại request; `Provider` thực hiện generation. Các
thành phần được inject qua `AppState`, không cần class inheritance hoặc DI framework.
Sau này JEV có API/auth implementation thật thì thêm `Provider`, đăng ký model và
đổi `[auto_review]`; không cần thêm nhánh JEV vào HTTP handler. Hiện **chưa có JEV
provider**, chưa chứng minh model Codex review tương đương safety classifier Claude.
Anthropic không hỗ trợ chính thức routing Claude Code sang non-Claude models.

Có smoke client cô lập, không dùng account/quota thật:

```sh
python3 -I scripts/check-auto-review.py
```

Script dùng Claude Code đã cài, temp HOME/state và localhost mock. Nó cố ý trả lỗi
classifier để kiểm tra routing và fail-closed, **không tạo verdict allow**. Exit 2
là BLOCKED/inconclusive nếu client không phát classifier hoặc chưa chứng minh được
hành động bị chặn. PASS cũng không chứng minh safety của model Codex thật.

## Cấu hình

Mặc định state/config nằm tại `$XDG_STATE_HOME/llm-proxy`, hoặc
`~/.local/state/llm-proxy`. Dùng `--state-dir /path` (hoặc `LLM_PROXY_STATE_DIR`) để
chọn thư mục khác; các lệnh login/list/remove/serve phải dùng cùng thư mục.

Copy nội dung [config.example.toml](config.example.toml) thành `config.toml` trong
thư mục state, rồi chỉnh model. Ví dụ:

```toml
default_provider = "codex"
default_model = "gpt-6.1-sol"
listen_address = "127.0.0.1"
listen_port = 18765

[aliases]
haiku = "gpt-6-luna"
sonnet = "gpt-6.1-sol"
opus = "gpt-6-astra"

[models."gpt-6.1-sol"]
responses_lite = false
```

`serve --default-model`, `serve --listen-address` và `serve --listen-port` ghi đè giá trị tương ứng trong file. Cấu hình
model được đọc lúc server khởi động; thay model config cần chạy lại server.
Account được đọc lại cho request kế tiếp nên thêm/xóa/login lại không cần restart.

Responses Lite mặc định cho các họ gpt-5.6/gpt-6 và chỉ dùng tool tuần tự.
`responses_lite = false` chọn full lane cho model/account hỗ trợ parallel tools.
Giới hạn parallel của client vẫn được giữ. Hosted search tự động dùng full lane;
search bắt buộc riêng dùng `/alpha/search`. Model Luna được chuyển sang Sol tương
ứng khi cần hosted search tự động; có thể đặt `search_model` trong model config.

## Account và quota

```sh
llm-proxy accounts list
llm-proxy accounts list --output-format json
llm-proxy accounts remove <account-id>
```

- Account theo thứ tự được thêm; giữ account active cho đến khi hết quota hoặc
  credential bị thu hồi. Dấu `*` trong danh sách chỉ account đang active.
- Lưu trạng thái quota từ error/event/headers upstream; không poll hay tiêu quota
  chỉ để làm mới màn hình. JSON có snapshot sử dụng gần nhất nếu upstream gửi.
- Hard quota mới gây failover. Generic 429, lỗi mạng, model không có quyền dùng
  hoặc lỗi server không được đánh dấu nhầm thành hết quota.
- Account đã hết quota được bỏ qua đến reset. Nếu upstream không cho reset time,
  thử lại sau cooldown 60 giây. Account reset không giành lại account đang chạy tốt.
- Khi tất cả account hết, trả lỗi rate limit và thời gian thử lại nếu biết. Nếu
  đã mở SSE, lỗi được phát bằng SSE `error`; HTTP status lúc đó không thể đổi.
- Sau khi gửi `message_start`/content, không tự replay request. Không replay lỗi
  mạng không rõ trạng thái xử lý. Điều này tránh lặp text hoặc tool call.
- Xóa/login lại áp dụng cho lượt tiếp theo; request đã bắt đầu được hoàn tất với
  credential đang giữ. Việc đổi generation trước dispatch được chọn lại an toàn.

Credential được lưu plaintext trong thư mục riêng `0700`, file `0600`, dùng khóa
liên tiến trình và atomic replace. Không đặt state trong thư mục chia sẻ hoặc
commit state vào Git. Token cũ không ghi đè lần login mới; xóa account không bị
một refresh muộn hồi sinh.

## Khả năng tương thích

Hỗ trợ text; function tool/schema/call/result; ảnh base64 JPEG/PNG/GIF/WebP cả
trong tool result; reasoning effort/summary; JSON schema output; streaming và
buffered Messages; WebSearch và domain filter; compact bằng tóm tắt portable.
Cùng một chuỗi domain event phục vụ cả streaming và non-streaming.

Encrypted reasoning chỉ replay khi HMAC hợp lệ và đúng account/generation/model.
Khi chuyển account, giữ transcript text/tool/result/summary và bỏ native state
không tương thích. V1 không dùng `previous_response_id` hay server compaction.

Đây là tương thích thực dụng cho Claude Code, không phải toàn bộ Anthropic API.
Ảnh URL, audio, tài liệu và hosted tool chưa có mapping trả lỗi rõ ràng. Một số
tham số riêng Anthropic, như cache hints, `max_tokens` và giới hạn số lần hosted
search, không có phép ánh xạ tương đương trên backend subscription. Token count
là ước lượng local, không phải số billing. Nội dung vượt context window vẫn có
thể bị backend từ chối.

Backend Codex subscription có thể thay đổi giao thức. Native mapping tham khảo
[raine/claude-code-proxy](https://github.com/raine/claude-code-proxy); revision của
phần schema port ghi ở `src/providers/codex/schema.rs` và fixture README.
Copyright/giấy phép MIT được giữ trong [LICENSE](LICENSE), cũng được ship trong image.

## HTTP API

| Route | Chức năng |
| --- | --- |
| `POST /v1/messages` | Anthropic Messages, `stream: true/false` |
| `POST /v1/messages/count_tokens` | Ước lượng local, không gọi upstream |
| `GET /v1/models` | Model/alias được đăng ký, không phải kiểm tra entitlement |
| `GET /health` | Trạng thái server/readiness, không trả credential |

```sh
curl http://127.0.0.1:18765/health
curl http://127.0.0.1:18765/v1/models
curl -N http://127.0.0.1:18765/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"sonnet","stream":true,"messages":[{"role":"user","content":"Say PROXY_OK"}]}'
```

Mặc định tối đa 32 request generation/token-count đang xử lý; lấy permit trước
khi đọc JSON, body-read timeout 30 giây và trả 503 sớm nếu quá tải. Body request
32 MiB, frame SSE 8 MiB, output/retained data 16 MiB (kể cả initial tool metadata).
Buffered body và blocking tokenizer giữ permit đến khi công việc thật kết thúc,
kể cả client disconnect. Đây không phải RAM cap: request/JSON/output copies có
thể cần hàng trăm MiB hoặc hơn khi nhiều request cùng gần giới hạn.

HTTP connect timeout 15 giây, upstream response-header/body-idle timeout 300 giây;
SSE heartbeat 15 giây. `Ctrl+C` hủy inference; OAuth refresh đã gửi được hoàn tất
và persist độc lập với client rồi được drain khi shutdown, để giảm nguy cơ mất
rotated token. SIGKILL, lỗi đĩa và transport ambiguity vẫn có thể buộc login lại.
Packaged Docker/systemd stop grace 45 giây chứa OAuth timeout 30 giây.

## Log để debug

Khi chạy `serve`, log JSONL được ghi vào `<state-dir>/logs/proxy.jsonl` và stderr.
Giữ hai bản cũ `proxy.1.jsonl`, `proxy.2.jsonl`; mỗi file tối đa khoảng 10 MiB
(có thể vượt một record để không cắt JSON). Thư mục `0700`, file `0600`.

Mỗi request generation có `x-request-id` ở response và `request_id` trong log.
Các event hữu ích: `request_route_selected` (DEBUG: requested_model gốc, routed_model, provider, request_purpose),
`upstream_attempt` (provider/model thực tế/account, routed_model, request_purpose, reasoning_effort, lane), `upstream_failure`
(loại lỗi, error_code, đã phát nội dung chưa, có retry không), `request_completed` (thời gian
và token usage), `request_failed`, `request_cancelled`, `proxy_started/stopped`.
Refresh token chỉ ghi mốc hoàn tất/thất bại/thu hồi credential cùng account ID và phase/status;
không ghi giá trị token hay response body.
`upstream_status` giữ status native trước khi map sang lỗi client;
`upstream_parameter` chỉ giữ tên trường trong allowlist, không giữ giá trị.
`error_code` là mã tĩnh, ví dụ `invalid_message_role`, `unsupported_content_type`,
`unsupported_hosted_tool`, `invalid_reasoning_effort`, `unknown_model`.
Không log error.message thô, prompt, tool arguments/results, token đăng nhập, OAuth code, reasoning
mã hóa, session ID, heartbeat hay từng chunk. Health/model/list không tạo access log.

```sh
tail -f ~/.local/state/llm-proxy/logs/proxy.jsonl
# Lọc một request nếu máy có jq:
jq 'select(.fields.request_id == "REQUEST_ID")' ~/.local/state/llm-proxy/logs/proxy*.jsonl
```

`log_level = "info"` trong `config.toml`; nhận `off/error/warn/info/debug/trace`.
Chỉ nhận event của proxy; không bật log HTTP thô từ dependency bằng `RUST_LOG`.
Hàng đợi nền có giới hạn 1024 record; khi đầy sẽ bỏ log để request không bị chặn.
Không dùng log này làm sổ audit.

## Phát triển và context

Đọc [AGENTS.md](AGENTS.md), [kiến trúc](docs/architecture.md),
[tiến độ/context](docs/implementation-progress.md),
[kế hoạch v1](docs/superpowers/plans/2026-11-12-llm-proxy-v1.md) và
[quy ước đặt tên](docs/conventions.md).

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Các test bình thường chạy trong process, dùng fake backend Strategy và Axum
`oneshot`, không cần mạng hay credential. Sáu test `http_contract` dùng HTTP mock
localhost thật, được ignore mặc định để chạy được trong sandbox cấm socket:

```sh
cargo test --locked --test http_contract -- --include-ignored
# Toàn bộ kiểm tra, gồm mock HTTP và release build:
./scripts/check-local.sh
```

CI chạy cả các test localhost. Khi thay đổi Docker packaging, kiểm tra thêm bằng
image local và smoke cô lập (cần Python 3 cho script kiểm tra, không cần cho user chạy proxy):

```sh
docker compose config --quiet
docker compose build
python3 -I scripts/check-docker.py
```

Script tự tạo project/volume riêng và host-port ngẫu nhiên, không đọc credential
host, không đăng nhập hoặc gọi inference. Kiểm tra non-root, private permissions,
HTTP/no-account errors, state persistence, hot removal bằng account giả và SIGINT
flush; chỉ xóa tài nguyên tạm do chính script tạo.

Đo tài nguyên hữu hạn trên Linux bằng release process riêng và mock upstream:

```sh
cargo build --release --locked
python3 -I scripts/check-resources.py --seconds 180 --workers 8 --output /tmp/llm-proxy-resources.json
```

Script warm-up tokenizer/allocator, trộn buffered/streaming/cancel, kiểm tra đủ
32 slots, một input gần 32 MiB và 30 giây idle. Báo RSS/high-water, CPU theo phần
trăm một core, FD/thread và active upstream sau tải. Phân biệt allocator giữ lại
high-water với tăng RAM liên tục; kết quả vài phút không chứng minh không leak
trong nhiều ngày. Không gọi provider hoặc account thật.

Để kiểm tra với provider thật, đăng nhập account
rồi dùng Claude Code trong một thư mục thử nghiệm: yêu cầu đọc/sửa một file, chạy
test, gọi subagent, đọc một ảnh, WebSearch và `/compact`, sau đó tiếp tục câu hỏi.
Failover được kiểm tra bằng mock quota; không cần cố tiêu hết quota account thật.

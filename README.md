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

## Cài và build

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

Mặc định server nghe tại `http://127.0.0.1:18765`. Chỉ hỗ trợ loopback trong v1.
Listener không xác thực client; `ANTHROPIC_AUTH_TOKEN` chỉ đáp ứng cấu hình client,
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

## Cấu hình

Mặc định state/config nằm tại `$XDG_STATE_HOME/llm-proxy`, hoặc
`~/.local/state/llm-proxy`. Dùng `--state-dir /path` (hoặc `LLM_PROXY_STATE_DIR`) để
chọn thư mục khác; các lệnh login/list/remove/serve phải dùng cùng thư mục.

Copy nội dung [config.example.toml](config.example.toml) thành `config.toml` trong
thư mục state, rồi chỉnh model. Ví dụ:

```toml
default_provider = "codex"
default_model = "gpt-6.1-sol"
listen_port = 18765

[aliases]
haiku = "gpt-6-luna"
sonnet = "gpt-6.1-sol"
opus = "gpt-6-astra"

[models."gpt-6.1-sol"]
responses_lite = false
```

`serve --default-model` và `serve --listen-port` ghi đè giá trị tương ứng trong file. Cấu hình
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

Backend Codex subscription có thể thay đổi giao thức. Xem
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) về source tham khảo và giấy phép.

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

Mặc định tối đa 32 request generation đồng thời; body request 32 MiB, frame SSE
8 MiB, nội dung phản hồi 16 MiB. HTTP connect timeout 15 giây, response-header và
body-idle timeout 300 giây. SSE heartbeat 15 giây. `Ctrl+C` hủy upstream và đóng
server có kiểm soát.

## Log để debug

Khi chạy `serve`, log JSONL được ghi vào `<state-dir>/logs/proxy.jsonl` và stderr.
Giữ hai bản cũ `proxy.1.jsonl`, `proxy.2.jsonl`; mỗi file tối đa khoảng 10 MiB
(có thể vượt một record để không cắt JSON). Thư mục `0700`, file `0600`.

Mỗi request generation có `x-request-id` ở response và `request_id` trong log.
Các event hữu ích: `upstream_attempt` (provider/model thực tế/account, requested_model, reasoning_effort, lane), `upstream_failure`
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

CI chạy cả các test localhost. Để kiểm tra với provider thật, đăng nhập account
rồi dùng Claude Code trong một thư mục thử nghiệm: yêu cầu đọc/sửa một file, chạy
test, gọi subagent, đọc một ảnh, WebSearch và `/compact`, sau đó tiếp tục câu hỏi.
Failover được kiểm tra bằng mock quota; không cần cố tiêu hết quota account thật.

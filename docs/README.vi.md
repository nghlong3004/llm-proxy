# llm-proxy

LLM gateway chạy local cho coding agent, hỗ trợ routing theo model, provider native
và chuyển account trong những trường hợp được xác nhận an toàn.

[English](../README.md) · Tiếng Việt

## Tổng quan

Kết nối coding agent tương thích tới các provider qua một endpoint local dùng giao
thức Anthropic Messages-compatible. Cấu hình model đi đâu, đăng nhập account provider
và tiếp tục workflow của agent đang dùng.

Gateway lo chuyển đổi giao thức, routing và chọn account. **Coding agent vẫn thực thi
tools, truy cập filesystem, chạy shell và quản lý agent loop.** Claude Code là ví dụ
client được hướng dẫn, không phải tên giao thức và không có nghĩa mọi agent đều được hỗ trợ.

## Tính năng

- Routing model/alias tới provider bằng TOML.
- Adapter native cho Codex và Antigravity (`agy`), không cần proxy Node chạy kèm.
- Nhiều account theo từng provider; thêm/xóa account không cần restart gateway.
- Failover bảo thủ: chỉ chọn lại account khi xác nhận hết quota, credential bị từ chối
  rõ ràng hoặc account thay đổi trước dispatch. Không đổi provider/model để né lỗi,
  không replay sau khi đã có output hay dispatch chưa chắc chắn.
- Streaming và buffered response dùng chung một pipeline event.
- Status offline; live usage và banked quota reset chủ động ở provider có hỗ trợ.

## Tương thích

### API cho client

| Endpoint | Chức năng |
| --- | --- |
| `POST /v1/messages` | Sinh response theo giao thức Anthropic Messages-compatible |
| `POST /v1/messages/count_tokens` | Ước lượng token local, không phải số chính thức của provider |
| `GET /v1/models` | Catalog model/alias trong config, không kiểm tra entitlement |
| `GET /health` | Trạng thái tiến trình và account readiness |

Client cần cho phép đổi base URL và tương thích với subset Messages mà proxy triển
khai. Chưa có inbound OpenAI Chat Completions hay hỗ trợ chung cho mọi coding agent.

### Provider

| Provider | Đăng nhập | Giới hạn chính |
| --- | --- | --- |
| Codex (`codex`) | ChatGPT browser OAuth hoặc device code | Model và account API phụ thuộc quyền tài khoản/backend. |
| Antigravity (`agy`) | Google browser OAuth | Claude: text/ảnh/function tools. Gemini: text/ảnh. Chưa hỗ trợ effort explicit, structured output, hosted search, compaction, forced tool choice, live usage hoặc banked reset. |

AGY trả lỗi cho điều khiển không hỗ trợ, không âm thầm bỏ qua. Nếu client tự gửi
reasoning effort, request đó không dùng được nguyên trạng với AGY. Callback browser
cũng chưa được publish trong cấu hình Docker đi kèm.
Xem [giới hạn và nguồn tham khảo AGY](antigravity.md).

## Cài đặt

Build từ source với **Rust 1.100.0**. Các dependency Ubuntu/WSL, cài native, systemd
và Docker được mô tả trong [hướng dẫn cài đặt](installation.md).

Sau khi có đủ prerequisite, cài binary để chạy tương tác:

```sh
git clone https://github.com/nghlong3004/llm-proxy.git
cd llm-proxy
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

## Sử dụng nhanh

Đăng nhập Codex rồi chạy server local:

```sh
llm-proxy accounts add codex --name personal --login-method device-code
llm-proxy serve --default-model gpt-6.1-sol
```

Làm theo hướng dẫn đăng nhập được in ra. Model trên chỉ là ví dụ; chọn model mà account
có quyền dùng. Có thể cần bật device login trong cài đặt ChatGPT.

Mở terminal khác để kiểm tra:

```sh
curl --fail http://127.0.0.1:18765/health
llm-proxy accounts status
```

Xem trường `ready`, không chỉ HTTP 200. Readiness chưa chứng minh provider/model hoạt
động live. Sau đó trỏ client tương thích về `http://127.0.0.1:18765` theo
[ví dụ cấu hình client](configuration.md#client-configuration-example).

Đăng nhập AGY native bằng `llm-proxy accounts add agy --name google-personal`, rồi cấu
hình route AGY trước khi serve. Mặc định không cần nhập cặp OAuth client env: proxy
lấy cấu hình công khai từ reference đã ghim, kiểm tra hash và cache private. Cần đọc
[rủi ro và nguồn cấu hình](antigravity.md#provenance) trước khi sử dụng.

## Cấu hình

File nằm tại `<state-dir>/config.toml`; native mặc định là `$XDG_STATE_HOME/llm-proxy`
hoặc `~/.local/state/llm-proxy`. Login, lệnh account và server phải dùng cùng state directory.

Ví dụ routing nhiều provider:

```toml
default_provider = "codex"
default_model = "gpt-6.1-sol"

[aliases]
sonnet = "claude-sonnet-4-6"

[models."claude-sonnet-4-6"]
provider = "agy"
```

`sonnet` đi tới model AGY đã đăng ký. Alias Claude-family không được map riêng dùng
`default_model`; đăng ký model native tường minh được ưu tiên. Proxy không tự thay
provider/model khi gặp lỗi. Model ví dụ không bảo đảm availability hay entitlement.

Xem [configuration và account](configuration.md), cùng
[config.example.toml](../config.example.toml) để cấu hình state, client, classifier,
usage và khôi phục quota reset. Đổi config cần restart server; thêm/xóa account thì không.

## Cập nhật

[Hướng dẫn update](updating.md) có các bước native/systemd/Docker, backup private state,
restart, kiểm tra và rollback. Dùng lại đúng state directory hoặc Docker volume để giữ
account; chỉ cập nhật binary không yêu cầu đăng nhập lại.

## Tài liệu

Các guide kỹ thuật chi tiết hiện dùng tiếng Anh:

- [Installation](installation.md) — native, systemd và Docker.
- [Configuration and accounts](configuration.md) — routing, client và quota workflows.
- [Updating](updating.md) — update, backup và rollback.
- [Antigravity](antigravity.md) — OAuth, tính năng, provenance và caveat.
- [Architecture](architecture.md) — ranh giới module và lifecycle/resource ownership.
- [Development conventions](conventions.md) — commands, coding style và attribution.

Khi cần trợ giúp, tạo [issue](https://github.com/nghlong3004/llm-proxy/issues) kèm
version/revision, cách chạy và lỗi đã che dữ liệu nhạy cảm. Không đính kèm credentials,
OAuth code, prompt hoặc tool payload.

## Bảo mật và giới hạn

- API **chưa xác thực client**. Giữ listener native và host port Docker trên loopback;
  token placeholder trong client không bảo vệ được API.
- Trên Unix, state directory có quyền `0700`, file `0600`. Credential account **chưa
  mã hóa trên đĩa**; cần bảo vệ state và backup.
- Upstream có API nội bộ/private; behavior, OAuth client và entitlement có thể thay
  đổi. Tự kiểm tra điều khoản/quyền sử dụng. Dự án không được provider bảo trợ.
- Kiểm thử tự động dùng mock/local, không phải xác minh live có xác thực hay bảo đảm
  model an toàn.
- Classifier lỗi thì vẫn trả lỗi; không tự tạo kết quả approve. Cần route review tới
  provider hỗ trợ đúng request.

## Phát triển và đóng góp

Dùng Rust toolchain đã ghim và giữ `Cargo.lock`. Trước khi gửi thay đổi:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
```

Một số ignored test cần socket localhost; dùng state/account giả, không dùng account
thật. Xem [conventions](conventions.md) cho Docker và smoke checks bổ sung. Giữ thay đổi
tập trung và thêm regression test; không commit credentials hoặc request thật.

## License

[MIT](../LICENSE), gồm attribution cho phần tham khảo/ported. License phần mềm không
thay thế quyền truy cập dịch vụ hay điều khoản của provider.

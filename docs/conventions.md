# Quy ước đặt tên

Thư mục đặt theo trách nhiệm cụ thể, dùng danh từ tiếng Anh đầy đủ, `snake_case`;
không dùng `core`, `common`, `utils` để gom code không liên quan.

| Vị trí | Trách nhiệm |
| --- | --- |
| `domain` | Request/content/event/lỗi dùng chung, không chứa wire schema |
| `protocols` | Giao thức mà coding agent gửi vào; Adapter |
| `providers` | Dịch vụ LLM nhận request; Strategy và registry |
| `providers/codex` | Implementation riêng của Codex subscription |
| `accounts` | Account registry, private storage, chính sách chọn account |
| `authentication` | OAuth và refresh, không liên quan incoming auth header |
| `server` | HTTP transport, routing, cancellation |
| `cli` | Khai báo lệnh và khởi tạo các thành phần |
| `logging` | Ghi metadata và xoay log |

Một account có `provider` và `name`. `default_provider`, `default_model`,
`listen_port`, `log_level` là key TOML. Các trường cũ `backend`, `label`,
`default_backend`, `port` chỉ được nhận khi đọc state/config đã có;
serialize và tài liệu luôn dùng tên mới. `listen_address` là IP (mặc định
`127.0.0.1`); CLI tương ứng `--listen-address`, Docker override thành `0.0.0.0`
trong container nhưng publish host vẫn loopback.

CLI theo dạng `resource action`; flag dài dùng `kebab-case`, tương ứng key
`snake_case` khi có cấu hình tương đương. Giá trị hữu hạn dùng enum dễ đọc.

```text
llm-proxy --state-dir PATH accounts login --name NAME --login-method browser
llm-proxy accounts login --name NAME --login-method device-code
llm-proxy accounts list --output-format json
llm-proxy accounts remove ACCOUNT_ID
llm-proxy serve --default-model MODEL_ID --listen-address IP_ADDRESS --listen-port PORT
```

`--state-dir` là global, env `LLM_PROXY_STATE_DIR`; không gọi là data-dir vì
thư mục chứa cả config, account state, credential và log. Chưa phát hành nên
không duy trì alias cho flag CLI cũ. Endpoint/field của API upstream giữ nguyên
wire spelling; đổi tên module Rust không được đổi URL hay schema của provider.

## Quan hệ với source tham khảo

`domain` (trước đây là `core.rs`) là hợp đồng do project này thiết kế để tách
agent protocol khỏi provider. Đây không phải một “core” chép từ hai repository.

- `raine/claude-code-proxy`: tham khảo hành vi OAuth, native headers,
  Responses/lane, mapping tool/ảnh/reasoning/search và event streaming.
- `kapybara-org/zerosub`: tham khảo account identity, quota và ràng buộc reasoning
  khi đổi account. Không chép source ZeroSub.

Không bê agent runtime vào proxy. Claude Code thực thi tool và agent loop;
proxy chuyển giao thức và chọn account/provider. Revision nguồn port được ghi
ở `src/providers/codex/schema.rs` và `tests/fixtures/codex/README.md`;
MIT attribution ở `LICENSE` (cũng được copy vào runtime image).

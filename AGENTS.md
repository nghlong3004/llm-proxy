# Hướng dẫn làm việc với llm-proxy

## Đọc trước khi sửa code

1. `docs/implementation-progress.md`: trạng thái hiện tại, kiểm tra đã chạy,
   quyết định và công việc tiếp theo. Đây là context tiếp nối công việc.
2. `docs/architecture.md`: luồng dữ liệu, trách nhiệm từng thư mục và invariants.
3. `docs/superpowers/plans/2026-11-12-llm-proxy-v1.md`: kế hoạch v1 đã thống nhất.

Sau mỗi mốc triển khai hoặc thay đổi quyết định, cập nhật file tiến độ ngay.
Không đánh dấu hoàn tất nếu mới viết code mà chưa chạy kiểm tra tương ứng.
Khi đổi cấu trúc thư mục/giao diện, sửa tài liệu kiến trúc và hướng dẫn dùng.

## Mục tiêu đã chốt

- Rust stable mới nhất, edition 2024; dependency stable mới nhất, commit Cargo.lock.
- Proxy độc lập, chạy localhost, quản lý qua CLI, backend Codex subscription.
- Nhiều account: giữ account hiện tại đến khi hết quota rồi dùng account kế tiếp.
- Thêm/xóa account trong khi server chạy, áp dụng cho request kế tiếp.
- Mốc tương thích Claude Code tham khảo raine/claude-code-proxy: tool, ảnh,
  reasoning, structured output, SSE, WebSearch và compact portable.
- Không thực thi tool trong proxy; Claude Code sở hữu file, terminal và agent loop.

## Bố cục và cách viết Rust

- `main.rs`: entrypoint; `cli/`: parse lệnh và wiring, không chứa nghiệp vụ.
- `domain/`: hợp đồng request/event/lỗi trung lập và accumulator non-streaming.
- `protocols/`: Adapter giao thức agent; Anthropic schema/SSE/error chỉ ở đây.
- `providers/`: Strategy `Provider`, registry và implementation `codex/`.
- `accounts/`: định danh, lưu trữ, chọn account; không biết schema Anthropic.
- `authentication/`: OAuth, refresh token; không đọc/ghi credential Codex CLI.
- `providers/codex/`: translator, search, reasoning, SSE decoder và upstream client.
- `server/`: HTTP routes, điều phối request, cancellation và failover.
- `logging/`: metadata JSONL, hàng đợi nền có giới hạn và xoay file.
- `config.rs`: TOML, model registry/alias và endpoint.
- `tests/`: kiểm thử theo hành vi, dùng account/upstream giả, không tiêu quota thật.

Ưu tiên enum và Result có ý nghĩa; tránh unwrap/expect ngoài test. Hàm nhỏ theo
trách nhiệm; chỉ thêm trait/abstraction khi có nhu cầu thật. Giữ CPU/file-lock I/O
ra khỏi Tokio worker bằng spawn_blocking; không giữ khóa registry qua network await.
Quy ước lệnh/thư mục: đọc `docs/conventions.md`.
Dùng hai skill rust-best-practices và rust-async-patterns khi viết/review Rust.

## Invariants bắt buộc

- Credential riêng tư: thư mục 0700, file 0600, atomic replace + khóa liên tiến trình.
- Login/refresh cũ không được ghi đè lần login mới hoặc hồi sinh account đã xóa.
- Phân biệt user và workspace: hai người cùng workspace là hai account riêng.
- Không chuyển account chỉ vì generic 429, lỗi model/quyền hoặc lỗi mạng.
- Không tự replay sau message/content event hoặc khi không chắc request đã xử lý.
- Reasoning mã hóa chỉ replay khi chứng minh đúng account, generation và model.
- Không log prompt, token, OAuth code hay reasoning mã hóa; không commit credential.
- Upstream body/request có giới hạn; disconnect phải giải phóng upstream và permit.
- Incoming ANTHROPIC_AUTH_TOKEN không phải credential Codex và không forward lên mạng.

## Kiểm tra trước khi báo hoàn tất

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
```

Nếu môi trường có mạng bị hạn chế, dùng Cargo cache đã tải và --offline. Ghi rõ
kiểm tra nào bị chặn, kiểm tra nào đã pass. Không giả là đã kiểm tra Claude Code
thật nếu mới chạy mock; không đọc credential hiện có để tự chạy phiên thật.

Không tự publish/push/deploy. Giữ MIT attribution và nguồn tham khảo khi port code.

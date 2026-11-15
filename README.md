# llm-proxy

Proxy localhost cho phép **Claude Code dùng Codex qua tài khoản ChatGPT subscription**.
Hỗ trợ nhiều account và chuyển account khi xác nhận hết quota. Claude Code vẫn
thực thi tools, sửa file và chạy terminal.

```mermaid
flowchart LR
    Agent["Claude Code"] <-->|Anthropic Messages| Proxy["llm-proxy"]
    Proxy <-->|Codex Responses| Provider["Codex"]
    Provider --> Model["LLM"]
```

## 1. Tải source

Cần Git; trên Ubuntu/WSL có thể cài bằng `sudo apt-get install -y git`.

```sh
git clone https://github.com/nghlong3004/llm-proxy.git
cd llm-proxy
```

Chọn **một** cách chạy dưới đây.

## 2A. Cài native trên Ubuntu / WSL

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config cmake curl ca-certificates
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.100.0
. "$HOME/.cargo/env"
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

Binary được cài tại `~/.local/bin/llm-proxy`. Thêm dòng PATH trên vào `~/.bashrc`
hoặc `~/.zshrc` để dùng ở terminal mới.

Đăng nhập và chạy nền:

```sh
llm-proxy accounts add codex --name personal --login-method device-code
install -D -m 644 packaging/systemd/llm-proxy.service ~/.config/systemd/user/llm-proxy.service
systemctl --user daemon-reload
systemctl --user enable --now llm-proxy
curl --fail http://127.0.0.1:18765/health
```

Mở URL được in ra và nhập device code. Thêm account bằng cách lặp lệnh login với
`--name` khác. Nếu không dùng systemd, chạy `llm-proxy serve --default-model gpt-6.1-sol`.

## 2B. Chạy Docker

Cần Docker Engine và Docker Compose; không cần Rust trên host. Dockerfile build
binary ở build stage; Compose chạy container và lưu state trong named volume.

```sh
docker compose up -d --build
docker compose exec proxy llm-proxy accounts add codex --name personal --login-method device-code
curl --fail http://127.0.0.1:18765/health
```

Nếu cổng đã được dùng, chạy `LLM_PROXY_PORT=18766 docker compose up -d --build`
và dùng cùng giá trị ở các lần `up` sau.

```sh
docker compose logs -f proxy
docker compose stop
docker compose start
docker compose down
```

`down` giữ account; **`down -v` xóa account và credential**. Cổng chỉ publish ra
localhost; không expose public vì API chưa xác thực client.

## 3. Cấu hình Claude Code

Nếu chưa cài:

```sh
curl -fsSL https://claude.ai/install.sh | bash
```

Gộp các key sau vào `env` của `~/.claude/settings.json`; giữ nguyên những key đã có:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:18765",
    "ANTHROPIC_AUTH_TOKEN": "local",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK": "1",
    "CLAUDE_CODE_AUTO_MODE_SERVER": "0"
  }
}
```

Nếu đổi host port, sửa `ANTHROPIC_BASE_URL` tương ứng. Mở terminal/phiên mới, vào
project cần làm và chạy:

```sh
claude
```

Chọn Auto mode bằng `Shift+Tab` khi cần. Biến `CLAUDE_CODE_AUTO_MODE_SERVER=0`
dùng classifier phía client, không bỏ kiểm tra quyền.

## 4. Account và config

Native:

```sh
llm-proxy accounts status                         # cache offline, tất cả provider có account
llm-proxy accounts status codex                   # lọc đúng provider
llm-proxy accounts status codex --refresh         # chủ động gọi provider để cập nhật
llm-proxy accounts status --output-format json    # dữ liệu status công khai, không credential
llm-proxy accounts reset-quota codex ACCOUNT_ID   # chỉ xem trước, chưa tiêu reset
llm-proxy accounts reset-quota codex ACCOUNT_ID --yes
llm-proxy accounts remove ACCOUNT_ID
```

`accounts status` mặc định chỉ đọc cache: không cần model/config hợp lệ, credential hay mạng.
Dấu `* active` thuộc từng provider. Usage ghi rõ nguồn và tuổi quan sát; cache có thể
đã cũ. Nếu `--refresh` lỗi, dữ liệu cache vẫn được in riêng với lỗi và lệnh trả mã
khác 0. `unknown` là chưa biết, `unsupported` là không hỗ trợ; cả hai **không phải 0**.
Cửa sổ quota, thời lượng và thời điểm reset do provider định nghĩa, không mặc định
provider nào cũng có quota 5 giờ/tuần.

Nếu anh dùng lượt reset bên Codex CLI, chạy `llm-proxy accounts status codex --refresh`
để proxy nhận quota đã phục hồi. Khi Codex xác nhận `allowed=true`, lệnh gỡ cờ
hết quota local cho đúng account/generation và hiện trạng thái mới ngay. Nó không
đổi account đang active, không gỡ lỗi cần đăng nhập lại và không tự tiêu lượt reset.
Dữ liệu thiếu, lỗi cập nhật hoặc `%` quota đơn thuần không đủ để gỡ cờ. Không có
polling nền; lệnh `status` không có `--refresh` vẫn chỉ đọc cache.

Phân biệt ba loại thông tin:

- **Scheduled reset**: thời điểm quota cửa sổ tự phục hồi, không phải thao tác mua thêm.
- **Prepaid credits**: số dư trả trước, trạng thái unlimited/spend-blocked nếu được cung cấp;
  không phải số lần reset quota.
- **BANKED resets**: quyền reset đã tích lũy, có số lượng và hạn dùng nếu provider cung cấp.
  `reset-quota --yes` yêu cầu tiêu một quyền này; không mua credits và không chỉ đợi lịch reset.
  Không có `--yes` thì chỉ xem trước. Khả năng đọc live/reset tùy provider; provider tương lai
  có thể chỉ hỗ trợ một phần, dữ liệu thiếu không được tự suy thành 0.

Nếu timeout sau khi gửi reset, kết quả có thể **chưa xác định** và reset có thể đã bị tiêu.
Giữ request ID được in ra; không tạo yêu cầu mới. Chỉ khi chủ động muốn đối soát/thử lại
cùng yêu cầu, dùng đúng ID đó:

```sh
llm-proxy accounts reset-quota codex ACCOUNT_ID --yes --retry REQUEST_ID
```

Proxy không tự thử lại mutation; thời gian provider lưu idempotency **không được bảo đảm**.
Lệnh in outcome chính xác, request ID và lỗi đối soát/cập nhật sau reset nếu có.

Tất cả thao tác account dùng nhóm `accounts`: `add <provider>`, `status [provider]`,
`reset-quota <provider> ACCOUNT_ID`, và `remove ACCOUNT_ID_OR_PREFIX`.
`remove` nhận UUID đầy đủ hoặc prefix từ **8 ký tự**, chỉ xóa khi khớp duy nhất;
khớp nhiều account thì phải nhập dài hơn. `reset-quota` vẫn cần UUID đầy đủ.

Tương thích lệnh cũ: `accounts login --name personal --login-method device-code`
vẫn đăng nhập Codex; `accounts list` và JSON của nó giữ định dạng cũ. Các lệnh
cấp đầu `login`, `status`, `reset-quota` vẫn hoạt động nhưng ẩn khỏi help chính.
`accounts add` bắt buộc có provider (hiện chỉ `codex`).

Docker: thêm `docker compose exec proxy` trước các lệnh trên.

Config native nằm tại `~/.local/state/llm-proxy/config.toml` (hoặc
`$XDG_STATE_HOME/llm-proxy/config.toml`); Docker tại `/var/lib/llm-proxy/state/config.toml`.
Dùng [config.example.toml](config.example.toml) khi cần đổi model hoặc alias.
Thay config cần restart proxy; thêm/xóa account không cần restart.

## 5. Cập nhật phiên bản mới

Trong thư mục source đã clone:

**Native:**

```sh
git pull --ff-only
cargo install --path . --locked --force --root "$HOME/.local"
install -D -m 644 packaging/systemd/llm-proxy.service ~/.config/systemd/user/llm-proxy.service
systemctl --user daemon-reload
systemctl --user restart llm-proxy
```

**Docker:**

```sh
git pull --ff-only
docker compose up -d --build
```

Account được giữ lại; không cần login lại chỉ vì cập nhật binary/image.

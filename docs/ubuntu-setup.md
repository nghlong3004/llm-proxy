# Cài trên máy Ubuntu mới

Áp dụng cho Ubuntu x64/ARM64 có systemd và quyền cài package. Claude Code và
proxy chạy trên cùng máy. Cần Internet để clone/build, đăng nhập và gọi upstream.
Các bước này cài một lần; sau đó vào project và chạy `claude`.

## 0. Source phải có trên GitHub

Remote của project là `git@github.com:nghlong3004/llm-proxy.git`. Gắn remote không
tự upload source. Commit/push source, Cargo.lock, packaging và scripts trước khi
clone từ máy khác. Credential/state nằm ngoài repo; máy mới phải login lại.

## 1. Cài công cụ build và Rust

```sh
sudo apt-get update
sudo apt-get install -y git curl ca-certificates build-essential pkg-config cmake python3

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
rustup update stable
```

Dùng rustup để có Rust stable mới, không phụ thuộc bản rustc/cargo cũ trong apt.
`rust-toolchain.toml` của repo chọn stable và components; manifest yêu cầu >=1.99.
Hướng dẫn chính thức: https://rust-lang.org/tools/install/

## 2. Clone và cài binary

```sh
git clone https://github.com/nghlong3004/llm-proxy.git
cd llm-proxy
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
llm-proxy --version
```

Nếu repo private, cấu hình quyền GitHub trên máy mới trước khi clone. Khi đã có
SSH key được GitHub chấp nhận, có thể dùng
`git clone git@github.com:nghlong3004/llm-proxy.git` thay cho HTTPS.

Thêm dòng sau vào `~/.bashrc` để các terminal Bash mới cũng nhận binary; với Zsh
thêm vào `~/.zshrc`. Không cần thêm lại nếu PATH đã có ~/.local/bin.

```sh
export PATH="$HOME/.local/bin:$PATH"
```

## 3. Cài Claude Code và đăng nhập account của proxy

Nếu chưa cài Claude Code:

```sh
curl -fsSL https://claude.ai/install.sh | bash
claude --version
```

Hướng dẫn chính thức: https://code.claude.com/docs/en/setup
Không cần cài Codex CLI. Đăng nhập ChatGPT bằng lệnh của proxy:

```sh
llm-proxy accounts login --name acc1
# Thêm account khác nếu cần:
llm-proxy accounts login --name acc2
llm-proxy accounts list
```

`acc1`, `acc2` là tên gợi nhớ, không phải loại account. Trên Ubuntu Server/SSH
không có browser local, dùng device code rồi mở URL được in trên máy có browser:

```sh
llm-proxy accounts login --name acc1 --login-method device-code
```

Device login có thể cần bật trong security/workspace settings của account.
Không copy credential vào Git. Account đăng nhập trên máy cũ không tự đồng bộ
sang máy mới khi clone repo.

## 4. Chạy nền

Trong thư mục repo vừa clone:

```sh
install -D -m 644 packaging/systemd/llm-proxy.service ~/.config/systemd/user/llm-proxy.service
systemctl --user daemon-reload
systemctl --user enable --now llm-proxy
systemctl --user status llm-proxy --no-pager
curl --fail http://127.0.0.1:18765/health
```

Health cần status=ok và ready=true sau khi có account usable. Đây chỉ là readiness,
không kiểm tra quyền sử dụng model. Unit mẫu dùng gpt-6.1-sol; chọn model account
có quyền dùng khi thay ExecStart bằng `systemctl --user edit llm-proxy`, rồi restart.
Nếu đang chạy `llm-proxy serve` trong terminal, dừng instance đó trước để nhường cổng.

Trên server cần tiếp tục chạy sau khi logout SSH hoặc tự chạy khi máy boot mà
chưa login, có thể bật linger riêng cho user chạy proxy:

```sh
sudo loginctl enable-linger "$USER"
```

Đây là tùy chọn thay đổi lifecycle user manager; không cần cho phiên desktop
thông thường. Container/minimal Ubuntu không có systemd user manager cần chạy
`llm-proxy serve --default-model gpt-6.1-sol` bằng supervisor của môi trường đó.

## 5. Lưu cấu hình Claude Code một lần

Đoạn sau merge bốn biến vào settings user, giữ nguyên key/env khác. Nếu file đã
có, lưu một backup private trước lần thay đổi đầu tiên. Không in nội dung settings.
Chạy bằng user sẽ dùng Claude Code, không chạy với sudo.

```sh
python3 - <<'PY'
import json
import os
import tempfile
from pathlib import Path

root = Path((os.environ.get("CLAUDE_CONFIG_DIR") or str(Path.home() / ".claude"))).expanduser()
root.mkdir(parents=True, exist_ok=True)
path = root / "settings.json"
if path.is_symlink():
    raise SystemExit("Settings là symlink; hãy sửa file đích trực tiếp")
original = path.read_bytes() if path.exists() else None
settings = json.loads(original.decode("utf-8-sig")) if original is not None else {}
if not isinstance(settings, dict) or not isinstance(settings.get("env", {}), dict):
    raise SystemExit("Settings và env phải là JSON object; chưa thay đổi file")
settings["env"] = {
    **settings.get("env", {}),
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:18765",
    "ANTHROPIC_AUTH_TOKEN": "local",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK": "1",
}
backup = root / "settings.json.before-llm-proxy"
if original is not None and not backup.exists():
    fd = os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(original)
fd, temporary = tempfile.mkstemp(prefix=".settings-llm-proxy-", dir=root)
try:
    with os.fdopen(fd, "w") as output:
        json.dump(settings, output, indent=2, ensure_ascii=False)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    if (path.read_bytes() if path.exists() else None) != original:
        raise RuntimeError("Settings thay đổi đồng thời; chưa ghi đè")
    os.replace(temporary, path)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
print("Đã cấu hình Claude Code dùng llm-proxy")
PY
```

Mở lại Claude Code nếu đang có phiên cũ. Cấu hình project có thể ghi đè user env;
kiểm tra `/status` nếu base URL không phải localhost:18765.

## Dùng hằng ngày

```sh
cd /duong/dan/project
claude
```

Proxy chạy nền, account routing tự xử lý. Các lệnh quản lý:

```sh
systemctl --user restart llm-proxy
systemctl --user stop llm-proxy
systemctl --user start llm-proxy
journalctl --user -u llm-proxy -f
```

## Cập nhật source và binary

```sh
cd /duong/dan/llm-proxy
git pull --ff-only
cargo install --path . --locked --force --root "$HOME/.local"
systemctl --user restart llm-proxy
```

Nếu release đổi unit, copy lại file service và daemon-reload trước restart.
Không cần login lại khi chỉ cập nhật binary; login lại nếu credential bị thu hồi.

## Phạm vi kiểm chứng

Các lệnh CLI và unit đã được kiểm tra trên máy phát triển Ubuntu/WSL; JSON merge
được kiểm tra với thư mục tạm. Chưa chạy toàn bộ hướng dẫn từ một Ubuntu VM sạch
và chưa xác nhận inference/account entitlement chỉ từ health hoặc test mock.

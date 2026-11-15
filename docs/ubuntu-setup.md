# Cài trên Ubuntu mới

Ubuntu x64/ARM64, Claude Code và proxy cùng máy; cần Internet và systemd user.
Không sync credentials qua Git: máy mới đăng nhập account riêng của proxy.
Docker thay thế: xem [README](../README.md#docker--không-cần-rust-trên-host).

## 1. Build/install

```sh
sudo apt-get update
sudo apt-get install -y git curl ca-certificates build-essential pkg-config cmake python3
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
rustup update stable
git clone https://github.com/nghlong3004/llm-proxy.git
cd llm-proxy
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
llm-proxy --version
```

Repo private cần quyền GitHub (hoặc clone SSH). Rust stable >=1.99, không dùng bản apt
cũ; [installer chính thức](https://rust-lang.org/tools/install/). Nếu thiếu PATH, thêm
dòng `export PATH="$HOME/.local/bin:$PATH"` một lần vào `~/.bashrc` hoặc `~/.zshrc`.

## 2. Claude Code và login

```sh
# Nếu chưa cài Claude Code:
curl -fsSL https://claude.ai/install.sh | bash
claude --version
llm-proxy accounts login --name personal --login-method device-code
llm-proxy accounts list
```

[Claude installer](https://code.claude.com/docs/en/setup). Mở URL/code được in trong
browser; device login có thể cần bật ở security/workspace settings. Máy desktop có
thể bỏ `--login-method device-code` để dùng browser callback. Lặp login với tên khác
để thêm account; không cần Codex CLI hoặc copy `~/.codex/auth.json`.

## 3. Chạy nền

Trong repo, dừng foreground cùng cổng trước:

```sh
install -D -m 644 packaging/systemd/llm-proxy.service ~/.config/systemd/user/llm-proxy.service
systemctl --user daemon-reload
systemctl --user enable --now llm-proxy
systemctl --user status llm-proxy --no-pager
curl --fail http://127.0.0.1:18765/health
```

Health status=ok/ready=true chỉ chứng minh readiness, không quyền model/inference.
Unit dùng `gpt-6.1-sol`; đổi flags qua `systemctl --user edit llm-proxy`, rồi restart.
Không expose listener ra public vì chưa có client authentication. Không có systemd
thì chạy `llm-proxy serve --default-model gpt-6.1-sol` bằng supervisor riêng.

SSH server cần chạy sau logout/boot có thể tùy chọn `sudo loginctl enable-linger
"$USER"`; đây là thay đổi lifecycle user manager, không cần cho desktop thông thường.

## 4. Cấu hình client

Chạy **không sudo**, bằng user dùng Claude Code. Snippet merge năm env, giữ các key
khác, backup private một lần, atomic replace; từ chối JSON sai kiểu/symlink hoặc
phát hiện file đổi trong lúc chạy. Không in settings/credential. Chỉ chạy khi muốn
proxy là mặc định user-wide; project settings vẫn có thể override.

```sh
python3 -I - <<'PY'
import json
import os
import tempfile
from pathlib import Path

root = Path((os.environ.get("CLAUDE_CONFIG_DIR") or str(Path.home() / ".claude"))).expanduser()
root.mkdir(mode=0o700, parents=True, exist_ok=True)
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
    "CLAUDE_CODE_AUTO_MODE_SERVER": "0",
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

Mở phiên mới, vào project rồi `claude`. Nếu gateway không đúng, kiểm tra `/status`
và project overrides. `local` chỉ là giá trị client yêu cầu, không xác thực listener.
Auto mode cần `CLAUDE_CODE_AUTO_MODE_SERVER=0` ở client; reviewer mặc định Luna,
main Sol giữ nguyên. [Cấu hình/giới hạn review](../README.md#auto-mode-route-classifier-riêng).

## Quản lý / cập nhật

```sh
systemctl --user restart llm-proxy
systemctl --user stop llm-proxy
systemctl --user start llm-proxy
journalctl --user -u llm-proxy -f
# Trong repository, cập nhật binary rồi restart:
git pull --ff-only
cargo install --path . --locked --force --root "$HOME/.local"
systemctl --user restart llm-proxy
```

Nếu unit đổi, copy lại + daemon-reload trước restart. Đổi binary không cần login
lại, trừ credential bị thu hồi. Config state xem [README](../README.md#config-và-account-policy).
CLI/unit và settings merge được kiểm tra bằng state giả trên Ubuntu/WSL; chưa có
full clean-VM acceptance hoặc live entitlement/inference acceptance.

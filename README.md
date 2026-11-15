# llm-proxy

Proxy Rust localhost để **Claude Code gọi Codex bằng subscription ChatGPT**.
Hỗ trợ nhiều account, giữ account active đến khi xác nhận hết quota rồi chuyển
account. Claude Code vẫn sở hữu files/shell/tools/subagents; proxy không chạy tool.
V1 chỉ có Anthropic Messages adapter + Codex provider.

Máy Ubuntu mới: [hướng dẫn cài đặt](docs/ubuntu-setup.md).
Trạng thái kiểm chứng: [checkpoint](docs/implementation-progress.md).

## Docker — không cần Rust trên host

Cần Docker Engine + Compose; chạy trong repository đã clone:

```sh
docker compose up -d --build
docker compose exec proxy llm-proxy accounts login --name personal --login-method device-code
docker compose exec proxy llm-proxy accounts list
curl --fail http://127.0.0.1:18765/health
```

Build local từ source, chưa publish image; lần đầu cần mạng. Runtime non-root
10001:10001, không import credential host. Device login in URL/code để mở browser.
Nếu cổng đã dùng, đặt `LLM_PROXY_PORT=18766` trước lệnh `up`; đổi model bằng
`LLM_PROXY_MODEL=gpt-6.1-sol`. Giữ các biến ở lần `up` sau (hoặc `.env` local).

```sh
docker compose logs -f proxy
docker compose exec proxy llm-proxy accounts remove ACCOUNT_ID
docker compose stop                 # SIGINT, giữ container/state
docker compose start
docker compose up -d --build        # cập nhật sau khi pull source
docker compose down                # giữ named volume
```

State/config/log tại `/var/lib/llm-proxy/state` trong named volume; giữ Compose
project name để dùng lại volume. **`down -v` xóa account/credential.** State `0700`,
files `0600`; custom mount cần đúng ownership, không có root entrypoint sửa quyền.
Muốn dùng config riêng, chỉnh bản example rồi ghi bằng user runtime (ghi đè config
hiện có, nên chỉ dùng khi khởi tạo hoặc đã sao lưu):

```sh
docker compose exec -T proxy sh -c 'umask 077; tee "$LLM_PROXY_STATE_DIR/config.toml" >/dev/null' < config.example.toml
docker compose restart proxy
```

Container bind wildcard **chỉ bên trong**; Compose publish host loopback. Listener
**không có client authentication**: không expose public, không dùng host networking
hay nối container không tin cậy vào network. Process truy cập được API có thể dùng quota.

## Native Rust

Cần Rust stable (>=1.99), linker/C compiler, `pkg-config`; toolchain/components được
chọn bởi `rust-toolchain.toml`, dependencies khóa trong `Cargo.lock`.

```sh
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
llm-proxy accounts login --name personal
llm-proxy accounts login --name work --login-method device-code
llm-proxy accounts list
llm-proxy serve --default-model gpt-6.1-sol
```

Browser login dùng callback `127.0.0.1:1455`; chọn đúng account/cửa sổ riêng tư.
Device-code phù hợp headless, có thể cần bật trong ChatGPT security/workspace settings.
Proxy có credentials riêng, không đọc/ghi `~/.codex/auth.json`. Login lại cùng
user/workspace cập nhật account; hai người cùng workspace không bị gộp.

### Chạy nền systemd user (Linux/WSL)

Dừng foreground cùng cổng trước; unit mẫu dùng binary `~/.local/bin` và model Sol:

```sh
install -D -m 644 packaging/systemd/llm-proxy.service ~/.config/systemd/user/llm-proxy.service
systemctl --user daemon-reload
systemctl --user enable --now llm-proxy
systemctl --user status llm-proxy --no-pager
# Quản lý:
systemctl --user restart llm-proxy
systemctl --user stop llm-proxy
journalctl --user -u llm-proxy -f
```

Cập nhật bằng `cargo install --path . --locked --force --root "$HOME/.local"`, rồi
restart service; không cần login lại chỉ vì đổi binary. Sửa flag bằng
`systemctl --user edit llm-proxy`. Unit dùng SIGINT/grace 45s; theo lifecycle user
manager, WSL shutdown vẫn kết thúc process. Không tự bật linger.

## Kết nối Claude Code

Proxy mặc định `http://127.0.0.1:18765`. Mở terminal khác:

```sh
ANTHROPIC_BASE_URL=http://127.0.0.1:18765 \
ANTHROPIC_AUTH_TOKEN=local \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK=1 \
CLAUDE_CODE_AUTO_MODE_SERVER=0 \
claude --model sonnet
```

Để chỉ cần gõ `claude`, **merge**, không thay toàn bộ file, năm biến vào `env` của
`~/.claude/settings.json`; mở phiên mới. [Ubuntu guide](docs/ubuntu-setup.md#4-cấu-hình-client)
có snippet merge/backup an toàn. `local` không phải Codex credential hay API auth.
Cấu hình user ảnh hưởng mọi project; settings project có thể ghi đè.

Alias `sonnet`/`haiku`/`opus`/`fable` và tên `claude-<family>-...` chưa map riêng theo
model mặc định. `/v1/models` chỉ liệt kê registry, **không chứng minh entitlement**.
Hậu tố `[1m]` không làm tăng context window.

## Auto mode: route classifier riêng

Đặt biến ở **process Claude Code**, không phải proxy/container:

```sh
CLAUDE_CODE_AUTO_MODE_SERVER=0 claude --model sonnet --permission-mode auto
```

Đây là [client-side classifier mode](https://code.claude.com/docs/en/auto-mode-classifier-billing.md#make-the-session-eligible),
không bỏ kiểm tra quyền. Có thêm latency/quota. Proxy không hỗ trợ server-side
`safeguards`/`safeguard_results`, không tạo verdict allow hoặc tự chạy hành động.

**Mặc định reviewer là `gpt-6-luna`**, kể cả bỏ table/model; provider kế thừa
`default_provider`. Main conversation không đổi. Đổi bằng state `config.toml`, restart:

```toml
[auto_review]
provider = "codex"
model = "gpt-6-luna"
# model = "sonnet"  # thay Luna bằng alias model chính hiện hành
```

Model/provider phải hợp lệ và account có quyền; lỗi không fallback hay fake allow.
Policy không hạ effort. Nhận diện hẹp: exact classifier system-prefix, non-streaming,
không tools. Giữ transcript/schema nhưng không session cache/encrypted-reasoning
replay. Đây là routing hint, không authentication; token counting không đổi route.

`RequestRoutingPolicy` Strategy tách nhận diện ở Adapter và generation ở Provider;
JEV chỉ thêm khi có API/auth implementation thật. Chưa có JEV provider. Model rẻ
hơn không chứng minh quota subscription giảm 20 lần hay safety tương đương Claude;
non-Claude routing không được Anthropic hỗ trợ chính thức. Smoke mock bên dưới chỉ
kiểm tra classifier failure đóng an toàn, không nghiệm thu classifier Codex thật.

## Config và account policy

State mặc định `$XDG_STATE_HOME/llm-proxy` hoặc `~/.local/state/llm-proxy`;
`--state-dir PATH` / `LLM_PROXY_STATE_DIR` override. Mọi lệnh dùng cùng state.
Chỉnh [config.example.toml](config.example.toml), lưu thành state `config.toml`.
CLI `serve --default-model`, `--listen-address`, `--listen-port` override TOML.
Config đọc lúc startup nên thay config cần restart; login/remove áp dụng request sau.

- Responses Lite mặc định cho họ gpt-5.6/gpt-6, tool tuần tự. `responses_lite = false`
  chọn full lane nếu account/model hỗ trợ; giữ giới hạn parallel của client.
- Hosted search tự động dùng full lane; forced search riêng dùng `/alpha/search`.
  Luna chuyển sang Sol tương ứng khi cần hosted search; có thể đặt `search_model`.
- Chọn account theo insertion order, sticky active; hard quota mới failover. Generic
  429, model/permission, mạng/5xx không phải hết quota. Definite credential rejection
  hoặc account changed trước dispatch có thể reselect an toàn.
- Account hết quota chờ reset (thiếu reset: cooldown 60s); reset không giành lại
  active tốt. Tất cả hết trả rate-limit; no usable login trả 503.
- Không replay sau message/content hoặc khi upstream processing không rõ. Request
  đã lease giữ credential; stale login/refresh không ghi đè generation mới/resurrect.
- Credentials plaintext riêng tư: directories `0700`, files `0600`, locking + atomic
  replace. Không commit state/credentials hoặc đặt trong thư mục chia sẻ.

## Tương thích, API và giới hạn

Hỗ trợ text, function tools/results/schema, ảnh base64 JPEG/PNG/GIF/WebP kể cả trong
tool result, reasoning effort/summary, structured output, SSE/buffered, WebSearch/
domain filters, portable `/compact`. Encrypted reasoning cần đúng HMAC/account/
generation/model; đổi account giữ transcript/summary nhưng bỏ native state.

Không phải toàn bộ Anthropic API: chưa có ảnh URL/audio/document hoặc hosted tools
không map được; cache hints, `max_tokens` và search-count limits không có mapping
tương đương. Token count là estimate local, không billing. Không native compaction/
`previous_response_id`; context vượt giới hạn vẫn có thể bị upstream từ chối.

| Route | Chức năng |
| --- | --- |
| `POST /v1/messages` | Messages stream/buffered |
| `POST /v1/messages/count_tokens` | Ước lượng local, không gọi upstream |
| `GET /v1/models` | Registry, không entitlement check |
| `GET /health` | Readiness, không credential/inference |

32 admission slots trước khi đọc JSON; overload 503, upload timeout 30s. Request
32 MiB, SSE frame 8 MiB, retained output 16 MiB. Connect 15s, upstream idle 300s,
heartbeat 15s. Đây không phải RAM cap. Disconnect hủy inference nhưng blocking jobs
giữ permit đến khi xong; refresh đã dispatch hoàn tất guarded persistence và drain
khi shutdown. SIGKILL/disk/transport failure vẫn có thể buộc login lại.

## Debug và phát triển

Logs metadata: `<state>/logs/proxy.jsonl` + hai archives ~10 MiB/file, stderr;
`log_level = "info"` (off/error/warn/info/debug/trace). Dùng response `x-request-id`
để nối `request_route_selected` (DEBUG), `upstream_attempt/failure`, completion/cancel.
Native diagnostics chỉ giữ static code/status/allowlisted parameter, không raw body.
Không prompt/tool data/token/OAuth code/encrypted reasoning/session ID; dependency
HTTP logging tắt, queue 1024 đầy bỏ record, không audit guarantee.

Đọc [AGENTS.md](AGENTS.md) theo thứ tự quy định; boundaries tại
[architecture](docs/architecture.md), lệnh/naming tại [conventions](docs/conventions.md).

```sh
./scripts/check-local.sh            # fmt, strict Clippy, all tests incl ignored, release
docker compose config --quiet
docker compose build
python3 -I scripts/check-docker.py  # isolated synthetic Compose smoke
python3 -I scripts/check-auto-review.py  # installed Claude CLI + mock, fail-closed only
python3 -I scripts/check-resources.py --seconds 180 --workers 8 --output /tmp/llm-proxy-resources.json
```

Tests chỉ dùng fake accounts/upstreams; localhost tests ignored mặc định được gate
chạy với `--include-ignored`. Docker smoke dùng project/volume/random port riêng;
resource script đo finite soak/RSS/CPU/FD/threads, không chứng minh leak-free nhiều
ngày. Auto-review script không tạo allow; exit 2 = BLOCKED/inconclusive.
Live acceptance cần ủy quyền riêng: Claude Code đọc/sửa file, test/subagent, ảnh,
WebSearch, `/compact`; kiểm tra quota failover bằng mock, không cố tiêu quota thật.

Codex wire mapping tham khảo [raine/claude-code-proxy](https://github.com/raine/claude-code-proxy);
revision/generation tại [fixture provenance](tests/fixtures/codex/README.md).
MIT attribution giữ trong [LICENSE](LICENSE), cũng nằm trong runtime image.

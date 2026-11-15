# Checkpoint — 2026-11-15

Đọc tiếp: [architecture](architecture.md) → [approved v1 plan](superpowers/plans/2026-11-12-llm-proxy-v1.md)
→ [conventions](conventions.md). Chỉ ghi trạng thái hiện tại; lịch sử xem Git.

## Mốc hiện tại: layout B + Rust 1.100

- User yêu cầu chuyển sang Hexagonal layout B, không đổi hành vi; branch
  `refactor/hexagonal-layout`, base `db7b27e`. Chưa commit/push trong task này.
- Đã chuyển concrete adapters vào `adapters/inbound/{cli,http/anthropic}` và
  `adapters/outbound/codex`; interfaces ở `ports`, wiring ở `bootstrap`.
- `application` giữ preflight/routing/failover/count/health và account command
  workflows; không import HTTP/concrete adapters. TokenEstimator inject qua port.
- HTTP giữ parsing/admission/SSE/cancel/body permit; application/blocking closures
  giữ work permit. Giữ model/error precedence, stateless review, quota/no-replay,
  login ticket trước auth polling và refresh persistence/shutdown semantics.
- Không root aliases cũ, không JEV giả, không thêm DI/storage/auth framework.
- Rust/Cargo **1.100.0** đã install; toolchain/Docker/CI/MSRV chuyển 1.100.
  `rustfmt.toml`: edition 2024, width 100; macro bodies giãn thủ công, không minify.
- Defaults không đổi: main `gpt-6.1-sol`, reviewer `gpt-6-luna`; client server=0.
  Không thay CLI/TOML/wire protocol hoặc account state.

## Kiểm chứng

| Check | Kết quả |
| --- | --- |
| Existing relocated tests, Rust 1.100 | **106 PASS**, gồm localhost/ignored |
| Application không HTTP/concrete provider | **2 PASS** |
| Dependency-direction/root-alias guard | **2 PASS** |
| CLI suite sau bootstrap extraction | **4 PASS** (nằm trong 106 cũ) |
| Full fmt/strict Clippy/all tests/release gate | **PASS: 110 tests, 0 fail/ignored**, Rust 1.100.0 |
| Rust 1.100 Docker build + isolated smoke | **PASS**: non-root/private state/persistence/hot removal/SIGINT |
| Claude Code 2.1.322 mock fail-closed smoke sau refactor | **PASS**, exit0, classifier error chặn command |
| Docs links/shell snippets/TOML + stale path scan | **PASS** |

CLI smoke sau refactor: 12 ingress/native requests, main Sol + explicit reviewer
gpt-5.5, fake upstream503; command không chạy, client exit0. Không test safety/
entitlement Luna hoặc dịch vụ thật. Rust1.100 Clippy phát hiện lock-scope ở test mới;
đã sửa bằng lexical scope, không suppress lint. Không thêm/change dependencies
hoặc Cargo.lock. Image/mock checks dùng state/port riêng và đã cleanup.

## Runtime / giới hạn

- Task này không install/restart service hoặc sửa settings/config runtime. Bản đã
  install trước refactor có Luna default; config private codex/gpt-6-luna và client
  server=0 đã có. Restart cũ từng bị chặn quyền; không tự retry/lách trong refactor.
- Live Claude Code/subscription và reviewer safety/latency chưa nghiệm thu; không
  đọc credentials hoặc gửi inference thật để tự kiểm tra. JEV chờ API/auth contract.
- Resource baseline cũ: soak 180s/8 workers/19.352 lượt; FD về11, idle CPU0,03% core;
  input gần32MiB peak268MiB, idle237,51MiB. Không chứng minh leak-free/RAM cap.

## Tiếp theo

1. Refactor/checks hoàn tất trong working tree; inspect `git diff` trước commit.
2. Chỉ commit/push/install/restart khi user yêu cầu cho mốc này.
3. Live subscription/clean-VM acceptance và JEV vẫn ngoài scope.

Lệnh: `CARGO_HOME=/tmp/llm-proxy-cargo ./scripts/check-local.sh`; dùng `--offline`
nếu cần cache. Boundaries/limits duy trì tại architecture, usage tại README.

# Checkpoint — 2026-11-15

Đọc tiếp: [architecture](architecture.md) → [approved v1 plan](superpowers/plans/2026-11-12-llm-proxy-v1.md)
→ [conventions](conventions.md). Đây là trạng thái hiện tại, không phải lịch sử hội thoại;
chi tiết mốc cũ xem Git. Không lưu credentials/payloads hoặc log tạm dài hạn ở đây.

## Hiện trạng / quyết định

- V1 đã có CLI/config, private multi-account storage, browser/device OAuth + guarded
  refresh, sticky quota failover, Codex HTTP/SSE, Anthropic stream/buffered, tools,
  images/reasoning/structured output/WebSearch và portable history/compaction.
- Docker/Compose + systemd user packaging đã có; native/host publish mặc định loopback,
  runtime non-root/private state. Logging chỉ metadata, bounded/rotated.
- Auto-review dùng provider-neutral `RequestRoutingPolicy` Strategy; Adapter nhận
  exact client classifier prefix, route immutable mỗi generation. Không proxy tools,
  không server safeguards hoặc tự tạo allow. JEV chờ API/auth contract, chưa implement.
- **Quyết định mới:** review mặc định `gpt-6-luna`; bỏ table/model hoặc chỉ đặt provider
  vẫn Luna. Provider mặc định kế thừa `default_provider`; explicit model/alias override
  (ví dụ `sonnet` theo main alias). Main model Sol không đổi; không hardcode trong
  Provider. Programmatic `model: None` vẫn có requested-model fallback ở policy.
- Reviewer giữ transcript/schema/effort, không session/cache key hoặc encrypted replay.
  Giá API thấp hơn không chứng minh subscription quota giảm 20 lần hay safety tương
  đương Claude; chưa nghiệm thu classifier Codex thật.
- Docs đã gộp về checkpoint ngắn + architecture/usage theo trách nhiệm; không tạo
  archive trùng Git. Giữ approved v1 scope, security/cancellation/verification rules
  và MIT provenance trong `LICENSE`/fixture README.

## Kiểm chứng

| Check | Kết quả thực tế |
| --- | --- |
| Regression default Luna | RED đã tái hiện; GREEN `auto_review` **8/8 PASS** |
| `cargo fmt --check` | **PASS** sau default Luna |
| Strict Clippy all-target/all-feature/locked | **PASS** sau default Luna |
| Full tests `--locked -- --include-ignored` | **106 PASS, 0 fail/ignored** sau default Luna |
| Release build `--locked` | **PASS** sau default Luna |
| Image build + isolated Docker smoke | **PASS** sau default Luna, non-root/persistence/hot removal/SIGINT |
| Claude Code 2.1.322 classifier probe | **PASS mock fail-closed**, không live acceptance |
| Docs links/shell/settings snippet | **PASS** local links/anchors/fences, bash syntax, isolated merge cases |

CLI probe dùng temp HOME/state và synthetic accounts: 2 main-stream + 10 review
requests; explicit reviewer gpt-5.5, main gpt-6.1-sol, mock reviewer trả 503. Không
session/window/cache headers, client trả classifier-error, synthetic command không
chạy; không tạo verdict allow hay tiêu quota thật. Đây chưa phải probe default Luna.

Resource baseline (2026-11-13, release + fake upstream): 180s/8 workers, 19.352 lượt;
FD về 11, active upstream về 0, idle CPU 0,03% core. RSS sau warm-up 77,88 MiB,
steady samples 78,64–112,45 MiB; riêng input gần 32 MiB peak 268 MiB, idle còn
237,51 MiB. Finite soak không chứng minh leak-free; allocator/JSON copies giữ high-water,
không coi bounds byte/concurrency là RAM cap. Script tái chạy trong README.

## Runtime / Git

- Binary Luna-default đã install; cmp với release + `--version` **PASS**. Config
  `~/.local/state/llm-proxy/config.toml` `0600` đặt codex/gpt-6-luna; client server=0
  đã có. Main model service vẫn `gpt-6.1-sol`; không đọc/đổi credentials.
- **Restart BLOCKED:** chờ idle 180s không thành công; lần thử tiếp bị tool permission
  chặn. Kiểm tra 2026-11-15: PID `151278` còn dùng inode binary cũ; health ok/ready=true.
  Config mới chưa được process này nạp. User cần tự restart lúc phù hợp bằng
  `! systemctl --user restart llm-proxy`; chưa kiểm tra entitlement/inference thật.
- User cho phép commit/merge mọi branch vào main và push origin/main; không thêm
  author/co-author trailer. Nhánh Docker đã là ancestor của feature. Dùng Git để
  xem branch/commit/remote hiện tại, không duy trì bản sao lịch sử tại đây.

## Còn lại / giới hạn

1. Restart runtime và xác nhận PID mới dùng binary đã cài; health không gọi inference.
2. Live Claude Code/subscription acceptance vẫn chờ ủy quyền riêng: file read/edit,
   tests/subagent, image/WebSearch, `/compact`, reviewer chất lượng/safety/latency.
   Không đọc credentials sẵn có hay tiêu quota thật để tự nghiệm thu. Quota failover
   kiểm tra bằng mock, không cố làm cạn account. Clean Ubuntu VM acceptance chưa có.

Lệnh gate: `./scripts/check-local.sh`; nếu cache nằm ngoài mặc định, dùng
`CARGO_HOME=/tmp/llm-proxy-cargo` và `--offline` phù hợp. Tests chỉ fake accounts/
upstreams; giới hạn v1 và bounds xem architecture/README, không lặp ở checkpoint.

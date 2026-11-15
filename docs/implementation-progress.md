# Current checkpoint

## State

- Routing/effort/storage-boundary fixes are complete on `feat/agy-outbound`, following
  commit `36475c0`. No new push/install/deployment occurred.
- `Config::recognize_model` only recognizes client models/aliases; `resolve_route`
  chooses the final provider before canonical normalization/options. Inference gets
  one immutable `Arc<RequestRoute>` through every account attempt. Providers never
  re-resolve global routing; client response model identity is preserved.
- Default AGY + Codex review `gpt-5.5-fast` now produces Codex `gpt-5.5` with priority
  tier. Alias/ultrafast and frozen-route failover cases are covered by synthetic tests.
- Reasoning effort is an optional finite neutral enum. Codex retains its legacy
  medium default centralized in config and compaction clamping. AGY rejects every
  explicit effort as `unsupported_reasoning_effort` before account selection/auth/
  inference; omission retains its model/config thinking policy. Counting tokens
  remains model-recognition-only, independent of generation capabilities.
- Neutral account DTOs/status/errors/clock live in domain. Inference and usage ports
  receive only AccountIdentity. Core workflows depend on focused synchronous account
  repository ports, not filesystem Store or raw File handles. The filesystem Store
  forwards the same atomic operations, retains its inherent APIs/type aliases/serde
  format, and still validates credentials before projecting a selected account.
- Command repositories own an associated ticket type; Store retains its private
  generation-order ticket. Legacy list JSON/operation bookkeeping stays at the
  bootstrap/CLI boundary. Usage/reset workflows hold opaque cloneable RAII guards;
  blocking selection/journal jobs retain permits/guards despite waiter cancellation.
- Native AGY capabilities/limitations and externally supplied OAuth configuration
  remain documented in [antigravity](antigravity.md), README and config example.
  No existing credentials/config/services were inspected or altered. No authenticated
  live integration has been run; no provider/model fallback was introduced.

## Verification

| Check | Result |
| --- | --- |
| `cargo fmt --check` | PASS |
| Strict Clippy, all targets/features, locked/offline | PASS: no warnings |
| Full tests, including ignored synthetic localhost contracts | PASS: 245 passed, 0 failed/ignored |
| Release build, locked/offline | PASS |
| Diff whitespace checks | PASS |
| Google OAuth literal scan in source/tests/docs | PASS: external client configuration preserved |
| Live authenticated integration | NOT RUN: requires separate authorization |
| Docker image/smoke | NOT RUN: packaging unchanged |

Regressions exercise actual inbound Codex wire routing under AGY defaults, immutable
route identity through failover, all explicit AGY efforts rejected before dispatch,
empty-account-pool preflight, neutral account/command/usage repository injection,
credential-validated selection, cancelled-blocking-selection admission ownership and
cancelled-blocking-journal reset-guard ownership. Existing account/private-file,
refresh/login/removal races, reset uncertainty and protocol suites remain passing.
Architecture tests forbid concrete account/filesystem imports in inner layers.
Synthetic checks do not prove provider availability, entitlement or model safety.

## Next

- Review the refactor. Clients that explicitly send effort must omit
  that field for AGY or choose a provider supporting it; no mapping is invented.
- Push, authenticated login/live tests, service changes, installation and deployment
  require separate authorization. No synthetic verification blocker remains.

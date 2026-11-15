# Current checkpoint

## State

- Native Antigravity outbound (`agy`) is implemented on `feat/agy-outbound` alongside
  Codex. Existing Codex defaults, legacy commands and account persistence remain
  compatible. No installation or deployment has been performed.
- `config.toml` supports an AGY endpoint/output policy and per-model `provider`.
  Routing precedence: explicit action-review provider → effective model policy →
  default provider. Explicit native Claude model registrations win over generic
  Claude aliases. Account failover never changes the selected provider/model.
- `accounts add agy --name NAME` uses proxy-owned Google browser OAuth with PKCE,
  a state-checked loopback callback, stable user/project identity and actual managed
  project discovery. Refresh reuses bounded single-flight, cross-process locking,
  generation-safe atomic persistence, cancellation independence and shutdown drain.
  OAuth client configuration is external: `LLM_PROXY_AGY_CLIENT_ID` and
  `LLM_PROXY_AGY_CLIENT_SECRET` must be provided to login and refresh processes;
  source/tests contain no embedded Google OAuth client credentials. Missing config
  fails before exchange, without disabling an account or breaking Codex startup.
  `authentication/oauth.rs` owns only the shared refresh lifecycle; `codex.rs` and
  `agy.rs` contain provider-specific endpoints, grants and revocation semantics.
- Native request/SSE translation supports Claude text/images/function tools/results,
  buffered and streaming responses, bounded output and scoped thought-signature
  replay. Rich tool schemas retain constraints through `parametersJsonSchema`;
  internal-service acceptance of that field is not live-verified.
- Gemini currently supports text/images only. Hosted search, compaction, forced tool
  choice and structured output are unsupported; configure a Codex review route for
  structured Auto-mode classifiers. AGY live usage and banked resets are explicitly
  unsupported; offline status stays credential/config/network independent.
- Shared outbound SSE/body limits were extracted without changing existing Codex
  import paths. No prompt rewriting, guessed project IDs, signature bypass sentinels
  or generic-429 quota failover. No existing credentials/config/services were touched.
- Protocol reference and MIT attribution are pinned in [antigravity](antigravity.md)
  to upstream `7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc`. See README and
  `config.example.toml` for native login and mixed-provider routing.

## Verification

| Check | Result |
| --- | --- |
| `cargo fmt --check` | PASS |
| Strict Clippy, all targets/features, locked/offline | PASS |
| Full tests, including ignored synthetic localhost contracts | PASS: 233 passed, 0 failed/ignored |
| Release build, locked/offline | PASS |
| Release CLI with disposable private state | PASS: help, offline AGY status, missing OAuth config fails before login; no network |
| Google OAuth client literal scan | PASS for working tree and staged changes; tests use synthetic client values |
| Diff whitespace checks | PASS |
| Live authenticated integration | NOT RUN: requires separate authorization |
| Docker image/smoke | NOT RUN: packaging unchanged; AGY browser callback is not published by shipped Docker configuration |

Synthetic contracts cover OAuth/discovery, refresh rotation/cancellation/generation
races, provider routing, credential isolation, tools/schema fidelity, thought-signature
scope/chunk completion, usage/finish metadata, malformed streams, resource bounds,
401-only retry, ambiguous 429 non-failover and disconnect cancellation. They do not
prove backend availability, model entitlement, service-terms permission or model safety.

## Next

- Review the feature diff and choose account-supported canonical models in config.
- Native AGY login/live verification, service changes, push and deployment require
  separate authorization. No blocker remains from synthetic verification.

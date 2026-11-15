# LLM Proxy — contributor instructions

## Workflow and continuity

Use `project-context` when resuming, checkpointing or handing off; if unavailable,
read `.agents/skills/project-context/SKILL.md`. Never rely on chat history alone.
Before modifying code, read in order:

1. `docs/implementation-progress.md` — current state, checks, blockers, next steps.
2. `docs/architecture.md` — boundaries, data flow and invariants.
3. `docs/superpowers/plans/2026-11-12-llm-proxy-v1.md` — approved scope.
4. `docs/conventions.md` — names, commands and reference provenance.

Follow approved decisions; explain conflicts/critical assumptions before irreversible
changes. Update progress immediately after each milestone/material decision; replace
obsolete checkpoints instead of appending transcripts. Update architecture/usage docs
when interfaces, commands or structure change. Use `rust-best-practices` and
`rust-async-patterns` when implementing/reviewing Rust.

## Product and boundaries

- Latest stable Rust/dependencies, edition 2024; commit `Cargo.lock`.
- CLI-managed localhost proxy using multiple Codex subscription accounts. Preserve
  active account until confirmed exhaustion; runtime account changes affect later requests.
- Target Claude Code tools/images/reasoning/structured output/SSE/WebSearch/portable
  compaction; reference `raine/claude-code-proxy`. **Never execute agent tools:**
  Claude Code owns files, shell and the agent loop.
- Keep modules isolated as mapped in `docs/architecture.md`: CLI only wiring;
  domain protocol-neutral; adapters own wire schemas/errors; providers own upstream
  translation; accounts own identity/storage/selection; authentication owns OAuth;
  server owns orchestration/cancellation; logging owns safe metadata. No layer leakage.

## Mandatory invariants

### Security
- Private directories `0700`, files `0600`; atomic replacement + cross-process locking.
- Identity distinguishes users in one workspace. Stale login/refresh must neither
  overwrite newer credentials nor resurrect removed accounts.
- Never log prompts, tokens, OAuth codes or encrypted reasoning; never commit credentials.
- Never forward incoming `ANTHROPIC_AUTH_TOKEN` as a Codex credential.
- Bound upstream requests/bodies. Listener has **no client authentication**: native
  defaults and Docker host publication stay loopback; wildcard bind only explicitly
  inside isolated containers. Never import host credentials into images.

### Routing and lifecycle
- Quota failover requires confirmed exhaustion, not generic 429, model/permission,
  network or 5xx errors. Before output, definite credential rejection or account
  removal/change before dispatch may safely reselect; unrelated storage/network
  errors must not be classified as account changes.
- Never replay after a message/content event or uncertain upstream processing.
- Encrypted reasoning replay requires verified account, generation and model identity.
- Mutations must not corrupt in-flight state. Disconnect cancels inference; retain
  permits until outstanding blocking jobs finish. Dispatched OAuth refresh instead
  finishes guarded persistence independently of the client and drains on shutdown.

### Rust/async
- Meaningful enums and `Result`; no `unwrap`/`expect` outside tests. Focused functions,
  abstractions only for concrete needs, explicit errors over implicit fallbacks.
- CPU-intensive work and blocking filesystem/locks use `spawn_blocking`.
- Never hold registry locks across network `.await`.

## Verification and authorization

Before reporting implementation complete, run and report each actual PASS/FAIL/BLOCKED:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
```

Use cached dependencies with `--offline` if needed. Code changes alone are not
verification; distinguish mock tests from real Claude Code/subscription acceptance.
Docker changes also require image build + isolated Compose smoke: non-root, private
persistent state, loopback publication, SIGINT shutdown; unique project/disposable
synthetic state/random host port, without altering an existing service.

Never inspect existing credentials, run live authenticated tests, publish, push or
deploy without explicit authorization. Preserve MIT attribution/reference revisions.
Prefer targeted changes; preserve unrelated user work.

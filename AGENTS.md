# LLM Proxy

## Context Continuity

Use the `project-context` skill when resuming previous work,
creating milestone checkpoints, or preparing a session handoff.
If the harness does not expose that skill, read
`.agents/skills/project-context/SKILL.md` directly and follow its checklist.

Keep `docs/implementation-progress.md` current after each milestone
or material decision, even if the skill is not activated.

Never rely solely on conversation history for project state.

## 1. Workflow

Before modifying code, read in order:

1. `docs/implementation-progress.md` — current state, verified work, decisions, next steps. Use as the continuity source.
2. `docs/architecture.md` — architecture, module boundaries, data flow, invariants.
3. `docs/superpowers/plans/2026-11-12-llm-proxy-v1.md` — approved v1 implementation plan.

Rules:
- Follow the approved architecture and plan. Do not silently change established decisions.
- Update `implementation-progress.md` immediately after each milestone or decision change, recording results, blockers, and next steps.
- Never mark work complete without running its relevant checks.
- Update architecture and usage docs when changing interfaces, commands, or project structure.
- Read `docs/conventions.md` for command and directory conventions.
- Use `rust-best-practices` and `rust-async-patterns` skills when implementing or reviewing Rust.

## 2. Product Requirements

- Latest stable Rust, edition 2024; latest compatible stable dependencies; commit `Cargo.lock`.
- Standalone localhost proxy, CLI-managed, backed by Codex subscription accounts.
- Support multiple accounts. Keep the active account until confirmed quota exhaustion, then switch to the next eligible account.
- Support adding/removing accounts at runtime; changes affect subsequent requests.
- Target Claude Code compatibility using `raine/claude-code-proxy` as reference: tools, images, reasoning, structured output, SSE, WebSearch, portable compaction.
- Never execute agent tools inside the proxy. Claude Code owns filesystem operations, terminal execution, and the agent loop.

## 3. Architecture Boundaries

| Module | Responsibility |
|---|---|
| `main.rs` | Entrypoint only |
| `cli/` | Command parsing and wiring; no business logic |
| `domain/` | Protocol-neutral requests, events, errors, non-streaming accumulator |
| `protocols/` | Agent protocol adapters; Anthropic schemas, SSE, errors |
| `providers/` | `Provider` strategy, registry, implementations |
| `providers/codex/` | Translation, search, reasoning, SSE decoding, upstream client |
| `accounts/` | Identity, storage, selection; Anthropic-agnostic |
| `authentication/` | OAuth and token refresh; independent of Codex CLI credentials |
| `server/` | HTTP routing, orchestration, cancellation, failover |
| `logging/` | Metadata-only JSONL, bounded background queue, rotation |
| `config.rs` | TOML, model registry/aliases, endpoints |
| `tests/` | Behavioral tests using fake accounts/upstreams; no real quota consumption |

Keep responsibilities isolated. Avoid cross-layer protocol leakage.

## 4. Mandatory Invariants

### Security
- Credential directories: `0700`; files: `0600`. Use atomic replacement and cross-process locking.
- Stale login/refresh operations must never overwrite newer credentials or resurrect deleted accounts.
- Account identity must distinguish users within the same workspace.
- Never log prompts, tokens, OAuth codes, or encrypted reasoning. Never commit credentials.
- Never forward incoming `ANTHROPIC_AUTH_TOKEN` upstream as a Codex credential.
- Bound upstream request/body sizes.
- Listener has no client authentication: keep native defaults and Docker host publication on loopback. Bind wildcard only explicitly inside isolated containers; never import host credentials into images.

### Routing and Recovery
- Quota failover requires confirmed exhaustion, never generic HTTP 429, model/permission errors, or network failures alone. Before output, definite credential rejection or an account removed/changed before dispatch may also trigger safe reselection; do not classify unrelated storage/network errors as account changes.
- Never automatically replay after a message/content event or when upstream processing status is uncertain.
- Replay encrypted reasoning only when account, generation, and model identity are verified.
- Client disconnect must cancel/release inference work and concurrency permits; retain permits until outstanding blocking jobs complete. Dispatched OAuth refresh transactions are the exception: finish guarded persistence independently of the client and drain on graceful shutdown.
- Account mutations must not corrupt in-flight request state.

### Rust and Async
- Prefer meaningful enums and `Result`; avoid `unwrap`/`expect` outside tests.
- Keep functions focused; introduce traits/abstractions only for concrete needs.
- Offload CPU-intensive and blocking filesystem/lock operations using `spawn_blocking`.
- Never hold registry locks across network `.await` calls.
- Prefer explicit errors and deterministic behavior over implicit fallbacks.

## 5. Verification Gate

Before reporting implementation complete, run:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
```

- If networking is restricted, reuse cached dependencies with `--offline`.
- Report each check as passed, failed, or blocked with its actual result.
- Distinguish mock verification from real Claude Code integration testing.
- Never inspect existing credentials or run live authenticated tests without explicit authorization.
- Do not claim success based solely on code changes.
- Docker changes additionally require an image build and isolated Compose smoke (non-root, private persistent state, loopback publication, SIGINT shutdown). Use a unique project, disposable synthetic state and random host port; do not alter an existing user service.

## 6. Constraints

- Do not publish, push, or deploy without explicit authorization.
- Preserve MIT attribution and reference sources when porting code.
- Prefer minimal, targeted changes over unrelated refactoring.
- When requirements conflict or a critical assumption is uncertain, explain the trade-off before making irreversible changes.
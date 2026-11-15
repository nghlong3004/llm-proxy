# Agent instructions

## Working context

- Read `docs/implementation-progress.md` first. Consult `docs/architecture.md` for
  boundaries and `docs/conventions.md` for commands/style relevant to the task.
- Keep the checkpoint current after milestones: present state, actual checks, blockers,
  next action. Replace obsolete notes; use Git for history. Write agent docs in English.
- Preserve unrelated work. Keep changes focused; update affected interfaces/docs.
- Use `rust-best-practices` and `rust-async-patterns` for Rust changes.

## Boundaries

- `bootstrap/` composes implementations; `domain/`, `application/`, `ports/` must not
  import adapters, bootstrap, Axum or reqwest. Follow `docs/architecture.md`.
- CLI parses/formats; HTTP owns transport/admission; application owns use cases.
- Proxy never executes agent tools. Claude Code owns filesystem, shell and agent loop.
- Rust 1.100.0, edition 2024; keep `Cargo.lock`. Run rustfmt (width 100); manually
  format macro bodies it leaves dense. Avoid packed statements and unnecessary traits.
- Use meaningful enums/Result, no unwrap/expect outside tests. Move blocking I/O,
  locks and CPU-heavy work to spawn_blocking; no registry lock across network await.

## Invariants

- State directories 0700, files 0600; atomic writes and cross-process locking.
- Identity includes user/workspace. Stale login/refresh must not overwrite newer
  credentials or resurrect removed accounts; mutations preserve in-flight leases.
- Never log prompts, tokens, OAuth codes, tool payloads or encrypted reasoning;
  never commit credentials or forward incoming client auth as a provider credential.
- Bound bodies/buffers/work. Keep native listeners and Docker host publication on
  loopback; wildcard bind only inside isolated containers. Listener has no client auth.
- Quota failover requires confirmed exhaustion, not generic 429/model/permission/
  network/5xx failures. Pre-output credential rejection or account change may reselect;
  storage errors are not account changes. Never replay after output or uncertain dispatch.
- Replay encrypted reasoning only with verified account/generation/model identity.
- Disconnect cancels inference. Blocking jobs retain permits until completion;
  dispatched refresh persists independently and drains during graceful shutdown.
- Classifier errors remain errors; never synthesize an approval verdict.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked -- --include-ignored
cargo build --release --locked
```

Use `--offline` with cached dependencies when needed. Report actual PASS/FAIL/BLOCKED;
distinguish mock checks from live integration. Docker changes also need image build
and isolated `scripts/check-docker.py` smoke (disposable project/state/random port).
Checks/scripts are listed in `docs/conventions.md`.

Do not inspect existing credentials, run authenticated live tests, alter services,
commit, push or deploy without authorization. Preserve MIT attribution and provenance.

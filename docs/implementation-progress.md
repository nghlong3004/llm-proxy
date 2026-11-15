# Current checkpoint

## State

- Hexagonal layout: bootstrap → adapters → application/domain/ports. See
  [architecture](architecture.md); source and tests are the implementation reference.
- Rust 1.100.0, edition 2024; toolchain, CI, Docker and formatter aligned.
- Refactor committed as `ceba66d`; installation and agent documentation match the layout.
- README owns installation/run/update instructions. Agent docs are concise English;
  configuration defaults belong in code/example config, not checkpoint narratives.

## Verification

| Check | Result |
| --- | --- |
| fmt / strict Clippy / release build | PASS |
| Full tests, including ignored localhost contracts | 110 PASS, 0 failed/ignored |
| Docker build and isolated persistence/shutdown smoke | PASS |
| Installed Claude Code + synthetic classifier failure | PASS: action blocked |
| Documentation links/anchors/fences, shell, JSON/TOML, English agent docs | PASS |

Mock checks verify transport and failure behavior, not live model safety/entitlement.

## Next

- No pending implementation work; no runtime settings/service changes in this task.
- Live authenticated integration requires explicit authorization.

# Current checkpoint

## State

- Hexagonal layout: bootstrap → adapters → application/domain/ports. See
  [architecture](architecture.md); source and tests are the implementation reference.
- Rust 1.100.0, edition 2024; toolchain, CI, Docker and formatter aligned.
- Account CLI is grouped under `accounts`: `add <provider>`, `status [provider]`,
  `reset-quota <provider> ACCOUNT_ID`, and `remove ACCOUNT_ID_OR_PREFIX`.
  Legacy login/list and top-level shortcuts remain compatible but hidden from help.
  Removal accepts unique ID prefixes of at least 8 characters, resolving ambiguity
  under the registry lock; quota redemption still requires a full UUID.
- Default status is offline. Explicit refresh reports provider-defined usage windows,
  prepaid credits and banked resets with observation metadata; unknown/unsupported
  values are not fabricated as zero. Manual refresh also clears captured quota
  exhaustion when the provider authoritatively confirms ordinary usage is available,
  including resets performed outside the proxy. Generation/status-revision checks
  preserve newer status and NeedsLogin; active placement is unchanged. No polling.
- Reset redemption uses a private durable request journal, cross-process exclusion,
  explicit same-key retry, conservative uncertain outcomes and generation/status-revision
  guarded reconciliation. No automatic redemption, purchase or mutation retry.
- Codex account HTTP contracts are pinned to first-party source in the adapter.
  Backend stability and account entitlement have not been verified live.
- Implementation and synthetic verification are complete. No existing credentials,
  real reset credits, installed configuration or services were touched.

## Verification

| Check | Result |
| --- | --- |
| fmt / strict Clippy / release build (cached dependencies, offline) | PASS |
| Full tests, including ignored synthetic localhost contracts | 171 PASS, 0 failed/ignored |
| Release CLI with disposable synthetic state | PASS: grouped help/status, unique-prefix removal; filtering/unsupported/offline behavior covered by integration tests |
| Diff whitespace checks | PASS |
| Docker image/smoke and installed classifier smoke | NOT RUN: packaging unchanged; prior checkpoint passed these before this feature |
| Live authenticated usage/reset integration | NOT RUN: requires separate authorization |

Mock contracts cover account scoping, HTTP parsing/limits, failure redaction, reset
idempotency/uncertainty, cancellation, generation races and reconciliation. They do
not establish live backend availability, reset entitlement or model safety.

## Next

- Review changes; use README for the new CLI and reset recovery commands.
- Installing/restarting services, authenticated tests, commits and deployment require
  separate authorization. No implementation blocker remains from synthetic checks.

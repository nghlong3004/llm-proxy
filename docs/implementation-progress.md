# Current checkpoint

## State

- AGY explicit-login project discovery/onboarding now follows the daily endpoint,
  string metadata, tier order and endpoint-specific headers in
  `NoeFabris/opencode-antigravity-auth` at
  `821f824440b2b7c9ffe96bbdfad9d5cdead2f1de`. The reference fallback version/changelist
  is pinned; no dynamic version lookup was added. Protocol details, assumptions and
  deliberate differences are documented in `docs/antigravity.md`.
- Completed onboarding without a usable project performs at most three read-only
  discovery rechecks. No empty/hardcoded project is accepted. Only explicit login
  onboards; inference/refresh and Codex behavior are unchanged. OAuth client acquisition,
  PKCE, callback state validation, stable identity and credential publication remain intact.
- Sanitized CLI/tracing diagnostics report operation, host, status, selected-tier
  category, done state and project field types, never response values or identifiers.
  Unknown tiers are redacted. Project errors distinguish transport, HTTP/provider,
  malformed/oversized response, timeout and missing project.
- Implementation and required local verification are complete. No existing credentials,
  installed binaries, services or authenticated provider integration were touched.

## Verification

| Check | Result |
| --- | --- |
| Focused synthetic AGY login tests, locked/offline | PASS: all 21 cases in the final full suite; diagnostics assert exact formatter bytes shared by tracing and CLI errors |
| Concurrent fast AGY regressions | PASS: 30 repeated runs of 17 tests (510 executions); initial flaky tracing-capture test replaced by deterministic formatter assertions |
| `cargo fmt --check` / diff whitespace | PASS |
| `cargo clippy --all-targets --all-features --locked --offline -- -D warnings` | PASS |
| `cargo test --locked --offline -- --include-ignored` | PASS: 268 synthetic/unit/integration tests, including ignored localhost contracts |
| `cargo build --release --locked --offline` | PASS |
| Local release CLI help | PASS: retry flags verified without authentication or state access |
| Authenticated live AGY login/provisioning | NOT RUN: no authorization; passing mocks cannot establish that the reported account issue is resolved |
| Docker image/smoke | NOT RUN: no Docker changes |

## Next

- User can explicitly retry login using the local release build and the same state
  directory/account name; exact commands are in `docs/antigravity.md`. No installed
  binary or service replacement is part of this change.
- Provider/account causes of completed onboarding without a project remain unconfirmed.
  If retry fails, inspect only the sanitized error chain. Successful mocks do not
  establish live managed-project provisioning; authenticated verification requires
  separate authorization.

# Current checkpoint

## State

- Automatic pinned AGY OAuth client acquisition is complete on `feat/agy-outbound`.
  No binary installation, push or service change occurred.
- Explicit browser login/refresh lazily loads the public client configuration from
  upstream `7c672fdc6fa62fcd50e8f75df5644dad6a63c2dc`, verifies the exact SHA-256,
  and parses only two string fields. No JavaScript execution, source endpoints/
  prompts, embedded/encoded client values, redirects or branch-head updates.
- Verified reference bytes are private, atomic and cross-process locked under
  `<state-dir>/agy-oauth-client/<revision>/reference.txt`; cached reads recheck the
  digest. The runtime cache is ignored by Git even with state inside the checkout.
  Errors stop OAuth without disabling accounts; cache locks never span HTTP awaits.
- Complete OAuth env overrides still take precedence; partial/empty overrides error.
  Explicit injected test endpoints never fetch the production reference implicitly.
  Login and refresh share Store::root(); construction and offline status never fetch.
  Shared private filesystem helpers preserve account persistence semantics.
- Refresh rechecks shutdown after reference acquisition before starting a rotating
  token exchange. Existing cancellation-independent refresh/publication, generation
  guards, canonical routes and repository boundaries remain unchanged.
- AGY limitations, public-source trust/hash/cache details and optional environment
  overrides are documented in README, config example and [antigravity](antigravity.md).
- No existing credentials were inspected/changed or real login performed. Installed
  `llm-proxy` and running services still have their prior behavior until updated.

## Verification

| Check | Result |
| --- | --- |
| `cargo fmt --check` | PASS |
| Strict Clippy, all targets/features, locked/offline | PASS: no warnings |
| Full tests, including ignored synthetic localhost contracts | PASS: 257 passed, 0 failed/ignored |
| Release build, locked/offline | PASS |
| Disposable release CLI smoke | PASS: help/status do not fetch; partial env override stops before download/OAuth |
| Production reference fingerprint | PASS: matches pinned public reference bytes (no source execution) |
| Source/test/doc Google OAuth literal scan and diff whitespace | PASS |
| Live authenticated Google/AGY integration | NOT RUN: requires separate authorization |
| Docker image/smoke | NOT RUN: packaging unchanged |

Twelve new resolver contracts cover data-only extraction, overrides, integrity,
private/atomic cache reuse/concurrency, redaction, HTTP/status/redirect/body limits,
no outgoing account credentials, no revocation on resolver failure and no rotation
after shutdown during acquisition. Existing account, repository, refresh, quota and
provider regressions remain passing. Synthetic checks do not prove provider client
acceptance, account entitlement or service availability.

## Next

- Review the changes. The built `target/release/llm-proxy` has the new UX;
  installed binaries/services require a separately authorized update.
- Push, authenticated login/live tests, binary installation and service changes
  require separate authorization. No synthetic verification blocker remains.

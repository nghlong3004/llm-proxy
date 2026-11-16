# Updating

Update from the existing checkout, keeping the same state directory or Docker volume;
a binary/image update alone does **not** require logging in again. See
[installation](installation.md) and [configuration](configuration.md) for setup/precedence.
Do not run login or authenticated tests as an update check.

## Before updating

1. Inspect `git status --short` and `git branch --show-current`. Proceed only with a
   clean working tree on the intended tracked branch (normally `main`). Preserve
   local changes first; do not discard them or blindly switch branches.
2. Record `git rev-parse HEAD` as the previous commit for rollback. Review upstream
   changes, especially configuration, service packaging and state compatibility.
3. Stop the proxy and **all other commands using its state**, including login,
   refresh and quota operations, before copying a backup. For foreground mode use
   Ctrl+C and wait for exit; for systemd use `systemctl --user stop llm-proxy`;
   for Docker use `docker compose stop` and wait for any one-off commands to exit.
4. Copy the complete state directory/volume to a private location outside Git.
   It contains OAuth credentials: keep backup directories `0700`, credential files
   `0600`, and preserve ownership (Docker uses UID/GID `10001:10001`). Include
   `config.toml` and other state files, not just account metadata. Do not publish
   backups or inspect/share credential contents. Save customized service files too.

Run the following from the checkout only after confirming the branch is correct:

```sh
git pull --ff-only
git rev-parse HEAD
```

If fast-forward fails, reconcile intentionally; do not use `git reset --hard`. Record the commit ID.

## Native: rebuild and replace

The repository requires Rust **1.100.0**. With rustup installed as in the installation guide:

```sh
. "$HOME/.cargo/env"
rustup toolchain install 1.100.0 --profile minimal
cargo install --path . --locked --force --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

`--force` replaces the binary even with an unchanged version; a failed build is not an update.

### Foreground process

Restart with the same state path and intentional model/options as before, for example:

```sh
llm-proxy serve --default-model gpt-6.1-sol
```

The example model is no entitlement guarantee. Restart the process to use the new binary.

### systemd user service

Compare `packaging/systemd/llm-proxy.service` with your installed unit and drop-ins
(`systemctl --user cat llm-proxy`). Reconcile changes deliberately; **do not blindly
overwrite local customizations**. The supplied `ExecStart` fixes the model to
`gpt-6.1-sol`, overriding TOML. Preserve your intended model, environment and state
path so the service continues to see the accounts used by CLI commands.

```sh
systemctl --user daemon-reload
systemctl --user restart llm-proxy
systemctl --user status llm-proxy
```

## Docker: rebuild and recreate

With the **same Compose project/volume**, retain `.env`/exports (`LLM_PROXY_PORT`, `LLM_PROXY_MODEL`):

```sh
docker compose up -d --build
docker compose exec proxy llm-proxy accounts status
```

`start` alone does not rebuild/recreate the image. Do not use `down -v`, remove the
state volume, or switch project names accidentally: credentials must survive updates.

## Verify and roll back

Run `~/.local/bin/llm-proxy --version` (Docker: `docker compose exec proxy llm-proxy
--version`), `accounts status`, and `curl --fail http://127.0.0.1:18765/health` using
your actual host port. Status is offline without `--refresh`; health `status: "ok"`
is not the same as `ready: true`, and neither proves live provider/model access.
The current package version is `0.1.0` across commits: `--version` **cannot prove**
the new commit was installed. Check the build result, executable path, recorded
source commit and process restart (or image recreation), not just the version string.

To roll back, stop all state users again, preserve the current state privately, and
with a clean checkout use `git switch --detach PREVIOUS_COMMIT` with your recorded
commit ID. Rebuild/reinstall using the native commands above, or rebuild/recreate
with Compose and the **same** volume, then restart and verify. Do not reset away
local work. Older binaries are **not guaranteed** to read newer state; assess
compatibility before starting one or deliberately restoring a private backup.
Restoring an old backup can lose newer account changes or contain stale credentials.
Return to your intended tracked branch before the next `git pull --ff-only`.

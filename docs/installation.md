# Installation

Choose **native Ubuntu/WSL** or **Docker Compose**. Commands below are for you to
run deliberately; login contacts the provider and stores private OAuth credentials.
No provider login or inference is needed merely to build the proxy.

## Get the source

Install Git first (`sudo apt-get install -y git` on Ubuntu), then:

```sh
git clone https://github.com/nghlong3004/llm-proxy.git
cd llm-proxy
```

## Native Ubuntu / WSL

### Build and install

In Ubuntu/WSL, install prerequisites and Rust **1.100.0** (review the installer first):

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config cmake curl ca-certificates
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.100.0
. "$HOME/.cargo/env"
cargo install --path . --locked --root "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

This builds from source and installs `~/.local/bin/llm-proxy`. Add the PATH export
to `~/.bashrc` or `~/.zshrc` for new terminals. See [updating](updating.md) for an
existing installation.

### Sign in, then run in the foreground

For Codex, explicitly authorize a login and follow the printed URL/device code:

```sh
llm-proxy accounts add codex --name personal --login-method device-code
```

Alternatively, use browser login on a machine where the browser can reach the
local callback at `http://localhost:1455/auth/callback`:

```sh
llm-proxy accounts add codex --name personal --login-method browser
```

These are authenticated actions you perform, not offline installation checks.
Repeat with another name to add accounts. Login and serving must use the **same
state directory**: `--state-dir PATH` or `LLM_PROXY_STATE_DIR`; otherwise the default
is `$XDG_STATE_HOME/llm-proxy`, falling back to `~/.local/state/llm-proxy`.

```sh
llm-proxy serve --default-model gpt-6.1-sol
```

Keep this terminal open; Ctrl+C stops the proxy. The model is an example, **not a
guarantee of provider access or entitlement**. The native listener defaults to
`127.0.0.1:18765`; keep it on loopback because it has no client authentication.

Configuration lives at `<state-dir>/config.toml`. See
[config.example.toml](../config.example.toml) and [configuration](configuration.md)
for model selection, aliases and client setup. CLI flags override TOML values;
restart serving after configuration changes. Adding/removing accounts needs no restart.

### Optional systemd user service

Use this instead of the foreground process, not alongside it on the same port.
WSL must have systemd enabled and a working user manager; otherwise use foreground mode.

```sh
install -D -m 644 packaging/systemd/llm-proxy.service "$HOME/.config/systemd/user/llm-proxy.service"
systemctl --user daemon-reload
systemctl --user enable --now llm-proxy
```

The supplied unit runs `%h/.local/bin/llm-proxy serve --default-model gpt-6.1-sol`.
That fixed model overrides `config.toml`; adjust the unit or an `ExecStart` override
if necessary (a drop-in must first clear `ExecStart=`). A user service does not
inherit your terminal's environment. If login used a custom state path or
`XDG_STATE_HOME`, explicitly align the unit's `LLM_PROXY_STATE_DIR` with that path
before starting it, for example through `systemctl --user edit llm-proxy`.
After unit changes, run `systemctl --user daemon-reload` and
`systemctl --user restart llm-proxy`.

```sh
systemctl --user status llm-proxy
journalctl --user -u llm-proxy -f
systemctl --user stop llm-proxy
```

## Docker Compose

Install Docker Engine and the Docker Compose plugin using their supported
installation instructions. No host Rust installation is required. From the source
checkout, Compose builds the image locally:

```sh
docker compose up -d --build
docker compose exec proxy llm-proxy accounts add codex --name personal --login-method device-code
```

Open the printed URL in your host browser and enter the device code. This login
stores credentials in the container's persistent state, not your native state.
The image runs as UID/GID **10001:10001**. Compose mounts the named `state` volume
at `/var/lib/llm-proxy`; its private `state/` child is initialized by image copy-up
and used through `LLM_PROXY_STATE_DIR=/var/lib/llm-proxy/state`. Directories are
`0700`, credential files `0600`. Keep this ownership/permissions if restoring or
changing storage; a host bind mount does not receive the image's copy-up setup.

Only `127.0.0.1:18765` is published. The wildcard listener is **inside the container**,
not a reason to expose the unauthenticated API publicly. To change the host port,
put `LLM_PROXY_PORT=18766` in the checkout's local `.env`, or export it in every
terminal used for Compose. Reuse it for **every** later `up`, including updates;
a one-command override is not persistent. Adjust health/client URLs accordingly.
The Compose example model `gpt-6.1-sol` is not an access guarantee; set
`LLM_PROXY_MODEL` to override it.

```sh
docker compose logs -f proxy  # follow logs; Ctrl+C only exits log following
docker compose stop          # stop containers, keep state
docker compose start         # start existing stopped containers
docker compose down          # remove containers/network, keep named volume
```

**Do not use `docker compose down -v`: it deletes the state volume, including
accounts and credentials.** Keep the same Compose project/volume when updating.

AGY requires browser OAuth with `http://localhost:51121/oauth-callback`; it has no
device-code flow. The supplied Compose configuration does **not** publish that
callback, so browser login inside this container setup is unsupported. Do not
expose the callback publicly to work around this. Native AGY setup and WSL
localhost-forwarding considerations are in [Antigravity](antigravity.md).

## Verify without contacting a provider

With the server running, use another terminal:

```sh
curl --fail http://127.0.0.1:18765/health
llm-proxy accounts status  # Docker: docker compose exec proxy llm-proxy accounts status
```

Health `status: "ok"` means the HTTP process is healthy, **not** necessarily ready.
Inspect `ready` and account counts; readiness reflects local eligibility, not a
successful provider request or model entitlement. `accounts status` reads cached
metadata offline; do not add `--refresh` unless you intend an authenticated request.
Next: [configuration](configuration.md) and [updating](updating.md).

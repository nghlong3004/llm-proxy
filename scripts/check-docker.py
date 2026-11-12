#!/usr/bin/env python3
"""Exercise the local image with disposable state; never authenticate or infer.

Run after docker compose build: python3 -I scripts/check-docker.py
Only this script's uniquely named Compose project and volume are removed.
"""

import json
import os
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
import uuid


ROOT = Path(__file__).resolve().parents[1]
PROJECT = "llm-proxy-smoke-" + uuid.uuid4().hex[:12]
ENV = {**os.environ, "LLM_PROXY_PORT": "0", "LLM_PROXY_MODEL": "gpt-6.1-sol"}
COMPOSE = [
    "docker", "compose", "--env-file", "/dev/null", "--file",
    str(ROOT / "compose.yaml"), "--project-name", PROJECT,
]
# Ignore host HTTP proxy variables for requests to the disposable localhost port.
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def run(args, *, data=None):
    result = subprocess.run(
        args, input=data, text=True, capture_output=True,
        env=ENV, cwd=ROOT, timeout=120,
    )
    if result.returncode:
        raise RuntimeError(
            f"Command failed ({result.returncode}): {args}\n"
            f"{result.stdout}\n{result.stderr}"
        )
    return result.stdout.strip()


def compose(*args, data=None):
    return run(COMPOSE + list(args), data=data)


def shell(script, *, data=None):
    return compose("exec", "-T", "proxy", "sh", "-ec", script, data=data)


def request(base, route, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    req = urllib.request.Request(
        base + route, data=data, headers={"content-type": "application/json"},
    )
    try:
        response = HTTP.open(req, timeout=3)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        return response.status, json.load(response)


def start():
    compose("up", "--detach", "--no-build", "--force-recreate", "proxy")
    container = compose("ps", "--quiet", "proxy")
    details = json.loads(run(["docker", "inspect", container]))[0]
    binding = details["NetworkSettings"]["Ports"]["18765/tcp"]
    assert len(binding) == 1 and binding[0]["HostIp"] == "127.0.0.1", binding
    base = "http://127.0.0.1:" + binding[0]["HostPort"]
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        try:
            status, health = request(base, "/health")
            if status == 200:
                return container, base, health
        except (OSError, ValueError):
            pass
        time.sleep(0.1)
    raise RuntimeError("Container did not serve /health within 20 seconds")


def stop(container):
    compose("stop", "proxy")
    details = json.loads(run(["docker", "inspect", container]))[0]
    assert details["State"]["ExitCode"] == 0, details["State"]
    assert details["Config"]["StopSignal"] == "SIGINT"
    logs = compose(
        "run", "--rm", "--no-deps", "-T", "--entrypoint", "sh", "proxy", "-ec",
        'grep -q proxy_stopped "$LLM_PROXY_STATE_DIR/logs/proxy.jsonl"; '
        'if grep -q synthetic-docker-prompt "$LLM_PROXY_STATE_DIR/logs/proxy.jsonl"; '
        'then exit 1; fi; printf flushed',
    )
    assert logs == "flushed", logs


def main():
    print(f"Docker smoke project: {PROJECT}", flush=True)
    try:
        # The image owns this private state/ child before the volume is populated.
        # An unroutable TOML bind verifies that Compose's explicit CLI overrides win.
        compose(
            "run", "--rm", "--no-deps", "-T", "--entrypoint", "sh", "proxy", "-ec",
            'test "$(id -u)" = 10001; test "$(id -g)" = 10001; '
            '! command -v rustc; ! command -v cargo; '
            'test "$(stat -c %a "$LLM_PROXY_STATE_DIR")" = 700; '
            'grep -q "Raine Virta" /usr/share/licenses/llm-proxy/LICENSE; '
            'umask 077; tee "$LLM_PROXY_STATE_DIR/config.toml" >/dev/null',
            data='default_model = "gpt-5.4"\nlisten_address = "192.0.2.1"\nlisten_port = 9\n',
        )
        container, base, health = start()
        assert health["status"] == "ok" and not health["ready"], health
        assert health["accounts"] == health["usable_accounts"] == 0, health
        assert request(base, "/v1/models")[0] == 200
        payload = {
            "model": "sonnet", "messages": [
                {"role": "user", "content": "synthetic-docker-prompt"},
            ],
        }
        status, count = request(base, "/v1/messages/count_tokens", payload)
        assert status == 200 and count["input_tokens"] > 0, (status, count)
        for stream in (False, True):
            status, error = request(base, "/v1/messages", {**payload, "stream": stream})
            assert status == 503 and error["type"] == "error", (status, error)
        assert json.loads(shell("llm-proxy accounts list --output-format json"))["accounts"] == []
        permissions = shell(
            'test "$(stat -c %a "$LLM_PROXY_STATE_DIR/credentials")" = 700; '
            'test "$(stat -c %a "$LLM_PROXY_STATE_DIR/locks")" = 700; '
            'find "$LLM_PROXY_STATE_DIR" -type f -exec stat -c "%a %u %g" {} +'
        )
        assert permissions and all(line == "600 10001 10001" for line in permissions.splitlines()), permissions
        stop(container)
        print("PASS: non-root, fresh private volume, listener overrides, HTTP, SIGINT/flush", flush=True)

        account_id = str(uuid.uuid4())
        registry = {
            "version": 1, "active": {"codex": account_id}, "accounts": [{
                "provider": "codex", "id": account_id, "name": "synthetic-docker-account",
                "subject": "synthetic-subject", "workspace": "synthetic-workspace",
                "generation": str(uuid.uuid4()), "status": {"kind": "available"},
            }],
        }
        # No credential exists for this synthetic account. Never infer while it exists.
        compose(
            "run", "--rm", "--no-deps", "-T", "--entrypoint", "sh", "proxy", "-ec",
            'umask 077; tee "$LLM_PROXY_STATE_DIR/accounts.seed" >/dev/null; '
            'mv "$LLM_PROXY_STATE_DIR/accounts.seed" "$LLM_PROXY_STATE_DIR/accounts.json"',
            data=json.dumps(registry),
        )
        container, base, health = start()
        assert health["accounts"] == health["usable_accounts"] == 1, health
        listed = json.loads(shell("llm-proxy accounts list --output-format json"))
        assert listed["accounts"][0]["id"] == account_id, listed
        shell("llm-proxy accounts remove " + account_id)
        status, health = request(base, "/health")
        assert status == 200 and health["accounts"] == 0 and not health["ready"], health
        assert shell('stat -c %a "$LLM_PROXY_STATE_DIR/accounts.json"') == "600"
        stop(container)
        print("PASS: state persistence after recreate and hot account removal", flush=True)
    finally:
        compose("down", "--volumes", "--remove-orphans")
    print("PASS: Docker smoke complete; disposable containers/volume removed", flush=True)


if __name__ == "__main__":
    main()

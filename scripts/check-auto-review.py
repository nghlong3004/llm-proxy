#!/usr/bin/env python3
"""Run installed Claude Code auto mode against disposable, loopback-only mocks.

    python3 -I scripts/check-auto-review.py
    python3 -I scripts/check-auto-review.py --binary target/debug/llm-proxy

This deliberately returns an upstream error for real client classifier requests:
no safety verdict is fabricated. PASS means mock routing plus fail-closed client
behavior, never real Codex safety. Exit 2 means BLOCKED/inconclusive (including
commands allowed without review). No existing settings or credentials are read.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


ROOT = Path(__file__).resolve().parents[1]
PREFIX = "You are a security monitor for autonomous AI coding agents."
MARKER = "AUTO_REVIEW_EXECUTED"
COMMAND = "python3 -I -c 'print(\"AUTO_REVIEW_EXECUTED\")'"
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))
MAX_BODY = 8 * 1024 * 1024


def private_json(path, value):
    path.write_text(json.dumps(value))
    path.chmod(0o600)


def system_text(body):
    system = body.get("system", [])
    if isinstance(system, str):
        return [system]
    return [part.get("text", "") for part in system if isinstance(part, dict)]


def request_body(handler):
    size = int(handler.headers.get("content-length", 0))
    if not 0 <= size <= MAX_BODY:
        raise ValueError("mock body size exceeds bound")
    return json.loads(handler.rfile.read(size))


def reply(handler, status, value, content_type="application/json"):
    body = value if isinstance(value, bytes) else json.dumps(value).encode()
    handler.send_response(status)
    handler.send_header("content-type", content_type)
    handler.send_header("content-length", str(len(body)))
    handler.end_headers()
    try:
        handler.wfile.write(body)
    except (BrokenPipeError, ConnectionResetError):
        pass


def sse(handler, events):
    body = "".join("data: " + json.dumps(event) + "\n\n" for event in events).encode()
    reply(handler, 200, body, "text/event-stream")


def stop_process(process):
    if process is None:
        return
    # Also release any tool subprocess if the client hit its bounded timeout.
    try:
        os.killpg(process.pid, signal.SIGINT)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=5)


def run(binary, claude, timeout):
    ingress, native, failures = [], [], []
    observed = {
        "tool_result": False, "tool_result_error": False, "classifier_error": False,
        "command_executed": False, "tool_denied": False,
    }
    with tempfile.TemporaryDirectory(prefix="llm-proxy-auto-review-") as directory:
        root = Path(directory)
        root.chmod(0o700)
        state, home, work = root / "state", root / "home", root / "work"
        for path in (state, home, work, state / "credentials"):
            path.mkdir(mode=0o700)
        account, generation = str(uuid.uuid4()), str(uuid.uuid4())
        private_json(state / "accounts.json", {
            "version": 1, "active": {"codex": account}, "accounts": [{
                "provider": "codex", "id": account, "name": "mock-only",
                "subject": "mock-user", "workspace": "mock-workspace",
                "generation": generation, "status": {"kind": "available"},
            }],
        })
        private_json(state / "credentials" / f"{account}-{generation}.json", {
            "access_token": "mock-access-only", "refresh_token": "mock-refresh-only",
            "id_token": None, "expires_at": int(time.time()) + 3600,
            "subject": "mock-user", "account_id": "mock-workspace",
            "generation": generation,
        })

        class QuietHandler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

        class CodexMock(QuietHandler):
            def do_POST(self):
                try:
                    body = request_body(self)
                    model = body.get("model")
                    review = model == "gpt-5.5"
                    native.append({
                        "model": model, "review_route": review,
                        "has_session_id": "session-id" in self.headers,
                        "has_window_id": "x-codex-window-id" in self.headers,
                        "has_prompt_cache_key": "prompt_cache_key" in body,
                    })
                    assert self.path == "/responses"
                    assert self.headers.get("authorization") == "Bearer mock-access-only"
                    assert model in ("gpt-6.1-sol", "gpt-5.5")
                    assert len(native) <= 12
                    if review:
                        assert "session-id" not in self.headers
                        assert "x-codex-window-id" not in self.headers
                        assert "prompt_cache_key" not in body
                        # Deliberately no guessed classifier verdict or allow decision.
                        reply(self, 503, {"error": {"type": "server_error", "message": "Synthetic classifier unavailable"}})
                        return
                    results = [item for item in body.get("input", []) if item.get("type") == "function_call_output"]
                    if results:
                        observed["tool_result"] = True
                        for result in results:
                            output = result.get("output", "")
                            if not isinstance(output, str):
                                output = json.dumps(output)
                            observed["command_executed"] |= MARKER in output
                            observed["classifier_error"] |= any(word in output.lower() for word in ("classif", "safety check"))
                            observed["tool_denied"] |= any(word in output.lower() for word in (
                                "denied", "permission", "not allowed", "blocked", "not permitted", "declined",
                            ))
                        text = "MOCK_TOOL_OBSERVED" if observed["command_executed"] else "MOCK_TOOL_NOT_EXECUTED"
                        events = [{"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "delta": text}]
                    else:
                        events = [
                            {"type": "response.output_item.added", "output_index": 0, "item": {
                                "type": "function_call", "id": "fc_mock", "call_id": "call_mock", "name": "Bash", "arguments": "",
                            }},
                            {"type": "response.function_call_arguments.delta", "output_index": 0, "delta": json.dumps({
                                "command": COMMAND, "description": "Print a fixed synthetic marker", "timeout": 1000,
                            })},
                            {"type": "response.output_item.done", "output_index": 0, "item": {
                                "type": "function_call", "id": "fc_mock", "call_id": "call_mock", "name": "Bash",
                                "arguments": json.dumps({"command": COMMAND, "description": "Print a fixed synthetic marker", "timeout": 1000}),
                            }},
                        ]
                    sse(self, [{"type": "response.created", "response": {"id": "resp_mock"}}, *events, {
                        "type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 100, "output_tokens": 10}},
                    }])
                except Exception as error:
                    failures.append(type(error).__name__)
                    reply(self, 500, {"error": {"message": "mock assertion failed"}})

        mock = ThreadingHTTPServer(("127.0.0.1", 0), CodexMock)
        with socket.socket() as probe:
            probe.bind(("127.0.0.1", 0))
            port = probe.getsockname()[1]
        proxy_url = f"http://127.0.0.1:{port}"

        class Gateway(QuietHandler):
            def do_CONNECT(self):
                failures.append("external_https_attempt_blocked")
                reply(self, 403, {"error": {"message": "mock-only"}})

            def do_GET(self):
                self.forward()

            def do_POST(self):
                self.forward()

            def forward(self):
                if not self.path.startswith("/"):
                    failures.append("external_http_attempt_blocked")
                    reply(self, 403, {"error": {"message": "mock-only"}})
                    return
                try:
                    data = None
                    if self.command == "POST":
                        body = request_body(self)
                        prefix = any(text.startswith(PREFIX) for text in system_text(body))
                        for message in body.get("messages", []):
                            content = message.get("content", [])
                            if isinstance(content, list):
                                for block in content:
                                    if isinstance(block, dict) and block.get("type") == "tool_result":
                                        observed["tool_result_error"] |= block.get("is_error") is True
                        ingress.append({
                            "path": self.path.split("?", 1)[0],
                            "classifier_prefix": prefix,
                            "stream": body.get("stream", False),
                            "tool_count": len(body.get("tools", [])),
                            "has_output_schema": "output_format" in body or "format" in body.get("output_config", {}),
                        })
                        data = json.dumps(body).encode()
                    req = urllib.request.Request(proxy_url + self.path, data=data, headers={"content-type": "application/json"})
                    try:
                        response = HTTP.open(req, timeout=20)
                    except urllib.error.HTTPError as error:
                        response = error
                    with response:
                        reply(self, response.status, response.read(MAX_BODY + 1), response.headers.get("content-type", "application/json"))
                except (OSError, ValueError):
                    failures.append("gateway_forward_failed")
                    reply(self, 502, {"error": {"message": "mock gateway failed"}})

        gateway = ThreadingHTTPServer(("127.0.0.1", 0), Gateway)
        for server in (mock, gateway):
            server.daemon_threads = True
            threading.Thread(target=server.serve_forever, daemon=True).start()
        config = state / "config.toml"
        config.write_text(
            f'codex_endpoint = "http://127.0.0.1:{mock.server_port}/responses"\n'
            'default_model = "gpt-6.1-sol"\n[auto_review]\nprovider = "codex"\nmodel = "gpt-5.5"\n'
        )
        config.chmod(0o600)
        # Whitelist instead of inheriting account, host-app, cloud or settings env.
        environment = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "TERM") if key in os.environ}
        gateway_url = f"http://127.0.0.1:{gateway.server_port}"
        environment.update({
            "HOME": str(home), "CLAUDE_CONFIG_DIR": str(root / "claude-config"),
            "XDG_CONFIG_HOME": str(root / "xdg-config"), "XDG_CACHE_HOME": str(root / "xdg-cache"),
            "XDG_DATA_HOME": str(root / "xdg-data"), "XDG_STATE_HOME": str(root / "xdg-state"),
            "ANTHROPIC_BASE_URL": gateway_url, "ANTHROPIC_AUTH_TOKEN": "local-mock-only",
            "CLAUDE_CODE_AUTO_MODE_SERVER": "0", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
            "CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK": "1", "DISABLE_AUTOUPDATER": "1",
            "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1",
            "HTTP_PROXY": gateway_url, "HTTPS_PROXY": gateway_url,
            "http_proxy": gateway_url, "https_proxy": gateway_url,
            "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost",
        })
        proxy, client = None, None
        try:
            version = subprocess.run([claude, "--version"], cwd=work, env=environment, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10).stdout.strip()
            proxy_environment = {key: value for key, value in environment.items() if not key.lower().endswith("proxy")}
            proxy = subprocess.Popen([
                str(binary), "--state-dir", str(state), "serve", "--listen-port", str(port),
            ], cwd=work, env=proxy_environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
            deadline = time.monotonic() + 10
            while True:
                if proxy.poll() is not None:
                    raise RuntimeError("isolated_proxy_exited")
                try:
                    with HTTP.open(proxy_url + "/health", timeout=1) as response:
                        assert response.status == 200
                    break
                except OSError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError("isolated_proxy_health_timeout")
                    time.sleep(0.05)
            command = [
                claude, "--model", "sonnet", "--print", "--output-format", "json",
                "--permission-mode", "auto", "--no-session-persistence",
                "--setting-sources", "", "--strict-mcp-config", "--tools", "Bash", "--",
                "Run the harmless fixed Python command that prints a synthetic marker, then report whether it ran. Do not perform any other operations.",
            ]
            client = subprocess.Popen(command, cwd=work, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
            timed_out = False
            try:
                stdout, stderr = client.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                timed_out = True
                stop_process(client)
                stdout, stderr = client.communicate(timeout=5)
            # Do not print/persist response payloads, prompts or classifier bodies.
            diagnostic = (stdout + stderr).decode(errors="replace")
            startup_literals = []
            for literal in (
                "Error: Input must be provided either through stdin or as a prompt argument when using --print",
                "auto mode is unavailable for your plan",
                "auto mode unavailable for this model",
                "auto mode disabled by settings",
            ):
                if literal in diagnostic:
                    startup_literals.append(literal)
            reasons = []
            for text, code in (
                ("auto mode is not available", "auto_mode_unavailable"),
                ("Auto mode is not available", "auto_mode_unavailable"),
                ("does not support auto", "model_auto_unsupported"),
                ("no longer has a check of its own", "client_has_no_local_check"),
                ("permission mode", "permission_mode_diagnostic"),
                ("not supported", "not_supported_diagnostic"),
                ("not allowed", "not_allowed_diagnostic"),
                ("safety check", "safety_check_diagnostic"),
            ):
                if text in diagnostic:
                    reasons.append(code)
            reviews = [item for item in ingress if item["classifier_prefix"] and not item["stream"] and not item["tool_count"]]
            native_reviews = [item for item in native if item["review_route"]]
            observed["tool_denied"] |= observed["tool_result_error"] and observed["classifier_error"]
            passed = bool(
                reviews and native_reviews and observed["tool_result_error"]
                and observed["classifier_error"] and not observed["command_executed"]
                and client.returncode == 0 and not timed_out and not failures
            )
            if passed:
                reason = "classifier_routed_to_mock_error_and_command_not_executed"
            elif observed["command_executed"] and not reviews:
                reason = "harmless_command_executed_without_classifier_inconclusive"
            elif not ingress:
                reason = "client_did_not_send_requests"
            elif not reviews:
                reason = "client_did_not_send_classifier_request"
            else:
                reason = "classifier_fail_closed_not_confirmed"
            report = {
                "result": "PASS" if passed else "BLOCKED", "reason": reason,
                "client": version, "client_exit": client.returncode, "timed_out": timed_out,
                "diagnostics": sorted(set(reasons)), "startup_literals": startup_literals,
                "output_metadata": {"stdout_bytes": len(stdout), "stderr_bytes": len(stderr), "stdout_json": stdout.startswith(b"{")},
                "ingress": ingress, "native": native,
                **observed, "failures": failures, "synthetic_credentials_only": True,
                "verdict": "deliberate_upstream_error_no_allow_verdict", "live_codex_safety_verified": False,
            }
            print(json.dumps(report, sort_keys=True))
            return 0 if passed else 2
        finally:
            stop_process(client)
            stop_process(proxy)
            for server in (gateway, mock):
                server.shutdown()
                server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/llm-proxy")
    parser.add_argument("--claude", default=shutil.which("claude"))
    parser.add_argument("--timeout", type=int, default=60)
    args = parser.parse_args()
    if not args.claude or not args.binary.is_file() or not 1 <= args.timeout <= 180:
        parser.error("installed claude, built proxy and timeout 1..180 required")
    try:
        return run(args.binary.resolve(), args.claude, args.timeout)
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print(json.dumps({"result": "BLOCKED", "reason": type(error).__name__, "live_codex_safety_verified": False}))
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

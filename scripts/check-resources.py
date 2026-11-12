#!/usr/bin/env python3
"""Finite Linux resource soak of a release proxy against a localhost fake upstream.

All accounts, tokens and messages are synthetic. Never touches user state/services.
Run: python3 -I scripts/check-resources.py --seconds 180 --workers 8
"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import signal
import socket
import statistics
import subprocess
import tempfile
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parents[1]
LOCK = threading.Lock()
ACTIVE = 0
MAX_ACTIVE = 0
STOP = threading.Event()


class Upstream(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        global ACTIVE, MAX_ACTIVE
        with LOCK:
            ACTIVE += 1
            MAX_ACTIVE = max(MAX_ACTIVE, ACTIVE)
        try:
            size = int(self.headers.get("content-length", "0"))
            if not 0 < size <= 32 * 1024 * 1024:
                self.send_error(413)
                return
            body = self.rfile.read(size)
            hold = b"synthetic-hold" in body
            large = b"synthetic-large-response" in body
            # This process is not included in the proxy's /proc measurements.
            json.loads(body)
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.send_header("connection", "close")
            self.end_headers()
            self.close_connection = True
            self.event({"type": "response.created"})
            text = "x" * (128 * 1024 if large else 1024)
            for _ in range(16 if large else 4):
                self.event({"type": "response.output_text.delta", "output_index": 0, "delta": text})
                time.sleep(0.003)
            if hold:
                deadline = time.monotonic() + 20
                while not STOP.is_set() and time.monotonic() < deadline:
                    self.wfile.write(b": synthetic heartbeat\n\n")
                    self.wfile.flush()
                    time.sleep(0.05)
            self.event({"type": "response.completed", "response": {
                "usage": {"input_tokens": 12, "output_tokens": 16},
            }})
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            with LOCK:
                ACTIVE -= 1

    def event(self, event):
        self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\n")
        self.wfile.flush()


def sample(pid):
    status = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        key, _, value = line.partition(":")
        status[key] = value.strip()
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return {
        "time": round(time.monotonic(), 3),
        "rss_mib": round(int(status["VmRSS"].split()[0]) / 1024, 2),
        "peak_rss_mib": round(int(status["VmHWM"].split()[0]) / 1024, 2),
        "threads": int(status["Threads"]),
        "fds": len(list(Path(f"/proc/{pid}/fd").iterdir())),
        "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
    }


def post(port, *, stream=False, content="synthetic-normal", cancel=False, count=False):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=30)
    payload = json.dumps({
        "model": "sonnet", "stream": stream,
        "messages": [{"role": "user", "content": content}],
    })
    route = "/v1/messages/count_tokens" if count else "/v1/messages"
    try:
        connection.request("POST", route, payload, {"content-type": "application/json"})
        response = connection.getresponse()
        if response.status != 200:
            raise RuntimeError(f"{route}: unexpected HTTP {response.status}")
        if cancel:
            while True:
                line = response.readline()
                if not line:
                    raise RuntimeError("Cancelled stream ended without any content")
                if line.startswith(b"event: content_block_delta"):
                    return
        else:
            data = response.read(4 * 1024 * 1024 + 1)
            if stream:
                assert b"event: message_stop" in data and b"event: error" not in data
            elif count:
                assert json.loads(data)["input_tokens"] > 0
            else:
                assert json.loads(data)["content"][0]["text"]
    finally:
        connection.close()


def health(port):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        connection.request("GET", "/health")
        response = connection.getresponse()
        assert response.status == 200
        return json.loads(response.read())
    finally:
        connection.close()


def quiesce():
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        with LOCK:
            if ACTIVE == 0:
                return
        time.sleep(0.1)
    raise RuntimeError("Mock upstream work did not return to zero after disconnects")


def saturation(port):
    held = []
    try:
        for _ in range(32):
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
            held.append(conn)
            conn.request("POST", "/v1/messages", json.dumps({
                "model": "sonnet", "stream": True,
                "messages": [{"role": "user", "content": "synthetic-hold"}],
            }), {"content-type": "application/json"})
            response = conn.getresponse()
            assert response.status == 200
            while True:
                line = response.readline()
                if not line:
                    raise RuntimeError("Saturation stream ended before any content")
                if line.startswith(b"event: content_block_delta"):
                    break
            # Keep both connection and response alive; closing either is cancellation.
            conn.held_response = response
        for route in ("/v1/messages", "/v1/messages/count_tokens"):
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=3)
            try:
                conn.request("POST", route, "{}", {"content-type": "application/json"})
                response = conn.getresponse()
                assert response.status == 503, (route, response.status)
                response.read()
            finally:
                conn.close()
    finally:
        for conn in held:
            if hasattr(conn, "held_response"):
                conn.held_response.close()
            conn.close()
    quiesce()
    post(port)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=int, default=180)
    parser.add_argument("--workers", type=int, default=8)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/llm-proxy")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.seconds < 10 or not 1 <= args.workers <= 16:
        parser.error("Use at least 10 seconds and 1..16 workers")
    binary = args.binary.resolve()
    assert binary.is_file(), "Build the release binary first"
    upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
    upstream.daemon_threads = True
    serving = threading.Thread(target=upstream.serve_forever, daemon=True)
    serving.start()
    with tempfile.TemporaryDirectory(prefix="llm-proxy-resources-") as temporary:
        state = Path(temporary)
        state.chmod(0o700)
        (state / "credentials").mkdir(mode=0o700)
        account_id, generation = str(uuid.uuid4()), str(uuid.uuid4())
        account = {
            "provider": "codex", "id": account_id, "name": "synthetic-resource-account",
            "subject": "synthetic-subject", "workspace": "synthetic-workspace",
            "generation": generation, "status": {"kind": "available"},
        }
        files = {
            state / "accounts.json": json.dumps({"version": 1, "active": {"codex": account_id}, "accounts": [account]}),
            state / "credentials" / f"{account_id}-{generation}.json": json.dumps({
                "access_token": "synthetic-resource-access", "refresh_token": "synthetic-resource-refresh",
                "id_token": None, "expires_at": 2**63, "subject": account["subject"],
                "account_id": account["workspace"], "generation": generation,
            }),
            state / "config.toml": (
                'default_model = "gpt-5.5"\n'
                f'codex_endpoint = "http://127.0.0.1:{upstream.server_port}/responses"\n'
            ),
        }
        for path, content in files.items():
            path.write_text(content)
            path.chmod(0o600)
        # Reserve an ephemeral port briefly; only a disposable process ever uses it.
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        process = subprocess.Popen(
            [str(binary), "--state-dir", str(state), "serve", "--listen-port", str(port)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 20
            while True:
                if process.poll() is not None:
                    raise RuntimeError("Disposable proxy exited during startup")
                try:
                    if health(port)["ready"]:
                        break
                except OSError:
                    pass
                if time.monotonic() > deadline:
                    raise RuntimeError("Disposable proxy did not become ready")
                time.sleep(0.1)
            # Warm up all worker shapes, tokenizer cache, connection pool and allocator.
            with ThreadPoolExecutor(max_workers=args.workers) as pool:
                def warm(_):
                    post(port, count=True)
                    post(port, stream=True, content="synthetic-large-response")
                    post(port, content="synthetic-large-response")
                list(pool.map(warm, range(args.workers)))
            quiesce()
            baseline = sample(process.pid)
            samples = [baseline]
            counts = {"completed": 0, "cancelled": 0, "token_counts": 0}
            errors = []
            finish = time.monotonic() + args.seconds

            def worker(index):
                iteration = index
                while time.monotonic() < finish:
                    try:
                        mode = iteration % 20
                        if mode == 0:
                            post(port, count=True)
                            key = "token_counts"
                        elif mode in (1, 2):
                            post(port, stream=True, content="synthetic-hold", cancel=True)
                            key = "cancelled"
                        else:
                            post(port, stream=iteration % 2 == 0,
                                 content="synthetic-large-response" if mode == 3 else "synthetic-normal")
                            key = "completed"
                        with LOCK:
                            counts[key] += 1
                    except Exception as error:
                        with LOCK:
                            if len(errors) < 10:
                                errors.append(str(error))
                        return
                    iteration += 1
                    time.sleep(0.02)

            with ThreadPoolExecutor(max_workers=args.workers) as pool:
                futures = [pool.submit(worker, i) for i in range(args.workers)]
                while time.monotonic() < finish:
                    time.sleep(min(10, max(0, finish - time.monotonic())))
                    row = sample(process.pid)
                    samples.append(row)
                    print("SOAK " + json.dumps(row), flush=True)
                for future in futures:
                    future.result()
            assert not errors, errors
            quiesce()
            loaded = sample(process.pid)
            saturation(port)
            # A single near-input-limit request, not 32 concurrent huge bodies.
            post(port, stream=True, content="synthetic-near-limit:" + "synthetic " * (31 * 1024 * 1024 // 10))
            quiesce()
            after_large = sample(process.pid)
            idle_start = sample(process.pid)
            time.sleep(30)
            idle = sample(process.pid)
            edge = min(3, max(1, len(samples) // 3))
            growth = statistics.median(row["rss_mib"] for row in samples[-edge:]) - statistics.median(
                row["rss_mib"] for row in samples[:edge])
            elapsed = loaded["time"] - baseline["time"]
            summary = {
                "duration_seconds": args.seconds, "workers": args.workers, "requests": counts,
                "baseline": baseline, "after_soak": loaded, "after_near_limit_input": after_large,
                "after_30s_idle": idle, "rss_steady_growth_mib": round(growth, 2),
                "rss_growth_warning": growth > 32,
                "soak_cpu_percent_one_core": round(100 * (loaded["cpu_seconds"] - baseline["cpu_seconds"]) / elapsed, 2),
                "idle_cpu_percent_one_core": round(100 * (idle["cpu_seconds"] - idle_start["cpu_seconds"]) / (idle["time"] - idle_start["time"]), 2),
                "max_mock_upstream_active": MAX_ACTIVE, "mock_upstream_active_after_idle": ACTIVE,
                "samples": samples,
                "scope": "finite synthetic localhost workload; not proof of multi-day leak freedom",
            }
            assert idle["fds"] <= baseline["fds"] + 8, summary
            assert ACTIVE == 0, summary
            if args.output:
                args.output.write_text(json.dumps(summary, indent=2) + "\n")
            print("SUMMARY " + json.dumps(summary), flush=True)
        finally:
            STOP.set()
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            upstream.shutdown()
            upstream.server_close()
        assert process.returncode == 0, f"Disposable proxy exit: {process.returncode}"


if __name__ == "__main__":
    main()

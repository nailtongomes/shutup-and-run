#!/usr/bin/env python3
"""A stand-in poll URL, for testing a worker by hand. Not for production.

    python3 examples/mock-poll.py            # listens on 0.0.0.0:8080
    MOCK_PUBLIC_URL=http://203.0.113.10:8080 python3 examples/mock-poll.py

It does what a real poll URL must do: it knows a machine by the SHA-256 of
its bearer token, records an unknown one as pending and answers 403, and
only serves a machine whose hash is in approved.txt (one hash, or its first
12 characters, per line). An approved machine gets MOCK_TASKS tasks
(default 3), each `hello-world`, then 204. With MOCK_PUBLIC_URL set, each
task carries a callback to this server, which prints how the job ended.
MOCK_IMAGE, MOCK_COMMAND ("sleep 300") and MOCK_TIMEOUT change the task.

Plain HTTP: the token crosses the network in clear text. Test only.
"""
import hashlib
import json
import os
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(os.environ.get("MOCK_PORT", "8080"))
TASKS = int(os.environ.get("MOCK_TASKS", "3"))
PUBLIC = os.environ.get("MOCK_PUBLIC_URL", "").rstrip("/")
APPROVED = os.environ.get("MOCK_APPROVED_FILE", "approved.txt")
IMAGE = os.environ.get("MOCK_IMAGE", "hello-world")
COMMAND = os.environ.get("MOCK_COMMAND", "").split()
TIMEOUT = int(os.environ.get("MOCK_TIMEOUT", "60"))
RUN = format(int(time.time()), "x")  # a job_key is never reused across runs
served = {}  # token hash -> tasks handed out
seen = set()


def approved(digest):
    try:
        with open(APPROVED, encoding="utf-8") as f:
            lines = [line.strip() for line in f if line.strip()]
    except FileNotFoundError:
        return False
    return any(len(line) >= 12 and digest.startswith(line) for line in lines)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def answer(self, code, body=None):
        data = json.dumps(body).encode() if body is not None else b""
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        length = min(int(self.headers.get("Content-Length") or 0), 64 * 1024)
        raw = self.rfile.read(length)
        ip = self.client_address[0]
        if self.path.startswith("/callback"):
            print(f"CALLBACK from {ip}: {raw.decode(errors='replace')}", flush=True)
            return self.answer(200, {})
        auth = self.headers.get("Authorization", "")
        if not auth.startswith("Bearer ") or len(auth) < 23:
            print(f"NO TOKEN   from {ip}", flush=True)
            return self.answer(401)
        digest = hashlib.sha256(auth[7:].encode()).hexdigest()
        short = digest[:12]
        if not approved(digest):
            if digest not in seen:
                seen.add(digest)
                print(f"PENDING    {short} ip={ip} body={raw.decode(errors='replace')}", flush=True)
                print(f"           approve with: echo {short} >> {APPROVED}", flush=True)
            return self.answer(403)
        count = served.get(digest, 0)
        if count >= TASKS:
            return self.answer(204)
        served[digest] = count + 1
        task = {"image": IMAGE, "timeout_secs": TIMEOUT, "job_key": f"mock-{RUN}-{short}-{count + 1}"}
        if COMMAND:
            task["command"] = COMMAND
        if PUBLIC:
            task["callback_url"] = f"{PUBLIC}/callback"
        print(f"TASK {count + 1}/{TASKS}   {short} ip={ip} body={raw.decode(errors='replace')}", flush=True)
        self.answer(200, {"task": task})


if __name__ == "__main__":
    print(f"mock poll URL on 0.0.0.0:{PORT}; approved machines are read from {APPROVED}", flush=True)
    HTTPServer(("0.0.0.0", PORT), Handler).serve_forever()

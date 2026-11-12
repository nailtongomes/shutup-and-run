#!/usr/bin/env python3
"""A stand-in poll URL, for testing a worker by hand and in CI. Not for production.

    python3 tests/poll-mock/poll_mock.py                       # one hello-world task
    python3 tests/poll-mock/poll_mock.py --tasks 20 --image ghcr.io/nailtongomes/shutup-and-run-selenium-probe --timeout 180 --shm 1g

It speaks the worker's poll contract and nothing more:

  POST /poll        the worker asks; the answer is {"task": {...}} or 204
  POST /callback    the worker reports how a job ended
  POST /upload/KEY  the job's own container sends a file (the probe's screenshot)

It queues --tasks equal tasks, hands one per poll, and when every task has
called back it prints a summary and exits: 0 when the number of failures is
--expect-failures (default 0), 1 otherwise. Everything is also written under
--out: events.jsonl, summary.txt, and one KEY.png per upload.

The address the worker used for /poll is the address put in callback_url and
PROBE_UPLOAD_URL, so use one that containers can reach too: the host's LAN
address, never 127.0.0.1.

Plain HTTP: the worker's token crosses the network in clear text. Test only.
"""
import argparse
import hashlib
import json
import os
import shlex
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_BODY = 64 * 1024
MAX_UPLOAD = 20 * 1024 * 1024
# A job that has not called back this long after its own timeout is lost.
LOST_AFTER = 900


def parse_args():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--port", type=int, default=8080)
    p.add_argument("--tasks", type=int, default=1, help="how many equal tasks to queue")
    p.add_argument("--image", default="hello-world")
    p.add_argument("--command", default="", help='the container command, e.g. "sleep 5"')
    p.add_argument("--env", action="append", default=[], metavar="K=V", help="repeatable")
    p.add_argument("--timeout", type=int, default=120, help="timeout_secs of each task")
    p.add_argument("--cpu", type=float, help="cpu limit of each task")
    p.add_argument("--memory", help='memory limit of each task, e.g. "1g"')
    p.add_argument("--shm", help='/dev/shm of each task, e.g. "1g" (Chrome wants it)')
    p.add_argument("--hang", type=int, default=0, help="this many tasks get PROBE_HANG=1")
    p.add_argument("--hang-command", default="", help="and this command instead")
    p.add_argument("--expect-failures", type=int, default=0)
    p.add_argument("--require-approval", action="store_true",
                   help="serve only a token whose hash is in approved.txt")
    p.add_argument("--approved-file", default="approved.txt")
    p.add_argument("--keep-serving", action="store_true", help="do not exit after the summary")
    p.add_argument("--out", default="poll-mock-results")
    return p.parse_args()


class Mock:
    def __init__(self, args):
        self.args = args
        self.lock = threading.Lock()
        self.done = threading.Event()
        self.run = format(int(time.time()), "x")  # a job_key is never reused across runs
        self.seen = set()
        self.workers = {}
        self.peak_running = 0
        self.started = None
        self.finished = None
        self.exit_code = 0
        os.makedirs(args.out, exist_ok=True)
        self.events = open(os.path.join(args.out, "events.jsonl"), "a", encoding="utf-8")
        env = dict(item.split("=", 1) for item in args.env)
        self.tasks = []
        for n in range(1, args.tasks + 1):
            hang = n <= args.hang
            task_env = dict(env)
            if hang:
                task_env["PROBE_HANG"] = "1"
            command = args.hang_command if hang and args.hang_command else args.command
            self.tasks.append({
                "n": n, "key": f"mock-{self.run}-{n}", "state": "queued", "hang": hang,
                "env": task_env, "command": shlex.split(command),
            })

    def log(self, kind, **fields):
        self.events.write(json.dumps({"t": round(time.time(), 3), "event": kind, **fields}) + "\n")
        self.events.flush()

    def say(self, text):
        print(text, flush=True)

    def approved(self, digest):
        if not self.args.require_approval:
            return True
        try:
            with open(self.args.approved_file, encoding="utf-8") as f:
                lines = [line.strip() for line in f if line.strip()]
        except FileNotFoundError:
            return False
        return any(len(line) >= 12 and digest.startswith(line) for line in lines)

    def by_key(self, key):
        return next((t for t in self.tasks if t["key"] == key), None)

    def next_task(self, base):
        """The task for this poll, or None. Call with the lock held."""
        now = time.time()
        for t in self.tasks:
            if t["state"] == "running" and now - t["handed"] > self.args.timeout + LOST_AFTER:
                self.finish(t, {"reason": "lost", "status": "lost", "exit_code": None, "duration_secs": None})
        task = next((t for t in self.tasks if t["state"] == "queued"), None)
        if task is None:
            return None
        task["state"] = "running"
        task["handed"] = now
        self.started = self.started or now
        running = sum(1 for t in self.tasks if t["state"] == "running")
        self.peak_running = max(self.peak_running, running)
        env = dict(task["env"])
        env["PROBE_UPLOAD_URL"] = f"{base}/upload/{task['key']}"
        body = {"image": self.args.image, "timeout_secs": self.args.timeout, "job_key": task["key"],
                "callback_url": f"{base}/callback", "env": env}
        if task["command"]:
            body["command"] = task["command"]
        for name in ("cpu", "memory", "shm"):
            if getattr(self.args, name) is not None:
                body[name] = getattr(self.args, name)
        return body

    def finish(self, task, result):
        """Record how a task ended. Call with the lock held."""
        if task["state"] == "done":
            return
        task["state"] = "done"
        task["result"] = result
        task["ok"] = (result.get("status") == "exited" and result.get("reason") == "exited"
                      and result.get("exit_code") == 0)
        self.finished = time.time()
        count = sum(1 for t in self.tasks if t["state"] == "done")
        word = "ok  " if task["ok"] else "FAIL"
        self.say(f"{word} {count}/{len(self.tasks)}  {task['key']}  reason={result.get('reason')} "
                 f"exit={result.get('exit_code')} {result.get('duration_secs')}s")
        if count == len(self.tasks):
            self.summary()

    def summary(self):
        tasks = self.tasks
        failed = [t for t in tasks if not t["ok"]]
        times = [t["result"]["duration_secs"] for t in tasks if t["result"].get("duration_secs") is not None]
        reasons = {}
        for t in failed:
            r = t["result"]
            why = r.get("reason") if r.get("reason") != "exited" else f"exit {r.get('exit_code')}"
            reasons[why] = reasons.get(why, 0) + 1
        wall = (self.finished or time.time()) - (self.started or time.time())
        mem = [t["metrics"]["peak_memory_bytes"] for t in tasks if t.get("metrics", {}).get("peak_memory_bytes")]
        cpu = [t["metrics"]["cpu_seconds"] for t in tasks if t.get("metrics", {}).get("cpu_seconds") is not None]
        shots = sum(1 for t in tasks if t.get("upload"))
        lines = ["", "SUMMARY",
                 f"  tasks            {len(tasks)}  ({self.args.image})",
                 f"  ok               {len(tasks) - len(failed)}",
                 f"  failed           {len(failed)}  {reasons if reasons else ''}".rstrip(),
                 f"  at once (peak)   {self.peak_running}",
                 f"  wall time        {wall:.0f}s" + (f"  ({len(tasks) * 60 / wall:.1f} tasks/min)" if wall > 0 else "")]
        if times:
            lines.append(f"  job duration     avg {sum(times) / len(times):.1f}s  min {min(times)}s  max {max(times)}s")
        if mem:
            lines.append(f"  RAM per job      peak {max(mem) / 2**20:.0f} MiB  avg {sum(mem) / len(mem) / 2**20:.0f} MiB"
                         f"  (x{self.peak_running} at once = about {max(mem) * self.peak_running / 2**30:.1f} GiB)")
        if cpu:
            lines.append(f"  CPU per job      avg {sum(cpu) / len(cpu):.1f} cpu-seconds")
        if shots:
            lines.append(f"  files received   {shots}  in {os.path.abspath(self.args.out)}")
        for name, w in self.workers.items():
            ram = f"{w.get('memory_bytes') / 2**30:.1f} GiB" if w.get("memory_bytes") else "?"
            lines.append(f"  worker           {name}  {w.get('os')}  v{w.get('version')}  "
                         f"{w.get('cpus')} cpus  {ram}  max_slots {w.get('max_slots')}")
        text = "\n".join(lines)
        self.say(text)
        with open(os.path.join(self.args.out, "summary.txt"), "w", encoding="utf-8") as f:
            f.write(text.strip() + "\n")
        self.log("summary", ok=len(tasks) - len(failed), failed=len(failed), reasons=reasons)
        self.exit_code = 0 if len(failed) == self.args.expect_failures else 1
        self.done.set()


def handler_for(mock):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def answer(self, code, body=None):
            data = json.dumps(body).encode() if body is not None else b""
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def body(self, limit):
            length = int(self.headers.get("Content-Length") or 0)
            if length > limit:
                return None
            return self.rfile.read(length)

        def do_POST(self):
            ip = self.client_address[0]
            path = self.path.split("?", 1)[0]
            if path == "/callback":
                return self.callback(ip)
            if path.startswith("/upload/"):
                return self.upload(ip, path[len("/upload/"):])
            if path == "/poll":
                return self.poll(ip)
            self.body(MAX_BODY)
            self.answer(404)

        do_PUT = do_POST

        def poll(self, ip):
            raw = self.body(MAX_BODY)
            if raw is None:
                return self.answer(413)
            auth = self.headers.get("Authorization", "")
            if not auth.startswith("Bearer ") or len(auth) < 23:
                mock.say(f"NO TOKEN  from {ip}")
                return self.answer(401)
            digest = hashlib.sha256(auth[7:].encode()).hexdigest()
            short = digest[:12]
            try:
                info = json.loads(raw or b"{}")
            except ValueError:
                info = {}
            with mock.lock:
                if not mock.approved(digest):
                    if digest not in mock.seen:
                        mock.seen.add(digest)
                        mock.say(f"PENDING   {short} ip={ip} body={raw.decode(errors='replace')}")
                        mock.say(f"          approve with: echo {short} >> {mock.args.approved_file}")
                        mock.log("pending", token=short, ip=ip, body=info)
                    return self.answer(403)
                name = str(info.get("worker", short))
                if name not in mock.workers:
                    mock.say(f"WORKER    {name} ip={ip} token={short} body={raw.decode(errors='replace')}")
                mock.workers[name] = info
                base = "http://" + self.headers.get("Host", f"127.0.0.1:{mock.args.port}")
                task = mock.next_task(base)
                if task is None:
                    return self.answer(204)
                mock.say(f"TASK      {task['job_key']} -> {name} (free_slots {info.get('free_slots')})")
                mock.log("task", key=task["job_key"], worker=name, ip=ip, poll=info)
            self.answer(200, {"task": task})

        def callback(self, ip):
            raw = self.body(MAX_BODY)
            try:
                result = json.loads(raw or b"{}")
            except ValueError:
                return self.answer(400)
            with mock.lock:
                mock.log("callback", ip=ip, body=result)
                task = mock.by_key(str(result.get("job_key")))
                if task is None:
                    mock.say(f"CALLBACK  for an unknown job from {ip}: {raw.decode(errors='replace')}")
                else:
                    mock.finish(task, result)
            self.answer(200, {})

        def upload(self, ip, key):
            data = self.body(MAX_UPLOAD)
            with mock.lock:
                task = mock.by_key(key)  # only a key this run made up, so no path can be smuggled in
                if task is None or data is None:
                    return self.answer(404)
                path = os.path.join(mock.args.out, f"{key}.png")
                with open(path, "wb") as f:
                    f.write(data)
                task["upload"] = path
                metrics = {}
                for header, field, kind in (("X-Peak-Memory-Bytes", "peak_memory_bytes", int),
                                            ("X-Cpu-Seconds", "cpu_seconds", float),
                                            ("X-Elapsed-Seconds", "elapsed_seconds", float)):
                    try:
                        metrics[field] = kind(self.headers[header])
                    except (TypeError, ValueError):
                        pass
                task["metrics"] = metrics
                mock.say(f"FILE      {key}.png  {len(data)} bytes")
                mock.log("upload", key=key, ip=ip, bytes=len(data), **metrics)
            self.answer(200, {})

    return Handler


def main():
    args = parse_args()
    mock = Mock(args)
    server = ThreadingHTTPServer(("0.0.0.0", args.port), handler_for(mock))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    mock.say(f"poll URL: http://<this machine's address>:{args.port}/poll   "
             f"{args.tasks} task(s) of {args.image}, results in {os.path.abspath(args.out)}")
    try:
        while not mock.done.wait(1):
            pass
        while args.keep_serving:
            time.sleep(1)
        time.sleep(1)  # let the last answer leave
    except KeyboardInterrupt:
        mock.say("interrupted before every task called back")
        mock.exit_code = 1
    server.shutdown()
    sys.exit(mock.exit_code)


if __name__ == "__main__":
    main()

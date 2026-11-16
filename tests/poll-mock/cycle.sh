#!/usr/bin/env bash
# The whole pull cycle on one Linux machine with Docker: the mock offers
# tasks, a worker started here takes them, runs them and calls back, and
# status.json goes active and back to idle. Run as root (the Docker socket).
#
#   sudo bash tests/poll-mock/cycle.sh target/release/shutup-and-run
#       three alpine tasks, one of which hangs and is killed at its timeout.
#       The worker must survive that and finish the others.
#   sudo bash tests/poll-mock/cycle.sh target/release/shutup-and-run probe IMAGE
#       one task of the Selenium probe image; its screenshot must arrive.
#   sudo bash tests/poll-mock/cycle.sh target/release/shutup-and-run one
#       the whole flow with one harmless task (hello-world), from the poll that
#       delivers it to its callback. Passes only when the callback for that
#       job_key arrives with exit code 0, and the same task offered a second
#       time runs nothing. Takes about a minute; gives up after four.
set -euo pipefail

BIN="${1:?usage: cycle.sh <binary> [small|probe IMAGE|one]}"
MODE="${2:-small}"
IMAGE="${3:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
PORT=18080
# An address both this host and its containers can reach: the bridge gateway.
HOST="$(docker network inspect bridge -f '{{(index .IPAM.Config 0).Gateway}}')"

case "$MODE" in
  small)
    IMAGE=alpine
    docker pull -q alpine >/dev/null
    MOCK_ARGS=(--tasks 3 --image alpine --command true --hang 1 --hang-command "sleep 300" --timeout 5 --expect-failures 1)
    ;;
  probe)
    [[ -n "$IMAGE" ]] || { echo "probe mode needs the image name" >&2; exit 2; }
    MOCK_ARGS=(--tasks 1 --image "$IMAGE" --timeout 180 --shm 1g)
    ;;
  one)
    IMAGE=hello-world
    docker pull -q hello-world >/dev/null
    # The key an API would derive from its own task id (docs/INTEGRATION-CONTRACT.md, section 6).
    JOB_KEY=task-1042-attempt-1
    MOCK_ARGS=(--tasks 1 --image hello-world --timeout 60 --job-key "$JOB_KEY" --deadline 240 --redeliver)
    ;;
  *) echo "unknown mode $MODE" >&2; exit 2 ;;
esac

cat >"$WORK/config.toml" <<TOML
listen = "127.0.0.1:18791"
http = true
max_slots = 2
allowlist = ["${IMAGE}"]
default_timeout_secs = 60
max_timeout_secs = 600
history_path = "${WORK}/jobs.sqlite"
poll_url = "http://${HOST}:${PORT}/poll"
poll_interval_secs = 30
worker_name = "cycle-test"
TOML

fail=0
check() { if "$@"; then echo "PASS: $DESC"; else echo "FAIL: $DESC"; fail=1; fi; }
# The mock ended with exit 1 and this line.
# shellcheck disable=SC2317,SC2329
failed_saying() { [[ "$1" -eq 1 ]] && grep -q "$2" "$3"; }

MOCK_PID=""
WORKER_PID=""
# Invoked by the trap below, which some shellcheck versions do not see.
# shellcheck disable=SC2317,SC2329
cleanup() {
  if [[ -n "$WORKER_PID" ]]; then kill "$WORKER_PID" 2>/dev/null || true; fi
  if [[ -n "$MOCK_PID" ]]; then kill "$MOCK_PID" 2>/dev/null || true; fi
  # The worker writes its last status.json as it stops. Removing the folder
  # before it is gone loses that race and fails with "Directory not empty".
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

if [[ "$MODE" == one ]]; then
  # The two ways this must fail, before the run that must pass. No worker is
  # up yet: nothing polls, so the first has no callback to receive.
  code=0
  python3 "$HERE/poll_mock.py" --port "$PORT" --out "$WORK/silent" --job-key "$JOB_KEY" --deadline 2 \
    >"$WORK/silent.log" 2>&1 || code=$?
  DESC="a callback that never comes fails the run and says which job_key"
  check failed_saying "$code" "FAIL: no callback for job_key $JOB_KEY within 2 s" "$WORK/silent.log"

  python3 "$HERE/poll_mock.py" --port "$PORT" --out "$WORK/stray" --job-key "$JOB_KEY" --deadline 30 \
    >"$WORK/stray.log" 2>&1 &
  MOCK_PID=$!
  for _ in $(seq 1 50); do
    curl -fsS -o /dev/null -X POST "http://127.0.0.1:${PORT}/callback" -H 'Content-Type: application/json' \
      -d '{"id":"0","job_key":"task-9999-attempt-1","status":"exited","reason":"exited","exit_code":0}' 2>/dev/null && break
    sleep 0.2
  done
  code=0
  wait "$MOCK_PID" || code=$?
  MOCK_PID=""
  DESC="a callback with another job_key fails the run and names both keys"
  check failed_saying "$code" "FAIL: a callback arrived with job_key .task-9999-attempt-1., and this run handed out $JOB_KEY" "$WORK/stray.log"
fi

python3 "$HERE/poll_mock.py" --port "$PORT" --out "$WORK/out" "${MOCK_ARGS[@]}" &
MOCK_PID=$!

SHUTUP_AND_RUN_CONFIG="$WORK/config.toml" \
SHUTUP_AND_RUN_TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')" \
SHUTUP_AND_RUN_POLL_TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')" \
  "$BIN" serve >"$WORK/worker.log" 2>&1 &
WORKER_PID=$!

state() { sed -n 's/.*"state":"\([^"]*\)".*/\1/p' "$WORK/status.json" 2>/dev/null || true; }

saw_active=0
for _ in $(seq 1 960); do
  [[ "$(state)" == active ]] && saw_active=1
  kill -0 "$MOCK_PID" 2>/dev/null || break
  kill -0 "$WORKER_PID" 2>/dev/null || { echo "FAIL: the worker died"; cat "$WORK/worker.log"; exit 1; }
  sleep 0.5
done
if kill -0 "$MOCK_PID" 2>/dev/null; then
  echo "FAIL: the mock is still waiting after 8 minutes"
  cat "$WORK/worker.log"
  exit 1
fi
mock_code=0
wait "$MOCK_PID" || mock_code=$?
MOCK_PID=""

DESC="the mock got every callback and the result it expected (exit $mock_code)"
check test "$mock_code" -eq 0
# hello-world is gone in under a second, between two looks at the file.
if [[ "$MODE" != one ]]; then
  DESC="status.json was active while a job ran"
  check test "$saw_active" -eq 1
fi
for _ in $(seq 1 60); do [[ "$(state)" == idle ]] && break; sleep 0.5; done
DESC="status.json is back to idle (now: $(state))"
check test "$(state)" = idle
DESC="the worker is still running"
check kill -0 "$WORKER_PID"

if [[ "$MODE" == one ]]; then
  DESC="the callback for $JOB_KEY arrived with exit code 0"
  check python3 -c '
import json, sys
for line in open(sys.argv[1], encoding="utf-8"):
    event = json.loads(line)
    body = event.get("body") or {}
    if event["event"] == "callback" and body.get("job_key") == sys.argv[2] and body.get("exit_code") == 0:
        sys.exit(0)
sys.exit(1)' "$WORK/out/events.jsonl" "$JOB_KEY"
  DESC="one task, one success"
  check grep -Eq "^ +ok +1$" "$WORK/out/summary.txt"
  DESC="the second offer of $JOB_KEY started nothing (the worker logged it as already recorded)"
  check grep -q "poll task already recorded" "$WORK/worker.log"
elif [[ "$MODE" == small ]]; then
  DESC="the hung task was killed at its timeout and counted as a failure"
  check grep -q "timeout_kill" "$WORK/out/summary.txt"
  DESC="the other two tasks ran after it"
  check grep -Eq "^ +ok +2$" "$WORK/out/summary.txt"
else
  shot="$(find "$WORK/out" -name '*.png' -size +5k | head -n 1)"
  DESC="a screenshot arrived (${shot:-none})"
  check test -n "$shot"
  if [[ -n "$shot" && -n "${CYCLE_KEEP:-}" ]]; then cp "$shot" "$CYCLE_KEEP"; fi
fi

if [[ "$fail" -ne 0 ]]; then
  echo "--- worker log"; cat "$WORK/worker.log"
  echo "--- mock events"; cat "$WORK/out/events.jsonl" 2>/dev/null || true
fi
exit "$fail"

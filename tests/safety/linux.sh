#!/usr/bin/env bash
# Adverse conditions against a real worker and a real Docker, on Linux, under
# the shared-PC profile. Nothing here stresses the machine: the worker is made
# to see a small computer by being pinned to two processors, and a job runs
# out of memory inside its own 16 MiB limit. Run as root (the Docker socket).
#
#   sudo bash tests/safety/linux.sh target/release/shutup-and-run
#
# It starts its own worker on a temporary config and history, with random
# tokens, and removes everything on exit. Not part of CI: the last step stops
# new jobs for fifteen minutes (the breaker), which only a fresh history ends.
set -euo pipefail

BIN="$(readlink -f "${1:?usage: linux.sh <binary>}")"
WORK="$(mktemp -d)"
PORT=18793
URL="http://127.0.0.1:${PORT}"
TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"

cat >"$WORK/config.toml" <<TOML
listen = "127.0.0.1:${PORT}"
http = true
max_slots = 2
allowlist = ["alpine"]
default_timeout_secs = 120
max_timeout_secs = 600
history_path = "${WORK}/jobs.sqlite"
resource_profile = "shared-pc"
min_free_memory = "64m"
min_free_disk = "64m"
TOML
export SHUTUP_AND_RUN_CONFIG="$WORK/config.toml" SHUTUP_AND_RUN_TOKEN="$TOKEN"

WORKER_PID=""
# shellcheck disable=SC2317,SC2329
cleanup() {
  if [[ -n "$WORKER_PID" ]]; then kill "$WORKER_PID" 2>/dev/null || true; fi
  wait 2>/dev/null || true
  docker ps -aq --filter label=shutup-and-run=1 | xargs -r docker rm -f >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

fail=0
check() { if "$@"; then echo "PASS: $DESC"; else echo "FAIL: $DESC"; fail=1; fi; }
# shellcheck disable=SC2317,SC2329
is() { [[ "$1" == "$2" ]] || { echo "      got: $1"; echo "   wanted: $2"; return 1; }; }

start_worker() { # the processors it may see, as a taskset list
  taskset -c "$1" "$BIN" serve >>"$WORK/worker.log" 2>&1 &
  WORKER_PID=$!
  for _ in $(seq 1 100); do
    curl -fsS -o /dev/null -H "Authorization: Bearer $TOKEN" "$URL/health" 2>/dev/null && return 0
    sleep 0.1
  done
  echo "the worker did not come up"; cat "$WORK/worker.log"; exit 1
}
stop_worker() { kill "$WORKER_PID"; wait "$WORKER_PID" 2>/dev/null || true; WORKER_PID=""; }
push() { curl -sS -o "$WORK/answer.json" -w '%{http_code}' -X POST "$URL/v1/jobs" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$1"; }
answer() { jq -r "$1" "$WORK/answer.json"; }
safety() { "$BIN" safety --json | jq -r "$1"; }
job() { curl -sS -H "Authorization: Bearer $TOKEN" "$URL/v1/jobs/$1"; }
wait_end() { # job id -> its JobResult once it has ended
  for _ in $(seq 1 240); do
    job "$1" >"$WORK/job.json"
    [[ "$(jq -r .status "$WORK/job.json")" == exited ]] && return 0
    sleep 0.5
  done
  echo "job $1 did not end"; return 1
}
settle() { sleep 1.5; } # one turn of the status loop

docker pull -q alpine >/dev/null
SLEEP='{"image":"alpine","command":["sleep","1"]}'

echo "== 1. a small computer: two processors =="
start_worker 0,1
settle
DESC="the worker sees a small machine and waits for its owner"
check is "$(safety '.safety.admission + " " + .safety.needs_authorization[0]')" \
  "authorization_required SAFETY_SMALL_MACHINE"
DESC="a push is answered 503 AUTHORIZATION_REQUIRED and nothing starts"
check is "$(push "$SLEEP") $(answer .code) $(answer '.reasons[0].code')" \
  "503 AUTHORIZATION_REQUIRED SAFETY_SMALL_MACHINE"
DESC="no container was created"
check is "$(docker ps -aq --filter label=shutup-and-run=1 | wc -l)" 0
DESC="status.json says why the worker waits"
check grep -q '"waiting_for":"the owner of this computer has to authorize: SAFETY_SMALL_MACHINE' "$WORK/status.json"

echo "== 2. nobody but a person at a terminal authorizes =="
code=0; "$BIN" safety authorize --json >"$WORK/cli.json" || code=$?
DESC="--json: exit 1, AUTHORIZATION_REQUIRED, a hint, and nothing changed"
check is "$code $(jq -r '.code + " " + (.hint | startswith("Do not answer for the owner") | tostring)' "$WORK/cli.json")" \
  "1 AUTHORIZATION_REQUIRED true"
code=0; echo AUTHORIZE | "$BIN" safety authorize >"$WORK/cli.txt" 2>&1 || code=$?
DESC="the word piped in without a terminal: exit 1, nothing changed"
check is "$code $(safety .safety.authorization)" "1 null"
DESC="a wrong word typed at a terminal is a no"
script -qec "$BIN safety authorize" /dev/null <<<"yes" >"$WORK/tty-no.txt" 2>&1 || true
check is "$(safety .safety.authorization)" "null"
DESC="the notice was shown, with the risk and what the yes does not change"
check grep -q "do not remove it" "$WORK/tty-no.txt"
DESC="the exact word typed at a terminal is recorded, with where and for what"
script -qec "$BIN safety authorize" /dev/null <<<"AUTHORIZE" >"$WORK/tty-yes.txt" 2>&1 || true
check is "$(safety '.safety.authorization | .channel + " v" + (.notice_version|tostring) + " " + (.codes|join(","))')" \
  "terminal v1 SAFETY_SMALL_MACHINE"
settle
DESC="with the owner's yes the same push is accepted and the job ends well"
check is "$(push "$SLEEP")" 202
wait_end "$(answer .id)"
DESC="that job exited 0"
check is "$(jq -r '(.exit_code|tostring) + " " + .reason' "$WORK/job.json")" "0 exited"
DESC="the doctor reports the warning as authorized, and exits 0"
"$BIN" doctor --json >"$WORK/doctor.json" || true
check is "$(jq -r '.checks[] | select(.name=="safety") | .status + " " + (.detail | contains("authorized by the owner") | tostring)' "$WORK/doctor.json")" "ok true"
DESC="withdrawn: the question is back"
"$BIN" safety revoke --json >/dev/null; settle
check is "$(push "$SLEEP") $(answer .code)" "503 AUTHORIZATION_REQUIRED"
stop_worker

echo "== 3. the same history on four processors: healthy, no question =="
start_worker 0-3
settle
DESC="a machine with room is healthy and takes the job with no authorization"
check is "$(safety '.safety.level + " " + .safety.admission') $(push "$SLEEP")" "healthy open 202"
wait_end "$(answer .id)"

echo "== 4. a job held at its CPU limit =="
DESC="a busy loop on a fifth of a processor ends well and is recorded as throttled"
# shellcheck disable=SC2016
push '{"image":"alpine","cpu":0.2,"timeout_secs":120,"command":["sh","-c","end=$(( $(date +%s) + 40 )); while [ $(date +%s) -lt $end ]; do :; done"]}' >/dev/null
wait_end "$(answer .id)"; settle
check is "$(jq -r .exit_code "$WORK/job.json") $(safety '[.safety.incidents[] | select(.kind=="cpu_throttled")] | length')" "0 1"
safety '.safety.incidents[] | select(.kind=="cpu_throttled") | "      " + .detail'

echo "== 5. Docker goes away under a running job, and comes back =="
push '{"image":"alpine","command":["sleep","300"]}' >/dev/null
RUNNING="$(answer .id)"
for _ in $(seq 1 40); do docker ps -q --filter "name=shutup-and-run-$RUNNING" | grep -q . && break; sleep 0.25; done
DOCKERD="$(pgrep -x dockerd || true)"
if [[ -n "$DOCKERD" && "$(ps -o comm= -p 1)" != systemd ]]; then
  kill "$DOCKERD"; while kill -0 "$DOCKERD" 2>/dev/null; do sleep 0.2; done
  sleep 2
  DESC="with Docker down a push fails cleanly (502) and the worker stays up"
  check is "$(push "$SLEEP") $(kill -0 "$WORKER_PID" && echo alive)" "502 alive"
  DESC="status.json says failed, and why"
  check grep -q '"state":"failed"' "$WORK/status.json"
  (nohup dockerd >"$WORK/dockerd.log" 2>&1 &)
  for _ in $(seq 1 60); do docker info >/dev/null 2>&1 && break; sleep 0.5; done
  sleep 3
  DESC="with Docker back the worker takes jobs again, without a restart"
  check is "$(push "$SLEEP")" 202
  wait_end "$(answer .id)"
  DESC="the job that was running is closed, once, and not started again"
  wait_end "$RUNNING"
  check is "$(jq -r .status "$WORK/job.json") $(docker ps -q --filter "name=shutup-and-run-$RUNNING" | wc -l)" "exited 0"
  echo "      it ended as: $(jq -c '{reason, exit_code}' "$WORK/job.json")"
else
  echo "SKIP: dockerd is managed by systemd here; not stopping it"
  docker kill "shutup-and-run-$RUNNING" >/dev/null
  wait_end "$RUNNING"
fi

echo "== 6. a plain kill is not an OOM =="
push '{"image":"alpine","command":["sleep","300"]}' >/dev/null
KILLED="$(answer .id)"
for _ in $(seq 1 40); do docker ps -q --filter "name=shutup-and-run-$KILLED" | grep -q . && break; sleep 0.25; done
docker kill "shutup-and-run-$KILLED" >/dev/null
wait_end "$KILLED"
DESC="docker kill: exit 137, and no oom_killed key"
check is "$(jq -r '(.exit_code|tostring) + " " + (has("oom_killed")|tostring)' "$WORK/job.json")" "137 false"

echo "== 7. a job that runs out of its own 16 MiB, three times =="
OOM='{"image":"alpine","memory":"16m","command":["sh","-c","tail /dev/zero"]}'
for n in 1 2 3; do
  DESC="OOM $n: accepted"
  check is "$(push "$OOM")" 202
  wait_end "$(answer .id)"
  DESC="OOM $n: exit 137, oom_killed true"
  check is "$(jq -r '(.exit_code|tostring) + " " + .reason + " " + (.oom_killed|tostring)' "$WORK/job.json")" "137 exited true"
  settle
  DESC="OOM $n: one incident for it, not two"
  check is "$(safety '[.safety.incidents[] | select(.kind=="oom")] | length')" "$n"
done
DESC="jobs says why, in words"
check is "$("$BIN" jobs --json | jq -r '.jobs[0] | (.oom_killed|tostring) + " " + (.failure | contains("ran out of memory") | tostring)')" "true true"
DESC="after three, a new job is refused: 503 SAFETY_BLOCKED, SAFETY_OOM_BREAKER"
check is "$(push "$SLEEP") $(answer .code) $(answer '.reasons[0].code')" "503 SAFETY_BLOCKED SAFETY_OOM_BREAKER"
DESC="an authorization does not lift it: nothing to authorize, still blocked"
script -qec "$BIN safety authorize" /dev/null <<<"AUTHORIZE" >/dev/null 2>&1 || true
check is "$(push "$SLEEP") $(answer .code)" "503 SAFETY_BLOCKED"
DESC="the doctor warns with the breaker's code and still exits 0 (the worker runs)"
code=0; "$BIN" doctor --json >"$WORK/doctor.json" || code=$?
check is "$code $(jq -r '.checks[] | select(.name=="safety") | .status + " " + .code' "$WORK/doctor.json")" "0 warn SAFETY_OOM_BREAKER"
DESC="the worker is alive, with no container left behind"
check is "$(kill -0 "$WORKER_PID" && echo alive) $(docker ps -q --filter label=shutup-and-run=1 | wc -l)" "alive 0"

echo "== incidents recorded =="
safety '.safety.incidents[] | "  " + (.at|todate) + " " + .kind + " " + .detail'
if [[ "$fail" -ne 0 ]]; then echo "--- worker log"; tail -n 60 "$WORK/worker.log"; fi
exit "$fail"

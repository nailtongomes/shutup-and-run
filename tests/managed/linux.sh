#!/usr/bin/env bash
# Managed containers against a real worker and a real Docker, on Linux:
# the two modes, an interval, an always container that crashes, one that is
# killed from outside, a worker killed with SIGKILL, stop, and the secret.
# Run as root (the Docker socket). It takes about four minutes: the waits of
# the backoff are the real ones.
#
#   sudo bash tests/managed/linux.sh target/release/shutup-and-run
#
# It starts its own worker on a temporary config and history, with random
# tokens, and removes everything on exit. Not part of CI.
set -euo pipefail

BIN="$(readlink -f "${1:?usage: linux.sh <binary>}")"
WORK="$(mktemp -d)"
PORT=18794
URL="http://127.0.0.1:${PORT}"
TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
SECRET="s3cr3t-$(head -c 8 /dev/urandom | od -An -tx1 | tr -d ' \n')"

cat >"$WORK/config.toml" <<TOML
listen = "127.0.0.1:${PORT}"
http = true
status_page = true
max_slots = 3
allowlist = ["alpine"]
default_timeout_secs = 120
max_timeout_secs = 600
history_path = "${WORK}/jobs.sqlite"
resource_profile = "none"
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
is() { [[ "$1" == "$2" ]] || { echo "      got: $1"; echo "   wanted: $2"; return 1; }; }

start_worker() {
  "$BIN" serve >>"$WORK/worker.log" 2>&1 &
  WORKER_PID=$!
  for _ in $(seq 1 100); do
    curl -fsS -o /dev/null -H "Authorization: Bearer $TOKEN" "$URL/health" 2>/dev/null && return 0
    sleep 0.1
  done
  echo "the worker did not come up"; cat "$WORK/worker.log"; exit 1
}
push() { curl -sS -o "$WORK/answer.json" -w '%{http_code}' -X POST "$URL/v1/jobs" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$1"; }
managed() { "$BIN" managed "$@" --json; }
state() { managed list | jq -r --arg n "$1" '.containers[] | select(.name == $n) | .state'; }
field() { managed list | jq -r --arg n "$1" ".containers[] | select(.name == \$n) | $2"; }
ours() { docker ps -q --filter label=shutup-and-run=1 | wc -l | tr -d ' '; }
until_state() { # name, state, seconds
  for _ in $(seq 1 "$3"); do [[ "$(state "$1")" == "$2" ]] && return 0; sleep 1; done
  echo "      $1 is $(state "$1"), not $2"; return 1
}
keeps() { docker ps --filter label=shutup-and-run=1 --format '{{.Command}}' | grep -c 'sleep 500' || true; }
until_keeps() { # how many `keep` containers Docker should show, seconds
  for _ in $(seq 1 "$2"); do [[ "$(keeps)" == "$1" ]] && return 0; sleep 1; done
  echo "      docker shows $(keeps) of them, not $1"; return 1
}
apply() { echo "$1" | "$BIN" managed apply - --json >"$WORK/apply.json"; }

start_worker

DESC="jobs mode is the default and a push runs"
check is "$(push '{"image":"alpine","command":["sleep","8"]}') $(managed mode | jq -r .mode)" "202 jobs"

apply '{"name":"keep","image":"alpine","command":["sleep","500"],"policy":"always","enabled":true,
        "env":{"PLAIN":"1"},"secret_env":{"API_TOKEN":"'"$SECRET"'"},"cpu":0.5,"memory":"64m","shm":"32m"}'
apply '{"name":"beat","image":"alpine","command":["sh","-c","echo beat; sleep 2"],
        "policy":"interval","interval_secs":60,"enabled":true}'
apply '{"name":"crash","image":"alpine","command":["sh","-c","exit 3"],"policy":"always","enabled":true}'
apply '{"name":"report","image":"alpine","command":["true"],"policy":"once","enabled":true}'

DESC="a definition with an image outside the allowlist is refused with exit 3, and nothing is saved"
set +e
echo '{"name":"evil","image":"busybox","policy":"once"}' | "$BIN" managed apply - --json >"$WORK/evil.json"
code=$?
set -e
check is "$code $(jq -r .code "$WORK/evil.json") $(managed list | jq '.containers | length')" "3 MANAGED_INVALID 4"

DESC="the mode does not change while the pushed job runs"
set +e
managed mode on >"$WORK/busy.json"; code=$?
set -e
check is "$code $(jq -r .code "$WORK/busy.json") $(managed mode | jq -r .mode)" "1 MANAGED_BUSY jobs"

DESC="in jobs mode no managed container started"
sleep 12
check is "$(keeps)" "0"

DESC="idle: managed mode turns on"
for _ in $(seq 1 30); do [[ "$(ours)" == 0 ]] && break; sleep 1; done
check is "$(managed mode on | jq -r .mode)" "managed"

DESC="in managed mode a push is refused with 409 MANAGED_MODE, and health says the mode"
check is "$(push '{"image":"alpine"}') $(jq -r .code "$WORK/answer.json") $(curl -sS -H "Authorization: Bearer $TOKEN" "$URL/health" | jq -r .mode)" "409 MANAGED_MODE managed"

DESC="the always container is running, with its limits and its secret in the container only"
check until_state keep running 20
# `running` is from the moment the run is accepted; the image is checked first.
until_keeps 1 30
KEEP="$(field keep .running_job)"
inspect() { docker inspect "shutup-and-run-$KEEP" --format "$1"; }
check is "$(inspect '{{.HostConfig.NanoCpus}} {{.HostConfig.Memory}} {{.HostConfig.ShmSize}}')" "500000000 67108864 33554432"
DESC="the container has the secret"
check is "$(inspect '{{range .Config.Env}}{{println .}}{{end}}' | grep -c "API_TOKEN=$SECRET")" "1"

DESC="once does not run by itself, and runs when started"
check is "$(state report) $(field report '.last')" "idle null"
managed start report >/dev/null
for _ in $(seq 1 20); do [[ "$(field report '.last.status')" == ok ]] && break; sleep 1; done
check is "$(field report '.last.status')" "ok"

DESC="the interval container ran once and is scheduled, not running twice"
for _ in $(seq 1 20); do [[ "$(field beat '.last.status')" == ok ]] && break; sleep 1; done
check is "$(field beat '.last.status') $(state beat)" "ok scheduled"

DESC="a worker killed with SIGKILL comes back to the same container, and starts no second one"
kill -9 "$WORKER_PID"; wait "$WORKER_PID" 2>/dev/null || true; WORKER_PID=""
start_worker
sleep 12
check is "$(field keep .running_job) $(keeps)" "$KEEP 1"
DESC="managed mode is still on after the restart"
check is "$(managed mode | jq -r .mode)" "managed"

DESC="killed from outside, the always container is started again, once"
sleep 45 # past a minute of life: a run that lasted
docker kill "shutup-and-run-$KEEP" >/dev/null
for _ in $(seq 1 60); do
  now="$(field keep .running_job)"; [[ "$now" != null && "$now" != "$KEEP" ]] && break; sleep 1
done
until_keeps 1 30
sleep 15 # and no second one a pass later
check is "$([[ "$now" != "$KEEP" && "$now" != null ]] && echo new) $(keeps)" "new 1"

DESC="the crashing container was tried a few times in two minutes, not in a loop"
tries="$("$BIN" jobs --limit 200 --json | jq '[.jobs[] | select(.exit_code == 3)] | length')"
check test "$tries" -ge 2 -a "$tries" -le 4
echo "      crash: $tries runs, $(field crash .failures) failures in a row, state $(state crash)"

DESC="the interval container ran again after its interval"
runs="$("$BIN" jobs --limit 200 --json | jq '[.jobs[] | select(.status == "ok" and .duration_secs >= 2)] | length')"
check test "$runs" -ge 2

DESC="stop kills it and it stays stopped"
managed stop keep >/dev/null
check until_state keep stopped 20
sleep 40
check is "$(state keep) $(keeps)" "stopped 0"

DESC="the secret is in no output, log or status file"
"$BIN" managed list >"$WORK/list.txt"; "$BIN" managed show keep >"$WORK/show.txt"
curl -sS "$URL/local/managed" >"$WORK/page.html"
check is "$(cat "$WORK"/worker.log "$WORK"/list.txt "$WORK"/show.txt "$WORK"/page.html "$WORK"/status.json "$WORK"/apply.json | grep -c "$SECRET" || true)" "0"
DESC="the page lists the containers"
check grep -q 'Managed mode is ON' "$WORK/page.html"

DESC="idle again: back to jobs mode, and a push runs"
managed disable crash >/dev/null; managed disable beat >/dev/null
for _ in $(seq 1 30); do [[ "$(ours)" == 0 ]] && break; sleep 1; done
check is "$(managed mode off | jq -r .mode) $(push '{"image":"alpine","command":["true"]}')" "jobs 202"

if [[ "$fail" == 0 ]]; then echo "managed containers: all passed"; else echo "managed containers: FAILED"; grep -v status-file "$WORK/worker.log" | tail -40; fi
exit "$fail"

#!/usr/bin/env bash
# A server's daily cycle against a real Docker, scaled down for CI: a push
# of an image that is not here (409 image_not_present, no Retry-After), the
# image prepared with POST /v1/images/pull, 40 pushes at once to 6 slots
# (6 accepted, the rest 429 with reason and Retry-After), the same 40 again
# (the 6 answer 200 with the same ids, nothing new starts), DELETE of each,
# then the cleanup: refused while a job runs, and once idle it removes an
# anonymous volume and keeps a named one. Run as root (the Docker socket).
#
#   sudo bash tests/server/burst.sh target/release/shutup-and-run
#
# It starts its own worker on a temporary config and history, with a random
# token, and removes everything on exit. alpine:latest comes from Docker Hub
# when it answers, else from the machine.
set -euo pipefail

BIN="$(readlink -f "${1:?usage: burst.sh <binary>}")"
WORK="$(mktemp -d)"
PORT=18796
URL="http://127.0.0.1:${PORT}"
TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"
SLOTS=6
PUSHES=40
KEEP="sar-burst-keep-$$"
ANON=""

cat >"$WORK/config.toml" <<TOML
listen = "127.0.0.1:${PORT}"
http = true
resource_profile = "server"
max_slots = ${SLOTS}
max_job_memory = "64m"
min_free_memory = "256m"
min_free_disk = "1g"
default_job_shm = "0"
allowlist = ["alpine"]
history_path = "${WORK}/jobs.sqlite"
TOML
export SHUTUP_AND_RUN_CONFIG="$WORK/config.toml" SHUTUP_AND_RUN_TOKEN="$TOKEN"

WORKER_PID=""
# shellcheck disable=SC2317,SC2329
cleanup() {
  if [[ -n "$WORKER_PID" ]]; then kill "$WORKER_PID" 2>/dev/null || true; fi
  wait 2>/dev/null || true
  docker ps -aq --filter label=shutup-and-run=1 | xargs -r docker rm -f >/dev/null 2>&1 || true
  docker volume rm "$KEEP" >/dev/null 2>&1 || true
  if [[ -n "$ANON" ]]; then docker volume rm "$ANON" >/dev/null 2>&1 || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

fail=0
check() { if "$@"; then echo "PASS: $DESC"; else echo "FAIL: $DESC"; fail=1; fi; }
# shellcheck disable=SC2329 # called through check
is() { [[ "$1" == "$2" ]] || { echo "      got: $1"; echo "   wanted: $2"; return 1; }; }
api() { curl -sS -H "Authorization: Bearer $TOKEN" "$@"; }
# One push: "STATUS RETRY_AFTER BODY" in $WORK/$2.line.
push() { # body, out
  local code retry
  code="$(api -o "$WORK/$2.json" -D "$WORK/$2.head" -w '%{http_code}' \
    -H 'Content-Type: application/json' -d "$1" "$URL/v1/jobs")"
  retry="$(tr -d '\r' <"$WORK/$2.head" | awk -F': ' 'tolower($1)=="retry-after"{print $2}')"
  echo "$code ${retry:--} $(cat "$WORK/$2.json")" >"$WORK/$2.line"
}
used() { api "$URL/health" | jq -r .slots_used; }
wait_idle() {
  for _ in $(seq 1 120); do [[ "$(used)" == 0 ]] && return 0; sleep 0.5; done
  return 1
}

if ! docker pull -q alpine:latest >/dev/null 2>&1; then
  docker image inspect alpine:latest >/dev/null || { echo "FAIL: no alpine:latest"; exit 1; }
  echo "NOTE: docker pull failed; the preparation will use the local alpine:latest."
fi
docker image rm alpine:sar-absent >/dev/null 2>&1 || true

"$BIN" serve >>"$WORK/worker.log" 2>&1 &
WORKER_PID=$!
for _ in $(seq 1 100); do
  curl -fsS -o /dev/null -H "Authorization: Bearer $TOKEN" "$URL/health" 2>/dev/null && break
  sleep 0.1
done

push '{"image":"alpine:sar-absent","command":["true"]}' absent
read -r code retry _ <"$WORK/absent.line"
DESC="an image that is not here is refused with 409 image_not_present, no Retry-After"
check is "$code $retry $(jq -r .reason "$WORK/absent.json")" "409 - image_not_present"

answer="$(api -H 'Content-Type: application/json' -d '{"image":"alpine:latest"}' "$URL/v1/images/pull")"
DESC="POST /v1/images/pull starts the preparation"
check is "$(jq -r .state <<<"$answer")" pulling
state=""
for _ in $(seq 1 240); do
  state="$(api "$URL/v1/images/pull?image=alpine:latest" | jq -r .state)"
  [[ "$state" != pulling ]] && break
  sleep 0.5
done
DESC="the preparation ends ready"
check is "$state" ready
DESC="/metrics says when the image was prepared"
check is "$(api "$URL/metrics" | grep -c '^shutup_and_run_image_prepared_timestamp_seconds{image="alpine:latest"}')" 1

burst() { # round
  local i pids=()
  mkdir -p "$WORK/$1"
  for i in $(seq 1 "$PUSHES"); do
    push "{\"image\":\"alpine:latest\",\"command\":[\"sleep\",\"60\"],\"job_key\":\"burst-$i\"}" "$1/$i" &
    pids+=("$!")
  done
  # Not a bare wait: the worker is a child too.
  wait "${pids[@]}"
}
count() { cat "$WORK/$1"/*.line | awk -v c="$2" '$1==c' | wc -l; }
burst r1
DESC="$PUSHES pushes at once to $SLOTS slots: $SLOTS accepted"
check is "$(count r1 202)" "$SLOTS"
DESC="the others are 429 no_slots with Retry-After 15"
check is "$(cat "$WORK"/r1/*.line | awk '$1==429 && $2==15' | grep -c '"reason":"no_slots"')" "$((PUSHES - SLOTS))"
DESC="/health: every slot used"
check is "$(used)" "$SLOTS"
ids1="$(cat "$WORK"/r1/*.json | jq -r 'select(.id) | .id' | sort)"

burst r2
DESC="the same pushes again: the $SLOTS jobs answer 200, nothing new is accepted"
check is "$(count r2 200) $(count r2 202)" "$SLOTS 0"
ids2="$(cat "$WORK"/r2/*.json | jq -r 'select(.id) | .id' | sort)"
DESC="with the same ids"
check is "$ids2" "$ids1"
DESC="Docker runs at most $SLOTS containers of the worker (5 start at once, max_starting)"
check test "$(docker ps -q --filter label=shutup-and-run=1 | wc -l)" -le "$SLOTS"

for id in $ids1; do api -o /dev/null -X DELETE "$URL/v1/jobs/$id"; done
DESC="DELETE of each frees every slot"
check wait_idle

# A container someone created and removed by hand left an anonymous volume.
docker volume ls -q >"$WORK/before"
docker rm "$(docker create -v /data alpine:latest true)" >/dev/null
ANON="$(docker volume ls -q | grep -vxFf "$WORK/before" | head -1 || true)"
docker volume create "$KEEP" >/dev/null
push '{"image":"alpine:latest","command":["sleep","60"]}' busy
running_id="$(jq -r .id "$WORK/busy.json")"
code="$(api -o "$WORK/clean.json" -w '%{http_code}' -X POST "$URL/v1/maintenance/cleanup")"
DESC="the cleanup is refused while a job runs (409 JOBS_RUNNING)"
check is "$code $(jq -r .code "$WORK/clean.json")" "409 JOBS_RUNNING"
api -o /dev/null -X DELETE "$URL/v1/jobs/$running_id"
wait_idle || true
code="$(api -o "$WORK/clean.json" -w '%{http_code}' -X POST "$URL/v1/maintenance/cleanup")"
DESC="once idle the cleanup runs (200) and removes volumes"
check is "$code $(jq -r '.volumes >= 1' "$WORK/clean.json")" "200 true"
DESC="there was an anonymous volume to remove"
check test -n "$ANON"
DESC="the anonymous volume is gone"
check is "$(docker volume ls -q | grep -cxF "${ANON:-none}" || true)" 0
DESC="the named volume stays"
check is "$(docker volume ls -q | grep -cxF "$KEEP" || true)" 1
DESC="the prepared image stays"
check is "$(docker image inspect -f ok alpine:latest 2>/dev/null)" ok

if [[ "$fail" != 0 ]]; then
  echo "--- worker log"
  grep -v status-file "$WORK/worker.log" | tail -n 60
  exit 1
fi
echo "server burst: all passed"

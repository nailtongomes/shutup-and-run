#!/usr/bin/env bash
# Smoke test for an installed, running shutup-and-run. It only uses the HTTP API,
# so it can run from the machine itself or from another one.
#
#   SMOKE_URL=https://203.0.113.10:8787 SMOKE_TOKEN=... bash scripts/smoke.sh
#   SMOKE_URL=... SMOKE_TOKEN=... bash scripts/smoke.sh preflight   # scenario 12 only
#   SMOKE_URL=... SMOKE_TOKEN=... bash scripts/smoke.sh 1 2 6       # chosen scenarios
#
# On the machine itself, as root, SMOKE_LOCAL=1 fills in the URL, the token,
# the fingerprint and the ban lift command from /etc/shutup-and-run:
#
#   sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh
#
# Required (unless SMOKE_LOCAL=1):
#   SMOKE_URL            base URL, http or https
#   SMOKE_TOKEN          the bearer token
#
# Optional:
#   SMOKE_FINGERPRINT    SHA-256 certificate fingerprint the installer printed.
#                        When set, the certificate is checked and its key is
#                        pinned for every request. When unset, TLS is not verified.
#   SMOKE_IMAGE          an image on the instance's allowlist. Default
#                        ghcr.io/nailtongomes/shutup-and-run-smoke, a small Python
#                        image built from examples/smoke-image. Any image with
#                        sh, echo and sleep works. hello-world works too, but it
#                        has no shell, so the scenarios that need a job to stay
#                        up (3, 4, 8, 10, 12) are skipped.
#   SMOKE_STUB_HOST      address the instance can use to reach this machine, for
#                        the callback and poll stub. Default 127.0.0.1 when
#                        SMOKE_URL is a loopback address. Otherwise unset, and
#                        the scenarios that need it are skipped.
#   SMOKE_STUB_PORT      port of that stub. Default 18790.
#   SMOKE_STUB_BIND      address the stub binds. Default 127.0.0.1, or 0.0.0.0
#                        when SMOKE_STUB_HOST is set to something else.
#   SMOKE_RESTART_CMD    command that restarts the service, for scenario 8.
#                        Example: "sudo systemctl restart shutup-and-run"
#   SMOKE_BAN_LIFT_CMD   command that lifts a ban; the address is appended.
#                        Example: "sudo shutup-and-run bans lift"
#   SMOKE_BAN_SOURCE     source address for the ban test. Default 127.0.0.2.
#   SMOKE_POLL=1         the instance has poll_url = http://<stub host>:<stub port>/poll
#   SMOKE_POLL_WAIT      seconds to wait for the instance to poll. Default 90.
#   SMOKE_STRICT=1       a skipped scenario counts as a failure.
#
# What it does to a live machine, so you can decide when to run it:
#   - Every job is the tiny image running echo or sleep, with timeout_secs of
#     at most 120. There is no cancel call in the API, so cleanup is waiting:
#     the script ends when its own jobs have ended.
#   - Scenario 3 takes every free slot for a few seconds. Callers get 429.
#   - Scenario 7 drains for about a second. Callers get 503. It always undrains.
#   - Scenario 8 runs only when you give SMOKE_RESTART_CMD.
#   - Slot counts are compared with the count at the start. Real jobs that
#     start or end during the run can make a scenario fail.
#   - It leaves a few finished rows in the job history. Purge removes them.
#
# It does not ban the caller. Scenario 9 sends its wrong tokens from
# SMOKE_BAN_SOURCE (127.0.0.2), which only works when SMOKE_URL is a loopback
# address, and lifts that ban afterwards. Run from another machine, scenario 9
# is skipped. Scenario 1 sends one request without a token: from 127.0.0.2 when
# it can, otherwise from your address. One failure is below the default limit
# of three and the next request clears it. If the instance has
# auth_failures = 1, set SMOKE_SKIP_UNAUTH=1.
set -uo pipefail

if [[ "${SMOKE_LOCAL:-}" == 1 ]]; then
  etc=/etc/shutup-and-run
  port=$(sed -n 's/^listen = ".*:\([0-9][0-9]*\)"$/\1/p' "$etc/config.toml" 2>/dev/null | head -n 1)
  : "${SMOKE_URL:=https://127.0.0.1:${port:-8787}}"
  : "${SMOKE_TOKEN:=$(sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' "$etc/env" 2>/dev/null | head -n 1)}"
  : "${SMOKE_FINGERPRINT:=$(openssl x509 -in "$etc/cert.pem" -noout -fingerprint -sha256 2>/dev/null | sed 's/^[^=]*=//')}"
  : "${SMOKE_BAN_LIFT_CMD:=shutup-and-run bans lift}"
fi
URL="${SMOKE_URL:-}"
TOKEN="${SMOKE_TOKEN:-}"
SMOKE_DEFAULT_IMAGE=ghcr.io/nailtongomes/shutup-and-run-smoke
IMAGE="${SMOKE_IMAGE:-$SMOKE_DEFAULT_IMAGE}"
# hello-world is one static binary. It cannot run a command we choose.
SHELL_OK=1
[[ "${IMAGE%%:*}" == hello-world ]] && SHELL_OK=0
STUB_PORT="${SMOKE_STUB_PORT:-18790}"
BAN_SOURCE="${SMOKE_BAN_SOURCE:-127.0.0.2}"
POLL_WAIT="${SMOKE_POLL_WAIT:-90}"

if [[ -z "$URL" || -z "$TOKEN" ]]; then
  echo "set SMOKE_URL and SMOKE_TOKEN" >&2
  exit 2
fi
URL="${URL%/}"
for tool in curl python3; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "$tool is required" >&2
    exit 2
  fi
done

ONLY=" $* "
[[ "$ONLY" == "  " ]] && ONLY=" 1 2 3 4 5 6 7 8 9 10 11 12 "
ONLY="${ONLY/ preflight / 12 }"

TMP=$(mktemp -d)
RUN="smoke-$(date +%s)-$$"
PASSED=0
FAILED=0
SKIPPED=0
STUB_PID=
DRAINED=0
BANNED=0
USED0=0
TOTAL=0

pass() { echo "PASS: $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL: $1"; FAILED=$((FAILED + 1)); }
skip() {
  echo "SKIP: $1"
  SKIPPED=$((SKIPPED + 1))
  [[ "${SMOKE_STRICT:-}" == 1 ]] && FAILED=$((FAILED + 1))
}
wants() { [[ "$ONLY" == *" $1 "* ]]; }

hostport="${URL#*://}"
hostport="${hostport%%/*}"
host="${hostport%:*}"
LOOPBACK=0
[[ "$host" == 127.* ]] && LOOPBACK=1

STUB_HOST="${SMOKE_STUB_HOST:-}"
[[ -z "$STUB_HOST" && "$LOOPBACK" == 1 ]] && STUB_HOST=127.0.0.1
STUB_BIND="${SMOKE_STUB_BIND:-}"
if [[ -z "$STUB_BIND" ]]; then
  STUB_BIND=127.0.0.1
  [[ -n "$STUB_HOST" && "$STUB_HOST" != 127.* ]] && STUB_BIND=0.0.0.0
fi

# TLS. With a fingerprint: fetch the certificate, compare, and pin its public
# key, which also works when the certificate does not name the address in the URL.
TLS=()
if [[ "$URL" == https://* ]]; then
  TLS=(-k)
  if [[ -n "${SMOKE_FINGERPRINT:-}" ]]; then
    if ! command -v openssl >/dev/null 2>&1; then
      echo "openssl is required to check SMOKE_FINGERPRINT" >&2
      exit 2
    fi
    [[ "$hostport" == *:* ]] || hostport="${hostport}:443"
    openssl s_client -connect "$hostport" </dev/null 2>/dev/null | openssl x509 -outform PEM >"$TMP/cert.pem" 2>/dev/null
    seen=$(openssl x509 -in "$TMP/cert.pem" -noout -fingerprint -sha256 2>/dev/null | sed 's/^[^=]*=//')
    want=$(printf '%s' "$SMOKE_FINGERPRINT" | tr '[:lower:]' '[:upper:]')
    if [[ -z "$seen" || "$seen" != "$want" ]]; then
      echo "FAIL: certificate fingerprint is ${seen:-unreadable}, expected ${want}"
      exit 1
    fi
    pin=$(openssl x509 -in "$TMP/cert.pem" -noout -pubkey | openssl pkey -pubin -outform DER 2>/dev/null | openssl dgst -sha256 -binary | base64)
    TLS=(-k --pinnedpubkey "sha256//${pin}")
    echo "certificate fingerprint matches; key pinned"
  else
    echo "NOTE: SMOKE_FINGERPRINT is unset; TLS is not verified"
  fi
fi

# api METHOD PATH [JSON]. Sets CODE and writes the body to $TMP/body.
api() {
  local method="$1" path="$2" data="${3:-}"
  local args=(-sS -m 180 -o "$TMP/body" -w '%{http_code}' -X "$method" -H "Authorization: Bearer ${TOKEN}")
  [[ -n "$data" ]] && args+=(-H 'Content-Type: application/json' --data "$data")
  CODE=$(curl "${TLS[@]}" "${args[@]}" "${URL}${path}" 2>"$TMP/curl.err") || CODE=000
}

# code_from SOURCE_ADDRESS TOKEN. Prints the status of GET /health. Empty token sends no header.
code_from() {
  local args=(-sS -m 10 -o /dev/null -w '%{http_code}')
  [[ -n "$1" ]] && args+=(--interface "$1")
  [[ -n "$2" ]] && args+=(-H "Authorization: Bearer $2")
  curl "${TLS[@]}" "${args[@]}" "${URL}/health" 2>/dev/null || echo 000
}

# field KEY [FILE]. Prints one top-level JSON field, empty when missing or null.
field() {
  python3 -c '
import json, sys
try:
    value = json.load(open(sys.argv[2])).get(sys.argv[1])
except Exception:
    value = None
if value is True: print("true")
elif value is False: print("false")
elif value is not None: print(value)
' "$1" "${2:-$TMP/body}"
}

used() { api GET /health; field slots_used; }

# sh_job SHELL_COMMAND TIMEOUT [EXTRA_JSON_FIELDS]. POSTs a job. Sets CODE and ID.
# An image without a shell runs its own entrypoint and the command is dropped.
sh_job() {
  local extra="${3:-}" command="\"command\":[\"sh\",\"-c\",\"$1\"],"
  [[ "$SHELL_OK" == 1 ]] || command=
  api POST /v1/jobs "{\"image\":\"${IMAGE}\",${command}\"timeout_secs\":$2${extra:+,$extra}}"
  ID=$(field id)
}

# needs_shell NUMBER NAME. Skips the scenario and returns 1 when the image has no shell.
needs_shell() {
  [[ "$SHELL_OK" == 1 ]] && return 0
  skip "$1 $2 (${IMAGE} has no shell; use an image with sh and sleep)"
  return 1
}

# wait_log ID TEXT SECONDS. Returns 0 once the job's logs contain TEXT, which
# means the image was pulled and the container is up.
wait_log() {
  local deadline=$((SECONDS + $3))
  while ((SECONDS < deadline)); do
    api GET "/v1/jobs/$1?logs=true"
    [[ "$(field logs)" == *"$2"* ]] && return 0
    sleep 0.5
  done
  return 1
}

# wait_exit ID SECONDS. Returns 0 once the job is exited; the row is in $TMP/body.
wait_exit() {
  local deadline=$((SECONDS + $2))
  while ((SECONDS < deadline)); do
    api GET "/v1/jobs/$1"
    [[ "$CODE" == 200 && "$(field status)" == exited ]] && return 0
    sleep 0.5
  done
  return 1
}

# wait_used COUNT SECONDS. Returns 0 once slots_used is at most COUNT.
wait_used() {
  local deadline=$((SECONDS + $2)) now
  while ((SECONDS < deadline)); do
    now=$(used)
    [[ -n "$now" ]] && ((now <= $1)) && return 0
    sleep 1
  done
  return 1
}

# Reachable from the EXIT trap. Shellcheck does not count that as a call.
# shellcheck disable=SC2317,SC2329
cleanup() {
  if [[ "$DRAINED" == 1 ]]; then
    api POST /v1/drain '{"draining":false}'
    [[ "$CODE" == 200 ]] || echo "WARNING: could not undrain (http ${CODE}). POST /v1/drain {\"draining\":false} by hand."
  fi
  if [[ "$BANNED" == 1 && -n "${SMOKE_BAN_LIFT_CMD:-}" ]]; then
    bash -c "${SMOKE_BAN_LIFT_CMD} ${BAN_SOURCE}" </dev/null >/dev/null 2>&1 || echo "WARNING: could not lift the ban on ${BAN_SOURCE}"
  fi
  if [[ "$TOTAL" != 0 ]]; then
    if wait_used "$USED0" 130; then
      echo "cleanup: smoke jobs have ended (slots_used=${USED0})"
    else
      echo "WARNING: slots_used is still above ${USED0}; smoke jobs end at their timeout_secs (120 at most)"
    fi
  fi
  if [[ -n "$STUB_PID" ]]; then
    kill "$STUB_PID" >/dev/null 2>&1 || true
    wait "$STUB_PID" >/dev/null 2>&1 || true
  fi
  rm -rf "$TMP"
}
trap cleanup EXIT

# One small HTTP server for scenarios 10, 11 and 12.
#   POST /callback/<anything>  appends the body, one line, to $TMP/callbacks
#   POST /poll                 hands out $TMP/poll.task once, then 204
start_stub() {
  [[ -n "$STUB_PID" ]] && return 0
  : >"$TMP/callbacks"
  : >"$TMP/polls"
  python3 -c '
import os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
tmp, bind, port = sys.argv[1], sys.argv[2], int(sys.argv[3])
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        data = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if self.path.startswith("/callback/"):
            with open(os.path.join(tmp, "callbacks"), "ab") as f:
                f.write(data.replace(b"\n", b" ") + b"\n")
            self.send_response(204); self.end_headers(); return
        if self.path == "/poll":
            task = os.path.join(tmp, "poll.task")
            try:
                os.rename(task, task + ".served")
                body = open(task + ".served", "rb").read()
                kind = b"task\n"
            except OSError:
                body, kind = b"", b"empty\n"
            with open(os.path.join(tmp, "polls"), "ab") as f:
                f.write(kind)
            if body:
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers(); self.wfile.write(body)
            else:
                self.send_response(204); self.end_headers()
            return
        self.send_response(404); self.end_headers()
    def log_message(self, fmt, *args):
        return
ThreadingHTTPServer((bind, port), H).serve_forever()
' "$TMP" "$STUB_BIND" "$STUB_PORT" </dev/null &
  STUB_PID=$!
  sleep 0.5
  if ! kill -0 "$STUB_PID" 2>/dev/null; then
    STUB_PID=
    return 1
  fi
}

# callbacks_for ID. Prints how many callback bodies name this job.
callbacks_for() { grep -c "\"id\":\"$1\"" "$TMP/callbacks" 2>/dev/null || true; }

# wait_callback ID. Returns 0 when exactly one callback for ID arrived and it is the final row.
wait_callback() {
  local deadline=$((SECONDS + 15))
  while ((SECONDS < deadline)); do
    [[ "$(callbacks_for "$1")" -ge 1 ]] && break
    sleep 0.5
  done
  # The launcher tries three times within a second. Wait past that to count duplicates.
  sleep 2
  [[ "$(callbacks_for "$1")" == 1 ]] || return 1
  grep "\"id\":\"$1\"" "$TMP/callbacks" >"$TMP/callback.json"
  [[ "$(field status "$TMP/callback.json")" == exited && "$(field reason "$TMP/callback.json")" == exited && "$(field exit_code "$TMP/callback.json")" == 0 ]]
}

echo "== shutup-and-run smoke: ${URL}, image ${IMAGE} =="

# A service that was just restarted needs a moment. A wrong token would be
# 401 at once, so only a refused connection is retried.
for _ in $(seq 1 20); do
  api GET /health
  [[ "$CODE" != 000 ]] && break
  sleep 0.5
done
if [[ "$CODE" != 200 ]]; then
  echo "FAIL: GET /health with the token returned ${CODE}. $(cat "$TMP/curl.err" 2>/dev/null)"
  [[ "$CODE" == 403 ]] && echo "403 with an empty body means this address is banned: shutup-and-run bans lift <ip>"
  [[ "$CODE" == 401 ]] && echo "401 is a wrong token. Each wrong token counts toward a ban of your address."
  exit 1
fi
if [[ "$(field draining)" == true ]]; then
  echo "FAIL: the instance is draining, so it refuses every job. Undrain it first. Nothing was changed."
  exit 1
fi
TOTAL=$(field slots_total)
USED0=$(field slots_used)
echo "slots: ${USED0} of ${TOTAL} in use before the run"

# The poll stub answers from the start, so the instance does not back off
# while the earlier scenarios run.
if [[ -n "$STUB_HOST" ]] && { wants 10 || wants 11 || wants 12; }; then
  start_stub || echo "NOTE: could not start the stub on ${STUB_BIND}:${STUB_PORT}"
fi

# Source for requests that are meant to fail authentication.
ALT=
if [[ "$LOOPBACK" == 1 && "$(code_from "$BAN_SOURCE" "$TOKEN")" == 200 ]]; then
  ALT=$BAN_SOURCE
fi

if wants 1; then
  api GET /health
  ok=0
  [[ "$CODE" == 200 && -n "$(field slots_total)" && -n "$(field slots_used)" && -n "$(field slots_free)" ]] && ok=1
  if [[ "${SMOKE_SKIP_UNAUTH:-}" == 1 ]]; then
    if [[ "$ok" == 1 ]]; then
      pass "1 health returns slot counts (the request without a token was skipped)"
    else
      fail "1 health (http ${CODE})"
    fi
  else
    anon=$(code_from "$ALT" "")
    # A success clears the single failure that request just recorded.
    code_from "$ALT" "$TOKEN" >/dev/null
    if [[ "$ok" == 1 && "$anon" == 401 ]]; then
      pass "1 health returns slot counts; 401 without a token"
    else
      fail "1 health (with token ${CODE}, without token ${anon})"
    fi
  fi
fi

if wants 2; then
  sh_job "echo ${RUN}-two; sleep 3" 60
  first=$CODE
  id=$ID
  api GET "/v1/jobs/${id}"
  running=$(field status)
  # hello-world can be gone before the first GET.
  [[ "$SHELL_OK" == 0 && "$running" == exited ]] && running=running
  expect="${RUN}-two"
  [[ "$SHELL_OK" == 1 ]] || expect="Hello from Docker"
  if [[ "$first" == 202 && "$running" == running ]] && wait_exit "$id" 60 && [[ "$(field exit_code)" == 0 ]]; then
    api GET "/v1/jobs/${id}?logs=true"
    if [[ "$(field logs)" == *"$expect"* ]]; then
      pass "2 job accepted, running, exited 0, logs returned"
    else
      fail "2 job ran but ?logs=true did not return its output"
    fi
  else
    [[ "$first" == 403 ]] && echo "NOTE: 403 means ${IMAGE} is not on the instance's allowlist. Set SMOKE_IMAGE."
    [[ "$(field reason)" == start_failed ]] && echo "NOTE: the job did not start. The instance could not pull or run ${IMAGE}."
    fail "2 job (post ${first}, first status ${running:-none}, last http ${CODE}, exit_code $(field exit_code))"
  fi
fi

if wants 3 && needs_shell 3 slots; then
  wait_used "$USED0" 60
  api GET /health
  free=$(field slots_free)
  if [[ -z "$free" || "$free" -lt 1 ]]; then
    fail "3 slots (no free slot to start from)"
  else
    long=$((20 + 2 * free))
    refused=0
    for ((i = 1; i < free; i++)); do
      sh_job "sleep ${long}" $((long + 30))
      [[ "$CODE" == 202 ]] || refused=$((refused + 1))
    done
    sh_job "sleep 5" 60
    [[ "$CODE" == 202 ]] || refused=$((refused + 1))
    short=$ID
    sh_job "sleep 1" 30
    full=$CODE
    if [[ "$refused" == 0 && "$full" == 429 ]] && wait_exit "$short" 60; then
      sh_job "sleep 1" 30
      if [[ "$CODE" == 202 ]]; then
        pass "3 ${free} slots filled, then 429, then 202 after one job ended"
      else
        fail "3 slots (after a job ended the next POST returned ${CODE})"
      fi
    else
      fail "3 slots (${refused} of ${free} fill jobs were not 202, POST when full ${full})"
    fi
    wait_used "$USED0" $((long + 40))
  fi
fi

if wants 4 && needs_shell 4 timeout; then
  wait_used "$USED0" 60
  sh_job "sleep 100" 2
  id=$ID
  if [[ "$CODE" == 202 ]] && wait_exit "$id" 40 && [[ "$(field reason)" == timeout_kill ]]; then
    pass "4 job past timeout_secs was killed, reason timeout_kill"
  else
    fail "4 timeout (post ${CODE}, status $(field status), reason $(field reason))"
  fi
fi

if wants 5; then
  wait_used "$USED0" 60
  api POST /v1/jobs '{"image":"smoke-not-allowlisted.invalid/nope:1","command":["true"],"timeout_secs":10}'
  denied=$CODE
  if [[ "$denied" == 403 && "$(used)" == "$USED0" ]]; then
    pass "5 image off the allowlist is 403 and nothing started"
  else
    fail "5 allowlist (post ${denied}, slots_used $(used), expected ${USED0})"
  fi
fi

if wants 6; then
  wait_used "$USED0" 60
  sh_job "sleep 2" 30 "\"job_key\":\"${RUN}-key\""
  c1=$CODE
  id1=$ID
  sh_job "sleep 2" 30 "\"job_key\":\"${RUN}-key\""
  if [[ "$c1" == 202 && "$CODE" == 200 && -n "$id1" && "$ID" == "$id1" ]]; then
    pass "6 same job_key is 202 then 200 with the same id"
  else
    fail "6 job_key (codes ${c1} ${CODE}, ids ${id1:-none} ${ID:-none})"
  fi
fi

if wants 7; then
  wait_used "$USED0" 60
  DRAINED=1
  api POST /v1/drain '{"draining":true}'
  d1=$CODE
  sh_job "sleep 1" 30
  refused=$CODE
  api POST /v1/drain '{"draining":false}'
  d2=$CODE
  [[ "$d2" == 200 ]] && DRAINED=0
  sh_job "sleep 1" 30
  if [[ "$d1" == 200 && "$refused" == 503 && "$d2" == 200 && "$CODE" == 202 ]]; then
    pass "7 drain is 503 for new jobs; undrain is 202 again"
  else
    fail "7 drain (drain ${d1}, post ${refused}, undrain ${d2}, post ${CODE})"
  fi
fi

if wants 8 && needs_shell 8 restart; then
  if [[ -z "${SMOKE_RESTART_CMD:-}" ]]; then
    skip "8 restart (set SMOKE_RESTART_CMD, for example \"sudo systemctl restart shutup-and-run\")"
  else
    wait_used "$USED0" 60
    sh_job "echo ${RUN}-eight" 30
    done_id=$ID
    wait_exit "$done_id" 30
    sh_job "echo ${RUN}-up; sleep 12" 60
    live_id=$ID
    # A job that is still pulling its image has no container yet, and a
    # restart would end it. Wait until it is really up.
    wait_log "$live_id" "${RUN}-up" 60
    bash -c "$SMOKE_RESTART_CMD" </dev/null >/dev/null 2>"$TMP/restart.err"
    up=0
    for _ in $(seq 1 60); do
      api GET /health
      [[ "$CODE" == 200 ]] && up=1 && break
      sleep 0.5
    done
    during=$(field slots_used)
    api GET "/v1/jobs/${done_id}?logs=true"
    kept=0
    [[ "$CODE" == 200 && "$(field status)" == exited && "$(field exit_code)" == 0 && "$(field logs)" == *"${RUN}-eight"* ]] && kept=1
    api GET "/v1/jobs/${live_id}"
    still=$(field status)
    if [[ "$up" == 1 && "$kept" == 1 && "$still" == running && "$during" == $((USED0 + 1)) ]] &&
      wait_exit "$live_id" 60 && [[ "$(field exit_code)" == 0 ]] && wait_used "$USED0" 20; then
      pass "8 restart kept history, kept the running job, and freed its slot when it ended"
    else
      fail "8 restart (up ${up}, history kept ${kept}, running job ${still:-none}, slots_used ${during:-?} then $(used), expected $((USED0 + 1)) then ${USED0}) $(cat "$TMP/restart.err" 2>/dev/null)"
    fi
  fi
fi

if wants 9; then
  if [[ -z "$ALT" ]]; then
    skip "9 ban (needs a loopback SMOKE_URL so the wrong tokens can come from ${BAN_SOURCE}, not from you)"
  elif [[ -z "${SMOKE_BAN_LIFT_CMD:-}" ]]; then
    skip "9 ban (set SMOKE_BAN_LIFT_CMD, for example \"sudo shutup-and-run bans lift\")"
  else
    BANNED=1
    bad=
    for _ in 1 2 3; do
      bad+="$(code_from "$ALT" "not-the-token-${RUN}") "
    done
    blocked=$(code_from "$ALT" "$TOKEN")
    mine=$(code_from "" "$TOKEN")
    bash -c "${SMOKE_BAN_LIFT_CMD} ${ALT}" </dev/null >/dev/null 2>"$TMP/lift.err"
    restored=$(code_from "$ALT" "$TOKEN")
    [[ "$restored" == 200 ]] && BANNED=0
    if [[ "$bad" == "401 401 401 " && "$blocked" == 403 && "$mine" == 200 && "$restored" == 200 ]]; then
      pass "9 three wrong tokens from ${ALT} is 403 even with the right token; bans lift restores it"
    else
      fail "9 ban (wrong tokens ${bad}, then ${blocked}, caller ${mine}, after lift ${restored}) $(cat "$TMP/lift.err" 2>/dev/null)"
    fi
  fi
fi

if wants 10 && needs_shell 10 poll; then
  if [[ "${SMOKE_POLL:-}" != 1 ]]; then
    skip "10 poll (set SMOKE_POLL=1 when the instance has poll_url = http://<this machine>:${STUB_PORT}/poll)"
  elif [[ -z "$STUB_PID" ]]; then
    fail "10 poll (the stub is not running on ${STUB_BIND}:${STUB_PORT})"
  else
    wait_used "$USED0" 60
    : >"$TMP/polls"
    printf '{"task":{"image":"%s","command":["sleep","100"],"timeout_secs":120,"job_key":"%s-poll"}}' "$IMAGE" "$RUN" >"$TMP/poll.task.new"
    mv "$TMP/poll.task.new" "$TMP/poll.task"
    served=0
    deadline=$((SECONDS + POLL_WAIT))
    while ((SECONDS < deadline)); do
      grep -q '^task$' "$TMP/polls" && served=1 && break
      sleep 0.5
    done
    started=0
    if [[ "$served" == 1 ]]; then
      for _ in $(seq 1 40); do
        [[ "$(used)" == $((USED0 + 1)) ]] && started=1 && break
        sleep 0.5
      done
    fi
    # The same key again must be the job the poll started, not a new one.
    sh_job "sleep 100" 120 "\"job_key\":\"${RUN}-poll\""
    replay=$CODE
    poll_id=$ID
    # Two more polls must come back empty and leave the count alone.
    : >"$TMP/polls"
    # A worker polls every 30 seconds at the fastest, so two polls take a
    # minute. The job sleeps for longer than that, to still hold its slot.
    deadline=$((SECONDS + 100))
    while ((SECONDS < deadline)); do
      [[ "$(grep -c '^empty$' "$TMP/polls")" -ge 2 ]] && break
      sleep 0.5
    done
    empties=$(grep -c '^empty$' "$TMP/polls")
    after=$(used)
    rm -f "$TMP/poll.task"
    if [[ "$served" == 1 && "$started" == 1 && "$replay" == 200 && "$empties" -ge 2 && "$after" == $((USED0 + 1)) ]]; then
      pass "10 poll took one task, started exactly one container, then got empty answers"
    else
      fail "10 poll (task served ${served}, container started ${started}, same key ${replay}, empty polls ${empties}, slots_used ${after:-?}, expected $((USED0 + 1)))"
    fi
    [[ -n "$poll_id" ]] && wait_exit "$poll_id" 130
  fi
fi

if wants 11; then
  if [[ -z "$STUB_HOST" ]]; then
    skip "11 callback (set SMOKE_STUB_HOST to an address the instance can use to reach this machine)"
  elif [[ -z "$STUB_PID" ]]; then
    fail "11 callback (the stub is not running on ${STUB_BIND}:${STUB_PORT})"
  else
    wait_used "$USED0" 60
    sh_job "echo ${RUN}-eleven" 30 "\"callback_url\":\"http://${STUB_HOST}:${STUB_PORT}/callback/${RUN}\""
    id=$ID
    if [[ "$CODE" == 202 ]] && wait_exit "$id" 30 && wait_callback "$id"; then
      pass "11 callback_url received the final job JSON once"
    else
      fail "11 callback (post ${CODE}, callbacks received for this job: $(callbacks_for "$id"))"
    fi
  fi
fi

# Preflight: one harmless job with every request field set, followed from POST
# to the freed slot. Run it alone with: smoke.sh preflight
if wants 12 && needs_shell 12 preflight; then
  wait_used "$USED0" 60
  extra="\"job_key\":\"${RUN}-preflight\",\"env\":{\"SMOKE_MARK\":\"${RUN}-preflight\"},\"cpu\":0.5,\"memory\":\"64m\",\"shm\":\"16m\""
  with_callback=0
  if [[ -n "$STUB_HOST" && -n "$STUB_PID" ]]; then
    with_callback=1
    extra+=",\"callback_url\":\"http://${STUB_HOST}:${STUB_PORT}/callback/${RUN}\""
  fi
  if [[ "$IMAGE" == "$SMOKE_DEFAULT_IMAGE"* ]]; then
    # No command: the image's own entrypoint runs, as a real worker's would.
    # It prints "python ok <version> <SMOKE_MARK>" and stays up two seconds.
    api POST /v1/jobs "{\"image\":\"${IMAGE}\",\"timeout_secs\":60,${extra}}"
    ID=$(field id)
    said="python ok"
  else
    # shellcheck disable=SC2016
    sh_job 'echo preflight $SMOKE_MARK; sleep 2' 60 "$extra"
    said="preflight"
  fi
  first=$CODE
  id=$ID
  taken=$(used)
  ok=0
  why="post ${first}"
  [[ "$first" == 403 ]] && why+=": ${IMAGE} is not on the instance's allowlist, set SMOKE_IMAGE"
  if [[ "$first" == 202 && -n "$id" ]]; then
    why="slots_used ${taken:-?} while running, expected $((USED0 + 1))"
    if [[ "$taken" == $((USED0 + 1)) ]]; then
      why="job did not exit 0"
      if wait_exit "$id" 60 && [[ "$(field exit_code)" == 0 && "$(field reason)" == exited ]]; then
        api GET "/v1/jobs/${id}?logs=true"
        why="logs do not show the env value"
        if [[ "$(field logs)" == *"$said"* && "$(field logs)" == *"${RUN}-preflight"* ]]; then
          sh_job "true" 60 "\"job_key\":\"${RUN}-preflight\""
          why="same job_key returned ${CODE}"
          if [[ "$CODE" == 200 && "$ID" == "$id" ]]; then
            why="slot was not freed"
            if wait_used "$USED0" 20; then
              why="callback did not arrive exactly once"
              if [[ "$with_callback" == 0 ]] || wait_callback "$id"; then
                ok=1
              fi
            fi
          fi
        fi
      fi
    fi
  fi
  if [[ "$ok" == 1 ]]; then
    note="env, cpu, memory, shm, job_key"
    [[ "$with_callback" == 1 ]] && note+=", callback"
    pass "12 preflight: one job end to end (${note}), slot freed"
  else
    fail "12 preflight (${why})"
  fi
fi

echo
echo "RESULT: ${PASSED} passed, ${FAILED} failed, ${SKIPPED} skipped"
[[ "$FAILED" -eq 0 ]]

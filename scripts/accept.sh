#!/usr/bin/env bash
# End-to-end check for shutup-and-run on an Ubuntu machine with Docker.
#
#   sudo bash scripts/accept.sh
#   sudo bash scripts/accept.sh /path/to/shutup-and-run
#   sudo SHUTUP_AND_RUN_SMOKE=1 bash scripts/accept.sh   # then scripts/smoke.sh on the same instance
#
# Do not run this on a machine that is already serving jobs. Test containers
# use the label shutup-and-run=1 and count against every shutup-and-run on this Docker.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=18787
CALLBACK_PORT=18788
BASE="http://127.0.0.1:${PORT}"
FAILURES=0
MODE=none
CB_PID=
IDS=()
WORK=

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); }

dump_logs() {
  if [[ "$MODE" == systemd ]]; then
    journalctl -u shutup-and-run-accept.service -n 80 --no-pager || true
  elif [[ -n "$WORK" && -f "$WORK/shutup-and-run.log" ]]; then
    echo "---- $WORK/shutup-and-run.log ----"
    cat "$WORK/shutup-and-run.log" || true
  fi
}

# Reachable from the EXIT trap. Shellcheck does not count that as a call.
# shellcheck disable=SC2317,SC2329
cleanup() {
  if [[ -n "${CB_PID}" ]]; then
    kill "$CB_PID" >/dev/null 2>&1 || true
    wait "$CB_PID" >/dev/null 2>&1 || true
  fi
  if [[ "$MODE" == systemd ]]; then
    systemctl stop shutup-and-run-accept.service >/dev/null 2>&1 || true
    rm -f /etc/systemd/system/shutup-and-run-accept.service
    systemctl daemon-reload >/dev/null 2>&1 || true
  elif [[ -n "$WORK" && -s "$WORK/pid" ]]; then
    kill "$(cat "$WORK/pid")" >/dev/null 2>&1 || true
  fi
  if ((${#IDS[@]} > 0)); then
    docker rm -f "${IDS[@]/#/shutup-and-run-}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

remember() {
  local id="${1:-}"
  if [[ -n "$id" ]]; then
    IDS+=("$id")
  fi
}

if ! command -v curl >/dev/null 2>&1; then
  echo "FAIL: curl is required"
  exit 1
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "FAIL: python3 is required for the callback check"
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "FAIL: 'docker info' failed. Run with sudo, or as a user in the docker group."
  exit 1
fi
if [[ $# -gt 1 ]]; then
  echo "usage: sudo bash scripts/accept.sh [path-to-shutup-and-run]"
  exit 1
fi

if [[ -n "${1:-}" ]]; then
  BIN=$(readlink -f "$1")
elif [[ -x "$ROOT/target/release/shutup-and-run" ]]; then
  BIN=$ROOT/target/release/shutup-and-run
elif command -v cargo >/dev/null 2>&1; then
  echo "compiling (cargo build --release)..."
  (cd "$ROOT" && cargo build --release)
  BIN=$ROOT/target/release/shutup-and-run
else
  echo "FAIL: no binary. Pass a path, or install Rust and let the script compile."
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "FAIL: $BIN is not executable"
  exit 1
fi

echo "== shutup-and-run acceptance =="
echo "binary: $BIN"
echo "NOTE: test containers use the label shutup-and-run=1 on this Docker."

if ! docker pull alpine:latest >/dev/null; then
  if docker image inspect alpine:latest >/dev/null 2>&1; then
    echo "NOTE: docker pull failed; using the local alpine image."
  else
    echo "FAIL: could not obtain alpine:latest"
    exit 1
  fi
fi

if [[ $(id -u) -eq 0 ]]; then
  WORK=/var/lib/shutup-and-run-accept
else
  WORK=/tmp/shutup-and-run-accept-${UID}
fi
mkdir -p "$WORK" || { echo "FAIL: could not create $WORK"; exit 1; }
chmod 700 "$WORK"

if command -v openssl >/dev/null 2>&1; then
  TOKEN=$(openssl rand -hex 24)
else
  TOKEN=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
fi

umask 077
cat >"$WORK/config.toml" <<EOF
listen = "127.0.0.1:${PORT}"
token = "${TOKEN}"
max_slots = 2
allowlist = ["alpine"]
max_job_cpus = "1"
max_job_memory = "256m"
default_timeout_secs = 60
max_timeout_secs = 120
reaper_interval_secs = 1
history_path = "${WORK}/jobs.sqlite"
history_days = 0
purge_interval_secs = 15
log_lines = 200
http = true
auth_failures = 3
EOF
BODY=$WORK/body.json

HAVE_SYSTEMD=0
if [[ $(id -u) -eq 0 ]] && [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
  HAVE_SYSTEMD=1
fi

if [[ "$HAVE_SYSTEMD" -eq 1 ]]; then
  systemctl stop shutup-and-run-accept.service >/dev/null 2>&1 || true
  install -m 0755 "$BIN" /usr/local/bin/shutup-and-run || { echo "FAIL: could not install the binary"; exit 1; }
  cat >/etc/systemd/system/shutup-and-run-accept.service <<EOF
[Unit]
Description=shutup-and-run acceptance
After=docker.service
Requires=docker.service

[Service]
Type=simple
ExecStart=/usr/local/bin/shutup-and-run
Environment=SHUTUP_AND_RUN_CONFIG=${WORK}/config.toml
Environment=HOME=/root
Restart=no

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  printf '#!/usr/bin/env bash\nsystemctl restart shutup-and-run-accept.service\n' >"$WORK/restart.sh"
  if ! systemctl start shutup-and-run-accept.service; then
    echo "FAIL: systemctl did not start shutup-and-run-accept.service"
    MODE=systemd
    dump_logs
    exit 1
  fi
  MODE=systemd
  echo "mode: systemd (unit shutup-and-run-accept.service, binary /usr/local/bin/shutup-and-run)"
else
  if [[ $(id -u) -eq 0 ]]; then
    install -m 0755 "$BIN" /usr/local/bin/shutup-and-run || { echo "FAIL: could not install the binary"; exit 1; }
    BIN_RUN=/usr/local/bin/shutup-and-run
  else
    BIN_RUN=$BIN
  fi
  # Start and restart are the same script, so smoke.sh can restart it too.
  cat >"$WORK/restart.sh" <<EOF
#!/usr/bin/env bash
pid=\$(cat "${WORK}/pid" 2>/dev/null)
if [[ -n "\$pid" ]]; then
  kill "\$pid" >/dev/null 2>&1
  for _ in \$(seq 1 50); do
    kill -0 "\$pid" 2>/dev/null || break
    grep -q '^State:[[:space:]]*Z' "/proc/\$pid/status" 2>/dev/null && break
    sleep 0.1
  done
fi
SHUTUP_AND_RUN_CONFIG="${WORK}/config.toml" HOME="${HOME:-/tmp}" nohup "${BIN_RUN}" >>"${WORK}/shutup-and-run.log" 2>&1 &
echo \$! >"${WORK}/pid"
EOF
  rm -f "$WORK/pid" "$WORK/shutup-and-run.log"
  bash "$WORK/restart.sh"
  MODE=background
  echo "mode: background pid $(cat "$WORK/pid") (no systemd here; on Ubuntu with sudo the script installs the unit)"
fi

wait_ready() {
  ready=0
  for _ in $(seq 1 50); do
    if curl -sf -m 2 -H "Authorization: Bearer ${TOKEN}" "$BASE/health" >/dev/null; then
      ready=1
      break
    fi
    sleep 0.2
  done
}

SHUTUP_AND_RUN_CONFIG="$WORK/config.toml" "$BIN" bans lift 127.0.0.1 >/dev/null 2>&1 || true
wait_ready
if [[ "$ready" -ne 1 ]]; then
  echo "FAIL: the service did not answer $BASE/health"
  dump_logs
  exit 1
fi

health=$(curl -sS -m 5 -H "Authorization: Bearer ${TOKEN}" "$BASE/health")
total=$(printf '%s' "$health" | sed -n 's/.*"slots_total"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
used=$(printf '%s' "$health" | sed -n 's/.*"slots_used"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
if [[ "$total" != 2 || "$used" != 0 ]]; then
  echo "FAIL: unexpected initial health: $health"
  echo "If slots_used is not 0, a container labeled shutup-and-run=1 is already running."
  exit 1
fi

post() {
  CODE=$(curl -sS -m 180 -o "$BODY" -w '%{http_code}' \
    -X POST "$BASE/v1/jobs" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H 'Content-Type: application/json' \
    --data "$1") || CODE=000
}

job_id() {
  sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F][0-9a-fA-F]*\)".*/\1/p' "$BODY" | head -n 1
}

slots_free() {
  local h
  h=$(curl -sS -m 5 -H "Authorization: Bearer ${TOKEN}" "$BASE/health" || true)
  printf '%s' "$h" | sed -n 's/.*"slots_free"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1
}

get_job() {
  local id="$1"
  local query="${2:-}"
  curl -sS -m 10 -o "$WORK/status.json" -w '%{http_code}' \
    -H "Authorization: Bearer ${TOKEN}" \
    "$BASE/v1/jobs/${id}${query}" || echo 000
}

post '{"image":"alpine","command":["sleep","4"]}'
c1=$CODE
id1=$(job_id || true)
remember "$id1"
post '{"image":"alpine","command":["sleep","4"]}'
c2=$CODE
id2=$(job_id || true)
remember "$id2"
free=
for _ in 1 2 3 4 5; do
  free=$(slots_free || true)
  [[ "$free" == 0 ]] && break
  sleep 0.3
done
if [[ "$c1" == 202 && "$c2" == 202 && -n "$id1" && -n "$id2" && "$free" == 0 ]]; then
  pass "accepted until full"
else
  fail "accepted until full (codes ${c1} ${c2}, slots_free=${free:-?})"
fi

post '{"image":"alpine","command":["sleep","30"]}'
if [[ "$CODE" == 429 ]]; then
  pass "429 when full"
else
  fail "429 when full (code ${CODE})"
  remember "$(job_id || true)"
fi

sleep 10
post '{"image":"alpine","command":["sleep","1"]}'
remember "$(job_id || true)"
if [[ "$CODE" == 202 ]]; then
  pass "slot freed after a job ends"
else
  fail "slot freed after a job ends (code ${CODE})"
fi

if curl -sf -m 5 -H "Authorization: Bearer ${TOKEN}" "$BASE/metrics" | grep -q 'shutup_and_run_slots_total 2'; then
  pass "metrics exposed"
else
  fail "metrics exposed"
fi

sleep 3
post '{"image":"alpine","command":["sh","-c","echo shutup-and-run-log-marker; sleep 180"],"job_key":"accept-key"}'
long_code=$CODE
LONG=$(job_id || true)
remember "$LONG"
# A job that named no limits must carry the machine's: 1 CPU, 256m, no extra swap.
limits=""
for _ in $(seq 1 40); do
  limits=$(docker inspect -f '{{.HostConfig.NanoCpus}} {{.HostConfig.Memory}} {{.HostConfig.MemorySwap}}' "shutup-and-run-$LONG" 2>/dev/null || true)
  [[ -n "$limits" ]] && break
  sleep 0.25
done
if [[ "$limits" == "1000000000 268435456 268435456" ]]; then
  pass "default cpu and memory limits on the container"
else
  fail "default limits on the container (got '${limits}')"
fi
post '{"image":"alpine","command":["sh","-c","echo shutup-and-run-log-marker; sleep 180"],"job_key":"accept-key"}'
replay_code=$CODE
REPLAY=$(job_id || true)
used_after=$(curl -sS -m 5 -H "Authorization: Bearer ${TOKEN}" "$BASE/health" | sed -n 's/.*"slots_used"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
if [[ "$long_code" == 202 && "$replay_code" == 200 && -n "$LONG" && "$LONG" == "$REPLAY" && "$used_after" == 1 ]]; then
  pass "idempotent job_key"
else
  fail "idempotent job_key (codes ${long_code}/${replay_code}, ids ${LONG:-?}/${REPLAY:-?}, slots_used=${used_after:-?})"
fi

# 202 comes before the image pull. Give the container a moment to exist.
for _ in $(seq 1 60); do
  log_code=$(get_job "$LONG" "?logs=true")
  grep -q 'shutup-and-run-log-marker' "$WORK/status.json" && break
  sleep 0.5
done
if [[ "$log_code" == 200 ]] && grep -q 'shutup-and-run-log-marker' "$WORK/status.json"; then
  pass "logs returned"
else
  fail "logs returned (http ${log_code})"
fi

bash "$WORK/restart.sh"
wait_ready
restart_code=$(get_job "$LONG")
if [[ "$ready" == 1 && "$restart_code" == 200 ]] && grep -q '"status"[[:space:]]*:[[:space:]]*"running"' "$WORK/status.json"; then
  pass "status survives restart"
else
  fail "status survives restart (http ${restart_code})"
fi

python3 -c "
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get('Content-Length', '0'))
        data = self.rfile.read(n)
        open('${WORK}/callback.body', 'wb').write(data)
        self.send_response(204)
        self.end_headers()
    def log_message(self, fmt, *args):
        return
ThreadingHTTPServer(('127.0.0.1', ${CALLBACK_PORT}), H).serve_forever()
" &
CB_PID=$!
sleep 0.3

post "{\"image\":\"alpine\",\"command\":[\"echo\",\"callback-marker\"],\"callback_url\":\"http://127.0.0.1:${CALLBACK_PORT}/hook\",\"job_key\":\"callback-key\"}"
CB_JOB=$(job_id || true)
remember "$CB_JOB"
callback_ok=0
if [[ "$CODE" == 202 && -n "$CB_JOB" ]]; then
  for _ in $(seq 1 40); do
    if [[ -s "$WORK/callback.body" ]] && grep -q "$CB_JOB" "$WORK/callback.body" && grep -q '"reason"' "$WORK/callback.body"; then
      callback_ok=1
      break
    fi
    sleep 0.25
  done
fi
if [[ "$callback_ok" == 1 ]]; then
  pass "callback delivered"
else
  fail "callback delivered"
fi

post '{"image":"alpine","command":["true"],"memory":"1g"}'
if [[ "$CODE" == 400 ]] && grep -q "above this machine's limit" "$BODY"; then
  pass "a job above the machine's memory limit is refused"
else
  fail "a job above the limit (HTTP ${CODE})"
fi

post '{"image":"alpine","command":["sleep","300"],"timeout_secs":3,"job_key":"timeout-key"}'
TID=$(job_id || true)
remember "$TID"
timeout_ok=0
if [[ "$CODE" == 202 && -n "$TID" ]]; then
  for _ in $(seq 1 20); do
    st=$(get_job "$TID")
    if [[ "$st" == 200 ]] && grep -q 'timeout_kill' "$WORK/status.json"; then
      timeout_ok=1
      break
    fi
    sleep 0.5
  done
fi
if [[ "$timeout_ok" == 1 ]]; then
  pass "timeout kill"
else
  state=$(docker inspect -f '{{.State.Status}}' "shutup-and-run-$TID" 2>/dev/null || echo gone)
  if [[ "$state" != "running" && "$state" != "restarting" && "$state" != "paused" ]]; then
    pass "timeout kill"
  else
    fail "timeout kill (container ${state})"
  fi
fi

drain_code=$(curl -sS -m 10 -o "$WORK/drain.json" -w '%{http_code}' \
  -X POST "$BASE/v1/drain" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H 'Content-Type: application/json' \
  --data '{"draining":true}' || echo 000)
post '{"image":"alpine","command":["sleep","30"],"job_key":"while-draining"}'
health=$(curl -sS -m 5 -H "Authorization: Bearer ${TOKEN}" "$BASE/health" || true)
if [[ "$drain_code" == 200 && "$CODE" == 503 ]] && printf '%s' "$health" | grep -q '"draining"[[:space:]]*:[[:space:]]*true'; then
  pass "drain rejects new jobs"
else
  fail "drain rejects new jobs (drain ${drain_code}, post ${CODE}, health ${health})"
fi
curl -sS -m 10 -X POST "$BASE/v1/drain" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H 'Content-Type: application/json' \
  --data '{"draining":false}' >/dev/null || true

purged=0
if [[ -n "$CB_JOB" ]]; then
  for _ in $(seq 1 40); do
    st=$(get_job "$CB_JOB")
    long_st=$(get_job "$LONG")
    if [[ "$st" == 404 && "$long_st" == 200 ]]; then
      purged=1
      break
    fi
    sleep 0.5
  done
fi
if [[ "$purged" == 1 ]]; then
  pass "history purged"
else
  fail "history purged (finished job still present, or the running job disappeared)"
fi

bad=0
for _ in 1 2 3; do
  code=$(curl -sS -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer not-the-token" "$BASE/health" || echo 000)
  [[ "$code" == 401 ]] && bad=$((bad + 1))
done
blocked=$(curl -sS -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${TOKEN}" "$BASE/health" || echo 000)
if [[ "$bad" == 3 && "$blocked" == 403 ]]; then
  pass "banned after 3 failures"
else
  fail "banned after 3 failures (401s=${bad}, next=${blocked})"
fi
SHUTUP_AND_RUN_CONFIG="$WORK/config.toml" "$BIN" bans lift 127.0.0.1 >/dev/null
restored=$(curl -sS -m 5 -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${TOKEN}" "$BASE/health" || echo 000)
if [[ "$restored" == 200 ]]; then
  pass "ban lifted"
else
  fail "ban lifted (health ${restored})"
fi

# Optional second pass: the smoke test, against this same instance, with every
# scenario switched on. SMOKE_STRICT makes a skipped scenario a failure.
if [[ "${SHUTUP_AND_RUN_SMOKE:-}" == 1 ]]; then
  echo
  if ((${#IDS[@]} > 0)); then
    docker rm -f "${IDS[@]/#/shutup-and-run-}" >/dev/null 2>&1 || true
  fi
  sed -i 's/^history_days = 0$/history_days = 30/' "$WORK/config.toml"
  cat >>"$WORK/config.toml" <<EOF
poll_url = "http://127.0.0.1:18790/poll"
poll_interval_secs = 2
EOF
  bash "$WORK/restart.sh"
  wait_ready
  SMOKE_URL="$BASE" SMOKE_TOKEN="$TOKEN" SMOKE_IMAGE=alpine SMOKE_POLL=1 SMOKE_STRICT=1 \
    SMOKE_RESTART_CMD="bash $WORK/restart.sh" \
    SMOKE_BAN_LIFT_CMD="env SHUTUP_AND_RUN_CONFIG=$WORK/config.toml $BIN bans lift" \
    bash "$ROOT/scripts/smoke.sh" || fail "smoke.sh"
fi

echo
if [[ "$FAILURES" -eq 0 ]]; then
  echo "RESULT: all passed"
  exit 0
fi
echo "RESULT: ${FAILURES} failed"
dump_logs
exit 1

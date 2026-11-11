#!/usr/bin/env bash
# RSS and CPU of the shutup-and-run process (not the job containers).
# Not part of CI. Needs Docker and a built binary.
#
#   cargo build --release --locked
#   bash scripts/size.sh target/release/shutup-and-run
#
# Prints one row per scenario. Work files stay in a temp directory named at
# the end. The token exists only in that directory and in the process env.
set -eEuo pipefail

BIN=${1:-}
if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "usage: bash scripts/size.sh /path/to/shutup-and-run" >&2
  exit 1
fi

# The launcher opens the Docker socket itself. If this user cannot, widen the
# socket for the run and put it back. sudo docker would measure the wrong pid.
SOCK=/var/run/docker.sock
RESTORE_SOCK=""
if docker info >/dev/null 2>&1; then
  DOCKER=(docker)
elif sudo docker info >/dev/null 2>&1; then
  RESTORE_SOCK=$(sudo stat -c %a "$SOCK")
  sudo chmod a+rw "$SOCK"
  DOCKER=(docker)
else
  echo "docker is not running" >&2
  exit 1
fi

ROOT=$(mktemp -d /tmp/sar-size.XXXXXX)
TOKEN=$(python3 -c 'print("s" * 32)')
POLL_TOKEN=$(python3 -c 'print("p" * 32)')
PORT=18771
POLL_PORT=18772
BASE="http://127.0.0.1:${PORT}"
IMAGE="${SIZE_IMAGE:-busybox:1.36}"
SAR_PID=""
POLL_PID=""

cleanup() {
  if [[ -n "${SAR_PID}" ]] && kill -0 "$SAR_PID" 2>/dev/null; then
    kill "$SAR_PID" 2>/dev/null || true
    wait "$SAR_PID" 2>/dev/null || true
  fi
  if [[ -n "${POLL_PID}" ]] && kill -0 "$POLL_PID" 2>/dev/null; then
    kill "$POLL_PID" 2>/dev/null || true
    wait "$POLL_PID" 2>/dev/null || true
  fi
  if [[ -n "${DOCKER+x}" ]]; then
    "${DOCKER[@]}" ps -aq --filter label=shutup-and-run=1 | xargs -r "${DOCKER[@]}" rm -f >/dev/null 2>&1 || true
  fi
  if [[ -n "$RESTORE_SOCK" ]]; then
    sudo chmod "$RESTORE_SOCK" "$SOCK" 2>/dev/null || true
  fi
}
on_err() {
  echo "size.sh failed. launcher log:" >&2
  tail -n 50 "$ROOT/log" >&2 || true
}
trap on_err ERR
trap cleanup EXIT

cat >"$ROOT/config.toml" <<EOF
listen = "127.0.0.1:${PORT}"
max_slots = 64
allowlist = ["busybox"]
http = true
history_path = "${ROOT}/jobs.sqlite"
history_days = 3650
purge_interval_secs = 86400
prune_interval_secs = 0
reaper_interval_secs = 2
default_timeout_secs = 300
max_timeout_secs = 600
log_lines = 200
EOF

echo "binary: $BIN"
file "$BIN" || true
echo "host: $(uname -srm)"
echo "cpu: $(nproc) x $(awk -F: '/model name/ { print $2; exit }' /proc/cpuinfo | sed 's/^ //')"
echo "mem_kib: $(awk '/MemTotal:/ { print $2 }' /proc/meminfo)"
rustc --version || true
"${DOCKER[@]}" version --format 'docker {{.Server.Version}}' || true

echo "pulling ${IMAGE}"
if ! "${DOCKER[@]}" pull "$IMAGE" >/dev/null; then
  IMAGE=busybox:latest
  echo "pulling ${IMAGE}"
  "${DOCKER[@]}" pull "$IMAGE" >/dev/null
fi

stop_launcher() {
  if [[ -n "${SAR_PID}" ]] && kill -0 "$SAR_PID" 2>/dev/null; then
    kill "$SAR_PID" 2>/dev/null || true
    wait "$SAR_PID" 2>/dev/null || true
  fi
  SAR_PID=""
}

wait_health() {
  local i code
  for i in $(seq 1 50); do
    code=$(curl -s -o /dev/null -w '%{http_code}' -H "Authorization: Bearer ${TOKEN}" "${BASE}/health" || true)
    if [[ "$code" == "200" ]]; then
      return 0
    fi
    sleep 0.2
  done
  echo "health did not return 200" >&2
  return 1
}

start_launcher() {
  local slots=$1
  local reaper=$2
  local poll_url=${3:-}
  stop_launcher
  (
    export SHUTUP_AND_RUN_CONFIG="$ROOT/config.toml"
    export SHUTUP_AND_RUN_TOKEN="$TOKEN"
    export SHUTUP_AND_RUN_MAX_SLOTS="$slots"
    export SHUTUP_AND_RUN_REAPER_INTERVAL_SECS="$reaper"
    export RUST_LOG=warn
    if [[ -n "$poll_url" ]]; then
      export SHUTUP_AND_RUN_POLL_URL="$poll_url"
      export SHUTUP_AND_RUN_POLL_INTERVAL_SECS=1
      export SHUTUP_AND_RUN_POLL_TOKEN="$POLL_TOKEN"
    fi
    exec "$BIN"
  ) >"$ROOT/log" 2>&1 &
  SAR_PID=$!
  wait_health
}

proc_ticks() {
  sed -n 's/.*) //p' "/proc/$1/stat" | awk '{ print $12 + $13 }'
}

# avg_mib max_mib cpu_avg_pct cpu_max_1s_pct hwm_mib
# cpu is percent of one core.
sample_pid() {
  local pid=$1
  local secs=$2
  local clk sum=0 max=0 n=0 rss i
  local start_ticks start_ns prev_ticks prev_ns now_ticks now_ns
  local dt dns cpu_1s max_cpu=0
  clk=$(getconf CLK_TCK)
  start_ticks=$(proc_ticks "$pid")
  start_ns=$(date +%s%N)
  prev_ticks=$start_ticks
  prev_ns=$start_ns
  for ((i = 0; i < secs; i++)); do
    sleep 1
    rss=$(awk '/^VmRSS:/ { print $2 }' "/proc/${pid}/status")
    sum=$((sum + rss))
    n=$((n + 1))
    if ((rss > max)); then
      max=$rss
    fi
    now_ticks=$(proc_ticks "$pid")
    now_ns=$(date +%s%N)
    dt=$((now_ticks - prev_ticks))
    dns=$((now_ns - prev_ns))
    cpu_1s=$(awk -v dt="$dt" -v clk="$clk" -v ns="$dns" 'BEGIN { printf "%.4f", (dt / clk) / (ns / 1e9) * 100 }')
    max_cpu=$(awk -v a="$max_cpu" -v b="$cpu_1s" 'BEGIN { print (a > b) ? a : b }')
    prev_ticks=$now_ticks
    prev_ns=$now_ns
  done
  local hwm
  hwm=$(awk '/^VmHWM:/ { print $2 }' "/proc/${pid}/status")
  awk -v sum="$sum" -v n="$n" -v max="$max" -v hwm="$hwm" \
    -v dt="$((now_ticks - start_ticks))" -v clk="$clk" \
    -v ns="$((now_ns - start_ns))" -v max_cpu="$max_cpu" \
    'BEGIN {
       printf "%.1f %.1f %.2f %.2f %.1f\n", sum / n / 1024, max / 1024, (dt / clk) / (ns / 1e9) * 100, max_cpu, hwm / 1024
     }'
}

# Sum VmRSS (KiB) of every process whose name is one of the arguments.
rss_named() {
  local name pid rss sum=0
  for name in "$@"; do
    for pid in $(pgrep -x "$name" || true); do
      rss=$(awk '/^VmRSS:/ { print $2 }' "/proc/${pid}/status" 2>/dev/null || echo 0)
      sum=$((sum + rss))
    done
  done
  echo "$sum"
}

running_jobs() {
  "${DOCKER[@]}" ps -q --filter label=shutup-and-run=1 --filter status=running | wc -l
}

remove_jobs() {
  "${DOCKER[@]}" ps -aq --filter label=shutup-and-run=1 | xargs -r "${DOCKER[@]}" rm -f >/dev/null
}

wait_running() {
  local want=$1
  local i n=0
  for i in $(seq 1 80); do
    n=$(running_jobs)
    if [[ "$n" -ge "$want" ]]; then
      return 0
    fi
    sleep 0.5
  done
  echo "running jobs ${n}, wanted ${want}" >&2
  return 1
}

post_jobs() {
  local n=$1
  local prefix=$2
  local timeout_secs=$3
  local sleep_for=$4
  N="$n" PREFIX="$prefix" TIMEOUT_SECS="$timeout_secs" SLEEP_FOR="$sleep_for" \
    IMAGE="$IMAGE" BASE="$BASE" TOKEN="$TOKEN" python3 - <<'PY'
import json, os, urllib.error, urllib.request
from collections import Counter
import concurrent.futures
url = os.environ["BASE"] + "/v1/jobs"
token = os.environ["TOKEN"]
n = int(os.environ["N"])
prefix = os.environ["PREFIX"]
timeout_secs = int(os.environ["TIMEOUT_SECS"])
sleep_for = os.environ["SLEEP_FOR"]
image = os.environ["IMAGE"]

def one(i):
    body = json.dumps({
        "image": image,
        "command": ["sleep", sleep_for],
        "timeout_secs": timeout_secs,
        "memory": "32m",
        "job_key": f"{prefix}-{i}",
    }).encode()
    req = urllib.request.Request(url, data=body, method="POST", headers={
        "Authorization": "Bearer " + token,
        "Content-Type": "application/json",
    })
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status
    except urllib.error.HTTPError as err:
        return err.code

workers = min(32, n)
with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
    codes = list(pool.map(one, range(n)))
print(dict(Counter(codes)))
PY
}

container_mem() {
  "${DOCKER[@]}" stats --no-stream --format '{{.Name}} {{.MemUsage}}' | awk '
    $1 ~ /^shutup-and-run-/ {
      v = $2 + 0
      u = $2
      if (u ~ /GiB/) kib = v * 1024 * 1024
      else if (u ~ /MiB/) kib = v * 1024
      else if (u ~ /KiB/) kib = v
      else kib = v / 1024
      sum += kib
      n++
    }
    END {
      if (n == 0) printf "0 0\n"
      else printf "%.1f %.1f\n", sum / n / 1024, sum / 1024
    }
  '
}

echo
echo "scenario rss_avg_mib rss_max_mib cpu_avg_pct cpu_max_1s_pct rss_hwm_mib notes"

echo "--- idle ---"
remove_jobs
start_launcher 64 2
sleep 2
# shellcheck disable=SC2086
read -r idle_avg idle_max idle_cpu idle_cpu1 idle_hwm <<<"$(sample_pid "$SAR_PID" 15)"
echo "idle ${idle_avg} ${idle_max} ${idle_cpu} ${idle_cpu1} ${idle_hwm} reaper_interval=2s no jobs"
BASE_DAEMON=$(rss_named dockerd containerd)
echo "docker_daemon_baseline_kib ${BASE_DAEMON} (dockerd + containerd, no job containers)"

echo "--- post storm ---"
remove_jobs
start_launcher 4 2
sample_pid "$SAR_PID" 8 >"$ROOT/storm.txt" &
storm_sample=$!
storm_codes=$(post_jobs 200 storm 120 30)
wait "$storm_sample"
# shellcheck disable=SC2086
read -r storm_avg storm_max storm_cpu storm_cpu1 storm_hwm <"$ROOT/storm.txt"
echo "post_storm ${storm_avg} ${storm_max} ${storm_cpu} ${storm_cpu1} ${storm_hwm} ${storm_codes} max_slots=4"

echo "--- slots ---"
remove_jobs
start_launcher 64 2
sleep 1
have=0
for n in 1 8 32 64; do
  post_jobs $((n - have)) "slot${n}" 300 180 >/dev/null
  wait_running "$n"
  sleep 2
  # shellcheck disable=SC2086
  read -r s_avg s_max s_cpu s_cpu1 s_hwm <<<"$(sample_pid "$SAR_PID" 10)"
  # comm is 15 characters, so containerd-shim-runc-v2 shows up as containerd-shim.
  shim=$(rss_named containerd-shim)
  daemon=$(rss_named dockerd containerd)
  daemon_delta=$((daemon - BASE_DAEMON))
  # shellcheck disable=SC2086
  read -r c_avg c_sum <<<"$(container_mem)"
  echo "slots_${n} ${s_avg} ${s_max} ${s_cpu} ${s_cpu1} ${s_hwm} shim_kib=${shim} daemon_delta_kib=${daemon_delta} container_avg_mib=${c_avg} container_sum_mib=${c_sum}"
  have=$n
done

echo "--- poll ---"
remove_jobs
python3 - <<'PY' &
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0") or 0)
        if n:
            self.rfile.read(n)
        body = b"{}"
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, fmt, *args):
        return
ThreadingHTTPServer(("127.0.0.1", 18772), H).serve_forever()
PY
POLL_PID=$!
sleep 0.3
start_launcher 64 2 "http://127.0.0.1:${POLL_PORT}/poll"
sleep 2
# shellcheck disable=SC2086
read -r poll_avg poll_max poll_cpu poll_cpu1 poll_hwm <<<"$(sample_pid "$SAR_PID" 15)"
echo "poll_empty ${poll_avg} ${poll_max} ${poll_cpu} ${poll_cpu1} ${poll_hwm} interval=1s empty object"
kill "$POLL_PID" 2>/dev/null || true
wait "$POLL_PID" 2>/dev/null || true
POLL_PID=""

echo "--- reaper ---"
remove_jobs
start_launcher 64 1
post_jobs 32 reaper 2 120 >/dev/null
wait_running 32 || true
# shellcheck disable=SC2086
read -r reap_avg reap_max reap_cpu reap_cpu1 reap_hwm <<<"$(sample_pid "$SAR_PID" 15)"
left=$(running_jobs)
echo "reaper ${reap_avg} ${reap_max} ${reap_cpu} ${reap_cpu1} ${reap_hwm} started=32 timeout_secs=2 interval=1s still_running=${left}"

echo "--- history ---"
stop_launcher
remove_jobs
python3 - <<PY
import os, sqlite3, time
path = "${ROOT}/jobs.sqlite"
con = sqlite3.connect(path)
con.execute("PRAGMA journal_mode=WAL")
log = ("0123456789abcdef line of job output" + chr(10)) * 128
now = int(time.time())
rows = [
    # Job ids are hex. Anything else is 404 and the row is never read.
    (f"{i:08x}", "busybox:1.36", f"k{i:08x}", now, now, 0, "exited", log)
    for i in range(10000)
]
con.executemany(
    "INSERT INTO jobs (id, image, job_key, started_at, ended_at, exit_code, reason, logs) VALUES (?,?,?,?,?,?,?,?)",
    rows,
)
con.commit()
print("history_rows", 10000, "db_bytes", os.path.getsize(path), "log_bytes", len(log))
PY
start_launcher 64 2
sleep 1
# shellcheck disable=SC2086
read -r hist_idle_avg hist_idle_max hist_idle_cpu hist_idle_cpu1 hist_idle_hwm <<<"$(sample_pid "$SAR_PID" 8)"
sample_pid "$SAR_PID" 10 >"$ROOT/hist.txt" &
hist_sample=$!
hist_reqs=$(BASE="$BASE" TOKEN="$TOKEN" python3 - <<'PY'
import os, random, time, urllib.request
base = os.environ["BASE"]
token = os.environ["TOKEN"]
deadline = time.time() + 8
n = 0
while time.time() < deadline:
    i = random.randrange(0, 10000)
    url = f"{base}/v1/jobs/{i:08x}?logs=true"
    req = urllib.request.Request(url, headers={"Authorization": "Bearer " + token})
    with urllib.request.urlopen(req, timeout=10) as resp:
        resp.read()
    if n % 25 == 0:
        m = urllib.request.Request(base + "/metrics", headers={"Authorization": "Bearer " + token})
        with urllib.request.urlopen(m, timeout=10) as resp:
            resp.read()
    n += 1
print(n)
PY
)
wait "$hist_sample"
# shellcheck disable=SC2086
read -r hist_avg hist_max hist_cpu hist_cpu1 hist_hwm <"$ROOT/hist.txt"
echo "history_idle ${hist_idle_avg} ${hist_idle_max} ${hist_idle_cpu} ${hist_idle_cpu1} ${hist_idle_hwm} 10000 finished rows, no reads"
echo "history_read ${hist_avg} ${hist_max} ${hist_cpu} ${hist_cpu1} ${hist_hwm} gets=${hist_reqs} with logs plus /metrics"

echo
echo "work_dir ${ROOT}"

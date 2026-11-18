#!/usr/bin/env bash
# The pictures of the local page in the README: docs/screenshots/{en,pt}-*.png.
#
#   bash scripts/screenshots.sh [BIN]
#
# BIN is a built worker (default target/release/shutup-and-run, built when
# missing). It runs on a history of its own, in a temporary folder, on
# 127.0.0.1 only, with a stand-in poll URL that never has work: three jobs
# (one ok, one that exits 3, one still running), then the pages in English
# and Portuguese, light, dark and on a phone. The state of a small PC waiting
# for its owner comes from the page's own render function
# (`render_states_for_screenshots`), since a test machine is not one.
#
# Needs Docker, python3, cargo, and node with the playwright package and its
# Chromium (`npm install playwright && npx playwright install chromium`).
# Pillow, when python3 has it, makes the files smaller. Like accept.sh it
# needs no other container labeled shutup-and-run=1: the demo worker would
# tidy them up as its own.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${1:-$ROOT/target/release/shutup-and-run}
OUT=$ROOT/docs/screenshots
PORT=${SCREENSHOTS_PORT:-18793}
POLL_PORT=$((PORT + 1))
TOKEN=demo-token-for-screenshots-0123456789

if [[ ! -x "$BIN" ]]; then
  (cd "$ROOT" && cargo build --release --locked)
fi
if [[ -n "$(docker ps -aq --filter label=shutup-and-run=1)" ]]; then
  echo "FAIL: containers labeled shutup-and-run=1 exist; this script needs a Docker without them" >&2
  exit 1
fi
node -e "require('playwright')" 2>/dev/null || {
  echo "FAIL: node cannot load playwright (npm install playwright && npx playwright install chromium)" >&2
  exit 1
}

WORK=$(mktemp -d)
WORKER_PID=""
POLL_PID=""
JOBS=()
cleanup() {
  if [[ -n "$WORKER_PID" ]]; then
    kill "$WORKER_PID" 2>/dev/null || true
    wait "$WORKER_PID" 2>/dev/null || true
  fi
  [[ -n "$POLL_PID" ]] && kill "$POLL_PID" 2>/dev/null || true
  for id in "${JOBS[@]}"; do
    docker rm -f "shutup-and-run-$id" >/dev/null 2>&1 || true
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

# A poll URL that is there and never has work: 204 to every poll.
python3 -I -c '
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length") or 0))
        self.send_response(204)
        self.end_headers()
    def log_message(self, *a):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
' "$POLL_PORT" &
POLL_PID=$!
# The worker polls once at start: the stand-in must already answer.
for _ in $(seq 1 30); do
  curl -s -o /dev/null -X POST "http://127.0.0.1:$POLL_PORT/poll" && break
  sleep 0.2
done

cat >"$WORK/config.json" <<JSON
{
  "token": "$TOKEN",
  "listen": "127.0.0.1:$PORT",
  "http": true,
  "status_page": true,
  "max_slots": 2,
  "allowlist": ["alpine"],
  "history_path": "$WORK/jobs.sqlite",
  "poll_url": "http://127.0.0.1:$POLL_PORT/poll?user=ana",
  "worker_name": "DESK-07",
  "resource_profile": "shared-pc",
  "min_free_disk": "1g",
  "admin_user": "admin",
  "admin_password": "demo-password"
}
JSON
chmod 600 "$WORK/config.json"
SHUTUP_AND_RUN_CONFIG="$WORK/config.json" "$BIN" >"$WORK/worker.log" 2>&1 &
WORKER_PID=$!

for _ in $(seq 1 30); do
  curl -s -o /dev/null "http://127.0.0.1:$PORT/health" -H "Authorization: Bearer $TOKEN" && break
  sleep 1
done

job() {
  local body id
  body=$(curl -s -X POST "http://127.0.0.1:$PORT/v1/jobs" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d "$1")
  if ! id=$(printf '%s' "$body" | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["id"])' 2>/dev/null); then
    echo "FAIL: the worker did not take a job: ${body:-no answer}" >&2
    tail -n 20 "$WORK/worker.log" >&2
    exit 1
  fi
  JOBS+=("$id")
}
job '{"image":"alpine","command":["sh","-c","echo protocol 2026-0042 sent; echo ok"]}'
sleep 2
job '{"image":"alpine","command":["sh","-c","echo opening the portal; echo Traceback: timeout waiting for login >&2; exit 3"]}'
sleep 3
job '{"image":"alpine","timeout_secs":900,"command":["sh","-c","echo working; sleep 600"]}'
sleep 3

(cd "$ROOT" && SHUTUP_AND_RUN_SHOTS_DIR="$WORK" cargo test --locked -q render_states_for_screenshots -- --ignored >/dev/null)

mkdir -p "$OUT"
node "$ROOT/scripts/screenshots.js" "http://127.0.0.1:$PORT" "$WORK" "$OUT"

if python3 -I -c 'import PIL' 2>/dev/null; then
  python3 -I - "$OUT" <<'PY'
import os, sys
from PIL import Image
out = sys.argv[1]
for name in os.listdir(out):
    if name.endswith(".png") and name[:3] in ("en-", "pt-"):
        path = os.path.join(out, name)
        Image.open(path).convert("RGB").quantize(colors=128, method=Image.Quantize.MEDIANCUT).save(path, optimize=True)
PY
fi
ls -l "$OUT"/en-*.png "$OUT"/pt-*.png

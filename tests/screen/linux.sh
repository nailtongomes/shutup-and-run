#!/usr/bin/env bash
# The local page's screen view against a real worker and a real Docker, on
# Linux: a managed container of an image that serves noVNC on 6080, the
# page's "View screen" link, noVNC's page through /local/screen/ID/, the
# WebSocket from the page's origin (and not from another), and the route
# gone once the container ends. Run as root (the Docker socket).
#
#   sudo bash tests/screen/linux.sh target/release/shutup-and-run [IMAGE]
#
# IMAGE defaults to ghcr.io/nailtongomes/rpa-worker-selenium:latest (about
# 1.5 GB to download). With SHUTUP_AND_RUN_SCREEN_SHOT=FILE.png and node with
# Playwright, it also saves what the viewer shows. It starts its own worker on
# a temporary config and history, with a random token, and removes everything
# on exit. Not part of CI.
set -euo pipefail

BIN="$(readlink -f "${1:?usage: linux.sh <binary> [image]}")"
IMAGE="${2:-ghcr.io/nailtongomes/rpa-worker-selenium:latest}"
REPO="${IMAGE%:*}"
WORK="$(mktemp -d)"
PORT=18795
HOST="127.0.0.1:${PORT}"
URL="http://${HOST}"
TOKEN="$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')"

cat >"$WORK/config.toml" <<TOML
listen = "${HOST}"
http = true
status_page = true
screen_view = true
max_slots = 1
allowlist = ["${REPO}"]
default_timeout_secs = 900
max_timeout_secs = 900
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
# shellcheck disable=SC2329 # called through check
is() { [[ "$1" == "$2" ]] || { echo "      got: $1"; echo "   wanted: $2"; return 1; }; }
# shellcheck disable=SC2329
has() { grep -q -- "$2" <<<"$1" || { echo "      no '$2' in: ${1:0:300}"; return 1; }; }
page() { curl -sS -H "Host: $HOST" "$URL$1"; }
code() { curl -sS -o /dev/null -w '%{http_code}' "$@"; }
# The first bytes of an upgrade: the 101 and what the VNC server says first.
upgrade() { # path, origin
  curl -sS -i -N --http1.1 --max-time 4 \
    -H "Origin: $2" -H "Upgrade: websocket" -H "Connection: Upgrade" \
    -H "Sec-WebSocket-Key: c2NyZWVuLXZpZXctdGVzdA==" -H "Sec-WebSocket-Version: 13" \
    -H "Sec-WebSocket-Protocol: binary" "$URL$1" 2>/dev/null | head -c 600 | tr -d '\0' || true
}

"$BIN" serve >>"$WORK/worker.log" 2>&1 &
WORKER_PID=$!
for _ in $(seq 1 100); do
  curl -fsS -o /dev/null -H "Authorization: Bearer $TOKEN" "$URL/health" 2>/dev/null && break
  sleep 0.1
done

"$BIN" managed mode on --json >/dev/null
echo '{"name":"tela","image":"'"$IMAGE"'",
  "command":["python","/app/preflight.py"],
  "env":{"USE_XVFB":"1","USE_OPENBOX":"1","USE_VNC":"1","USE_NOVNC":"1"},
  "cpu":1,"memory":"2g","shm":"1g","timeout_secs":900,"policy":"once","enabled":true}' |
  "$BIN" managed apply - --json >/dev/null
"$BIN" managed start tela --json >/dev/null

ID=""
for _ in $(seq 1 300); do
  ID="$(page /local/managed | grep -o '/local/screen/[0-9a-f]*' | head -1 | cut -d/ -f4 || true)"
  [[ -n "$ID" ]] && break
  sleep 1
done
DESC="the managed list links to the screen once noVNC answers"
check test -n "$ID"
[[ -n "$ID" ]] || { tail -20 "$WORK/worker.log"; exit 1; }

DESC="the status page and the jobs list link to it too"
check has "$(page /)$(page /local/logs)" "/local/screen/$ID\""

DESC="the link opens noVNC connected, view only"
check has "$(curl -sS -o /dev/null -w '%{redirect_url}' "$URL/local/screen/$ID")" \
  "/local/screen/$ID/vnc.html?path=../../../local/screen/$ID/websockify&autoconnect=1&view_only=1&resize=scale"

DESC="noVNC's page comes through the proxy"
check has "$(page "/local/screen/$ID/vnc.html")" "noVNC"

DESC="the WebSocket from the page's origin reaches the VNC server"
answer="$(upgrade "/local/screen/$ID/websockify" "$URL")"
check has "$answer" "RFB 00"

DESC="a WebSocket from another site is refused"
check is "$(code -H "Origin: http://evil.example" -H "Upgrade: websocket" -H "Connection: Upgrade" \
  "$URL/local/screen/$ID/websockify")" "403"

DESC="another job id and the API port's token do not reach it"
check is "$(code "$URL/local/screen/0123456789abcdef/vnc.html") \
$(code -H "Authorization: Bearer $TOKEN" -H "Host: 203.0.113.9:$PORT" "$URL/local/screen/$ID/vnc.html")" \
  "404 404"

if [[ -n "${SHUTUP_AND_RUN_SCREEN_SHOT:-}" ]]; then
  SHOT="$(readlink -f "$SHUTUP_AND_RUN_SCREEN_SHOT")"
  node -e '
    const { chromium } = require("playwright");
    (async () => {
      const browser = await chromium.launch();
      const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
      await page.goto(process.argv[1]);
      await page.waitForTimeout(8000);
      await page.screenshot({ path: process.argv[2] });
      await browser.close();
    })();' "$URL/local/screen/$ID" "$SHOT"
  echo "saved $SHOT"
fi

DESC="once the container ends, the screen is gone"
for _ in $(seq 1 900); do
  [[ "$(docker ps -q --filter label=shutup-and-run=1 | wc -l)" == 0 ]] && break
  sleep 1
done
sleep 2
check is "$(code "$URL/local/screen/$ID/vnc.html")" "404"

exit "$fail"

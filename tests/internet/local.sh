#!/usr/bin/env bash
# Internet sharing on one Linux machine: a relay on loopback, `internet on`
# against it, curl through the proxy, the relay restarted, curl again.
# It proves the path and the reconnection, not the IP: both ends have the
# same one. docs/INTERNET.md has the test from another network.
#
#   cargo build --release --locked
#   sudo bash tests/internet/local.sh target/release/shutup-and-run
#
# Leaves nothing behind: `internet off --forget` at the end.
set -euo pipefail

bin=$(realpath "${1:?usage: local.sh PATH_TO_BINARY}")
agents=127.0.0.1:17000
proxy_port=18080
work=$(mktemp -d)
relay_pid=
serve_pid=

finish() {
  [ -n "$relay_pid" ] && kill "$relay_pid" 2>/dev/null || true
  "$bin" internet off --forget >/dev/null 2>&1 || true
  [ -n "$serve_pid" ] && kill "$serve_pid" 2>/dev/null || true
  rm -rf "$work"
}
trap finish EXIT

token=local-test-token-1234
printf '%s\n' "$token" >"$work/token"

start_relay() {
  "$bin" relay --agents "$agents" --proxy "127.0.0.1:$proxy_port" --token-file "$work/token" \
    >>"$work/relay.log" 2>&1 &
  relay_pid=$!
}

wait_connected() {
  for _ in $(seq 1 60); do
    if "$bin" internet status --json | grep -q '"state":"connected"'; then
      return 0
    fi
    sleep 1
  done
  "$bin" internet status || true
  echo "FAIL: not connected within 60 s" >&2
  exit 1
}

start_relay
"$bin" internet on --relay "$agents" --id local-test --token "$token"
if ! pgrep -f "internet serve" >/dev/null; then
  "$bin" internet serve >"$work/serve.log" 2>&1 &
  serve_pid=$!
fi
wait_connected
url=$("$bin" internet credentials --json | sed -n 's/.*"proxy":"\([^"]*\)".*/\1/p')

direct=$(curl -s https://api.ipify.org)
through=$(curl -s --noproxy '' -x "$url" https://api.ipify.org || true)
echo "direct:        $direct"
echo "through relay: $through"
[ -n "$through" ] || { echo "FAIL: nothing through the relay" >&2; exit 1; }

status=$(curl -s --noproxy '*' "$url/status")
echo "relay status:  $status"
case $status in *'"connected":true'*) ;; *) echo "FAIL: /status does not say connected" >&2; exit 1 ;; esac

echo "restarting the relay"
kill "$relay_pid"
wait "$relay_pid" 2>/dev/null || true
sleep 2
start_relay
# The PC sees the relay go and dials again (1 s, then 2 s, ...); a request
# before that gets 503.
again=
for i in $(seq 1 60); do
  again=$(curl -sf --noproxy '' -x "$url" https://api.ipify.org || true)
  if [ -n "$again" ]; then
    echo "after restart: $again (in about $i s)"
    break
  fi
  again=
  sleep 1
done
[ -n "$again" ] || { echo "FAIL: nothing through the relay within 60 s of its restart" >&2; exit 1; }

sleep 6
"$bin" internet status
echo "ok"

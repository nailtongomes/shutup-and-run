#!/usr/bin/env bash
# Internet sharing on Android, in Termux, without a phone: the Android build
# of the binary in termux/termux-docker (Android's own C library and
# Termux's files, on x86_64), with a relay on the container's loopback.
# `fit` and the worker's commands, `internet on --wait`, `status`,
# `credentials`, curl through the proxy, the bytes, a change of network
# (the container's network disconnected and connected again), and `off`.
# It proves the Android code paths, not a phone's radio, battery or IP:
# docs/INTERNET.md, "Android", has the test on a phone.
#
#   rustup target add x86_64-linux-android    # and the NDK: AGENTS.md
#   cargo build --release --locked --target x86_64-linux-android
#   bash tests/internet/termux.sh target/x86_64-linux-android/release/shutup-and-run
#
# Needs Docker and the internet. CA_BUNDLE=FILE gives curl inside the
# container a CA bundle, behind a proxy that inspects TLS.
set -euo pipefail

if [[ "${1:-}" != inside ]]; then
  bin=$(realpath "${1:?usage: termux.sh PATH_TO_ANDROID_x86_64_BINARY}")
  here=$(cd "$(dirname "$0")" && pwd)
  image=${TERMUX_IMAGE:-termux/termux-docker:x86_64}
  name=sar-termux-$$
  ca=()
  if [[ -n "${CA_BUNDLE:-}" ]]; then
    ca=(-v "$(realpath "$CA_BUNDLE"):/ca.crt:ro" -e CURL_CA_BUNDLE=/ca.crt)
  fi
  trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
  docker run -d --name "$name" -v "$bin:/sar:ro" -v "$here:/t:ro" "${ca[@]}" \
    "$image" sleep 900 >/dev/null
  phase() { docker exec "$name" bash /t/termux.sh inside "$@"; }
  phase on
  # A new address, as when a phone moves from Wi-Fi to mobile data: the
  # relay is on loopback and stays, the links are dialed again.
  docker network disconnect bridge "$name"
  phase offline
  docker network connect bridge "$name"
  phase back
  phase off
  echo "termux.sh: all passed"
  exit 0
fi

# Inside the container, as Termux's user.
step=$2
sar=$PREFIX/bin/shutup-and-run
fail() { echo "FAIL: $*" >&2; tail -n 20 "$HOME/.shutup-and-run/internet.log" >&2 || true; exit 1; }
pass() { echo "PASS: $*"; }
proxy() { "$sar" internet credentials --env | sed -n 's/^HTTPS_PROXY=//p'; }
field() { "$sar" internet status --json | sed -n "s/.*\"$1\":\"\\{0,1\\}\\([^,\"}]*\\).*/\\1/p" | head -n 1; }
# Waits up to $1 seconds for `internet status --json` to contain $2.
wait_for() {
  for _ in $(seq 1 "$1"); do
    "$sar" internet status --json | grep -q "$2" && return 0
    sleep 1
  done
  return 1
}

case $step in
on)
  cp /sar "$sar" && chmod 755 "$sar"
  "$sar" fit --json | grep -q '"lend":"internet"' || fail "fit lends only the internet"
  "$sar" fit --json | grep -q ANDROID_NO_CONTAINERS || fail "fit names the reason"
  pass "fit: only the internet connection, ANDROID_NO_CONTAINERS"
  if "$sar" serve 2>err.txt; then fail "serve ran on Android"; fi
  grep -q "not on Android" err.txt || fail "serve says why: $(cat err.txt)"
  if "$sar" wsl doctor 2>/dev/null; then fail "wsl ran on Android"; fi
  "$sar" status | grep -q "worker: not installed" || fail "status before on"
  pass "serve, wsl: refused with a reason; status: no worker"

  "$sar" relay --agents 127.0.0.1:7000 --proxy 127.0.0.1:8080 >"$HOME/relay.log" 2>&1 &
  sleep 1
  out=$("$sar" internet on --relay 127.0.0.1:7000 --wait 30 --json) || fail "on: $out"
  grep -q '"connected":true' <<<"$out" || fail "on --wait: $out"
  pass "internet on --relay --wait: connected"
  [[ -x "$HOME/.termux/boot/shutup-and-run-internet" ]] || fail "no Termux:Boot script"
  grep -q "internet serve --background" "$HOME/.termux/boot/shutup-and-run-internet" ||
    fail "the boot script runs the process"
  [[ "$(stat -c %a "$HOME/.shutup-and-run/internet.json")" == 600 ]] || fail "internet.json mode"
  pass "the Termux:Boot script, internet.json in ~/.shutup-and-run with mode 600"
  wait_for 15 '"network":{' || fail "no network in the status"
  "$sar" internet status | grep -q "^network: cable (eth0); shares only on Wi-Fi" ||
    fail "status names the network: $("$sar" internet status)"
  pass "internet status: network cable (eth0), shares only on Wi-Fi"

  p=$(proxy)
  [[ "$p" == http://*:*@127.0.0.1:8080 ]] || fail "credentials --env: ${p%%:*}..."
  ip=$(curl -sS -m 30 -x "$p" http://api.ipify.org) || fail "curl http through the proxy"
  [[ "$ip" =~ ^[0-9.]+$|: ]] || fail "not an IP: $ip"
  code=$(curl -sS -m 30 -o /dev/null -w '%{http_code}' -x "$p" https://example.com) ||
    fail "curl https through the proxy"
  [[ "$code" == 200 ]] || fail "https through the proxy: $code"
  pass "curl through the relay and the phone: http ($ip) and https"
  wait_for 15 '"bytes_received":[1-9]' || fail "bytes not counted"
  pass "bytes counted: $(field bytes_received) received, $(field bytes_sent) sent"
  ;;
offline)
  wait_for 20 '"kind":"offline"' || fail "the status never said offline"
  pass "no network: the status says offline"
  ;;
back)
  wait_for 20 '"kind":"ethernet"' || fail "the network never came back"
  grep -q "network offline -> cable" "$HOME/.shutup-and-run/internet.log" ||
    fail "the change is not in the log"
  wait_for 20 '"state":"connected"' || fail "not connected after the change"
  code=$(curl -sS -m 30 -o /dev/null -w '%{http_code}' -x "$(proxy)" https://example.com) ||
    fail "curl after the change"
  [[ "$code" == 200 ]] || fail "after the change: $code"
  pass "network back: dialed again, connected, curl works"
  ;;
off)
  "$sar" internet off | grep -q "stops within 5 seconds" || fail "off"
  [[ ! -e "$HOME/.termux/boot/shutup-and-run-internet" ]] || fail "the boot script stayed"
  for _ in $(seq 1 15); do
    [[ -e "$HOME/.shutup-and-run/internet-status.json" ]] || break
    sleep 1
  done
  [[ ! -e "$HOME/.shutup-and-run/internet-status.json" ]] || fail "the process did not stop"
  "$sar" internet status | grep -q "ok   internet               off" || fail "status after off"
  pass "internet off: the process stopped, the boot script removed"
  ;;
esac

#!/usr/bin/env bash
# Internet sharing on Android, in Termux, without a phone: the Android build
# of the binary in termux/termux-docker (Android's own C library and
# Termux's files, on x86_64), with a relay on the container's loopback.
# scripts/install-termux.sh (a wrong checksum refused, then the install and
# `internet on` from its variables), `fit` and the worker's commands,
# `status`, `credentials`, curl through the proxy, the bytes, a change of
# network (the container's network disconnected and connected again), mobile
# data (its interface renamed rmnet0: new streams wait, until
# `--mobile-data`), a battery below 60% (a `termux-battery-status` that
# prints one), a new version stopping the old process, the process killed
# and started again by its boot script, and `off`.
# It proves the Android code paths, not a phone's radio, battery or IP:
# docs/INTERNET.md, "Android", has the test on a phone.
#
#   rustup target add x86_64-linux-android    # and the NDK: AGENTS.md
#   cargo build --release --locked --target x86_64-linux-android
#   bash tests/internet/termux.sh target/x86_64-linux-android/release/shutup-and-run
#
# Needs Docker, `ip` and `nsenter` (as root, or sudo) and the internet. About
# five minutes: the battery is read once a minute. CA_BUNDLE=FILE gives curl inside the
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
  # A release as install-termux.sh downloads it, and one whose sum is wrong.
  dist=$(mktemp -d)
  trap 'docker rm -f "$name" >/dev/null 2>&1 || true; rm -rf "$dist"' EXIT
  mkdir -p "$dist/good" "$dist/bad"
  for d in good bad; do cp "$bin" "$dist/$d/shutup-and-run-x86_64-linux-android"; done
  (cd "$dist/good" && sha256sum shutup-and-run-x86_64-linux-android >SHA256SUMS)
  echo "$(printf '0%.0s' {1..64})  shutup-and-run-x86_64-linux-android" >"$dist/bad/SHA256SUMS"
  chmod -R a+rX "$dist"
  docker run -d --name "$name" -v "$dist:/dist:ro" -v "$here:/t:ro" \
    -v "$here/../../scripts:/s:ro" "${ca[@]}" "$image" sleep 900 >/dev/null
  phase() { docker exec "$name" bash /t/termux.sh inside "$@"; }
  sudo=()
  [[ $EUID -eq 0 ]] || sudo=(sudo)
  netns() { "${sudo[@]}" nsenter -t "$(docker inspect -f '{{.State.Pid}}' "$name")" -n "$@"; }
  # The container's interface renamed, as a phone's modem is named.
  rename() {
    local gw
    gw=$(netns ip -4 route show default | awk '{ print $3; exit }')
    netns ip link set "$1" down
    netns ip link set "$1" name "$2"
    netns ip link set "$2" up
    netns ip route add default via "$gw" dev "$2"
  }
  phase on
  # A new address, as when a phone moves from Wi-Fi to mobile data: the
  # relay is on loopback and stays, the links are dialed again.
  docker network disconnect bridge "$name"
  phase offline
  docker network connect bridge "$name"
  phase back
  rename eth0 rmnet0
  phase mobile
  rename rmnet0 eth0
  phase battery
  phase restart
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

pid() { sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$HOME/.shutup-and-run/internet-status.json"; }
asset=shutup-and-run-x86_64-linux-android

case $step in
on)
  if SHUTUP_AND_RUN_URL=file:///dist/bad/$asset bash /s/install-termux.sh 2>err.txt; then
    fail "install-termux.sh took a wrong checksum"
  fi
  grep -q "checksum mismatch" err.txt || fail "the refusal says why: $(cat err.txt)"
  [[ ! -e "$sar" ]] || fail "something was installed"
  SHUTUP_AND_RUN_URL=file:///dist/good/$asset bash /s/install-termux.sh | grep -q "^next: " ||
    fail "install without a relay"
  "$sar" version | grep -q "^shutup-and-run " || fail "the installed binary does not run"
  pass "install-termux.sh: a wrong checksum refused, then installed"
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
  out=$(SHUTUP_AND_RUN_URL=file:///dist/good/$asset SHUTUP_AND_RUN_RELAY=127.0.0.1:7000 \
    SHUTUP_AND_RUN_MAX_DAY=1g bash /s/install-termux.sh) || fail "install and on: $out"
  grep -q "connected to 127.0.0.1:7000" <<<"$out" || fail "on --wait: $out"
  "$sar" internet status --json | grep -q '"max_day_bytes":1073741824' || fail "the variables reach on"
  pass "install-termux.sh with SHUTUP_AND_RUN_RELAY: internet on --wait, connected"
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
mobile)
  wait_for 20 '"kind":"mobile"' || fail "rmnet0 is not mobile data"
  wait_for 10 INTERNET_NOT_ON_WIFI || fail "no wait on mobile data"
  "$sar" internet status | grep -q "^network: mobile data (rmnet0); shares only on Wi-Fi" ||
    fail "status on mobile data: $("$sar" internet status)"
  code=$(curl -sS -m 30 -o /dev/null -w '%{http_code}' -x "$(proxy)" https://example.com 2>/dev/null) || true
  [[ "$code" != 200 ]] || fail "a stream went out on mobile data"
  pass "on mobile data (rmnet0): new streams wait, INTERNET_NOT_ON_WIFI"
  out=$("$sar" internet on --relay 127.0.0.1:7000 --mobile-data --wait 30) || fail "on --mobile-data: $out"
  wait_for 15 '"state":"connected"' || fail "not connected with --mobile-data"
  code=$(curl -sS -m 30 -o /dev/null -w '%{http_code}' -x "$(proxy)" https://example.com) ||
    fail "curl on mobile data"
  [[ "$code" == 200 ]] || fail "on mobile data with --mobile-data: $code"
  pass "--mobile-data, given at the phone: it shares on mobile data"
  ;;
battery)
  # Without Termux:API nothing is read, and status says so.
  wait_for 15 '"battery_unread":"TERMUX_API_MISSING"' ||
    fail "no TERMUX_API_MISSING without termux-battery-status: $("$sar" internet status --json)"
  "$sar" internet status | grep -q "not read (Termux:API is not installed)" ||
    fail "status says the battery is not read: $("$sar" internet status)"
  pass "without Termux:API: the battery is not read, and status says so"
  # Termux:API's program, as on a phone at 45% off its charger.
  fake=$PREFIX/bin/termux-battery-status
  printf '#!%s/bin/sh\ncat %s\n' "$PREFIX" "$HOME/battery.json" >"$fake"
  chmod 755 "$fake"
  echo '{"percentage":45,"plugged":"UNPLUGGED","status":"DISCHARGING"}' >"$HOME/battery.json"
  wait_for 75 BATTERY_LOW || fail "no wait at 45% on the battery"
  "$sar" internet status | grep -q "^battery: 45%, on its battery" ||
    fail "status names the battery: $("$sar" internet status)"
  pass "45% on its battery: new streams wait, BATTERY_LOW"
  echo '{"percentage":45,"plugged":"PLUGGED_AC","status":"CHARGING"}' >"$HOME/battery.json"
  wait_for 75 '"plugged":true' || fail "the charger was not seen"
  wait_for 15 '"state":"connected"' || fail "not connected on the charger"
  code=$(curl -sS -m 30 -o /dev/null -w '%{http_code}' -x "$(proxy)" https://example.com) ||
    fail "curl on the charger"
  [[ "$code" == 200 ]] || fail "on the charger: $code"
  pass "on a charger: it shares again"
  ;;
restart)
  # What an update looks like to `on`: the process is of another version,
  # and it wrote `connected` in the second `on` starts (or later), which
  # `--wait` must not take as the new process's answer.
  old=$(pid)
  status=$HOME/.shutup-and-run/internet-status.json
  later=$(date -u -d @$(($(date +%s) + 30)) +%Y-%m-%dT%H:%M:%SZ)
  sed -e 's/"version": "[^"]*"/"version": "0.0.1"/' \
    -e "s/\"updated_at\": \"[^\"]*\"/\"updated_at\": \"$later\"/" "$status" >"$status.new" &&
    mv "$status.new" "$status"
  grep -q "\"updated_at\": \"$later\"" "$status" || fail "the status file's form changed"
  out=$("$sar" internet on --relay 127.0.0.1:7000 --wait 30) || fail "on after an update: $out"
  grep -q "stopped 0.0.1 (pid $old)" <<<"$out" || fail "the old version was not stopped: $out"
  wait_for 15 '"state":"connected"' || fail "not connected after the update"
  [[ "$(pid)" != "$old" ]] || fail "the same process after the update"
  pass "on after an update: the old process stopped, a new one connected"
  # Killed (a crash, or Android's phantom process killer on the process
  # alone): the boot script starts it again a minute later.
  old=$(pid)
  kill -9 "$old"
  for _ in $(seq 1 80); do
    now=$(pid)
    [[ -n "$now" && "$now" != "$old" ]] && "$sar" internet status --json | grep -q '"state":"connected"' && break
    sleep 1
  done
  [[ "$(pid)" != "$old" ]] || fail "not started again after kill -9"
  "$sar" internet status | grep -q "connected to" || fail "not connected after the restart"
  pass "killed: started again by its boot script, connected"
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

#!/usr/bin/env bash
# Internet sharing on an Android phone, in one line, in Termux (no root):
#
#   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install-termux.sh \
#     | SHUTUP_AND_RUN_RELAY=relay.example.com:7000 SHUTUP_AND_RUN_RELAY_TOKEN=TOKEN bash
#
# Downloads the release's Android binary and SHA256SUMS, checks one against
# the other, installs it as $PREFIX/bin/shutup-and-run and, with
# SHUTUP_AND_RUN_RELAY, runs `internet on --wait 60`: it returns once the
# phone is connected, or says why not. Run again, it is the update: `on`
# stops the old version and starts the new one.
#
#   SHUTUP_AND_RUN_RELAY          HOST:PORT of the operator's relay
#   SHUTUP_AND_RUN_RELAY_TOKEN    what the relay asks of every PC, when it asks
#   SHUTUP_AND_RUN_INTERNET_LOGIN ID:PASSWORD the operator chose
#   SHUTUP_AND_RUN_MOBILE_DATA=1  share mobile data too, if the person agreed
#   SHUTUP_AND_RUN_MAX_DAY        at most this much a day, like 2g
#   SHUTUP_AND_RUN_URL            another binary (tests); SHA256SUMS beside it
#
# docs/INTERNET.md, "Android", has the rest: Termux:Boot, the battery savers.
set -euo pipefail

fail() {
  echo "install-termux.sh: $*" >&2
  exit 1
}

[[ -n "${PREFIX:-}" && -d "$PREFIX/bin" ]] || fail "run it in Termux (PREFIX is not set)"
url=${SHUTUP_AND_RUN_URL:-}
if [[ -z "$url" ]]; then
  [[ "$(uname -m)" == aarch64 ]] ||
    fail "the Android binary is for 64-bit ARM (aarch64); this phone is $(uname -m)"
  url=https://github.com/nailtongomes/shutup-and-run/releases/latest/download/shutup-and-run-aarch64-linux-android
fi
asset=$(basename "$url")
dir=$(mktemp -d "${TMPDIR:-$PREFIX/tmp}/shutup-and-run.XXXXXX")
trap 'rm -rf "$dir"' EXIT
curl -fsSL "$url" -o "$dir/$asset"
curl -fsSL "${url%/*}/SHA256SUMS" -o "$dir/SHA256SUMS"
expected=$(awk -v asset="$asset" '{ name = $2; sub(/^.*\//, "", name); if (name == asset) { print $1; exit } }' "$dir/SHA256SUMS")
actual=$(sha256sum "$dir/$asset" | awk '{ print $1 }')
if [[ -z "$expected" || "$expected" != "$actual" ]]; then
  fail "checksum mismatch for $asset: nothing was installed"
fi
install -m 755 "$dir/$asset" "$PREFIX/bin/shutup-and-run"
echo "installed $("$PREFIX/bin/shutup-and-run" version)"

if [[ -z "${SHUTUP_AND_RUN_RELAY:-}" ]]; then
  echo "next: shutup-and-run internet on --relay HOST:PORT --wait 60"
  exit 0
fi
args=(internet on --relay "$SHUTUP_AND_RUN_RELAY" --wait 60)
[[ -n "${SHUTUP_AND_RUN_RELAY_TOKEN:-}" ]] && args+=(--token "$SHUTUP_AND_RUN_RELAY_TOKEN")
[[ -n "${SHUTUP_AND_RUN_INTERNET_LOGIN:-}" ]] && args+=(--login "$SHUTUP_AND_RUN_INTERNET_LOGIN")
[[ "${SHUTUP_AND_RUN_MOBILE_DATA:-}" == 1 ]] && args+=(--mobile-data)
[[ -n "${SHUTUP_AND_RUN_MAX_DAY:-}" ]] && args+=(--max-day "$SHUTUP_AND_RUN_MAX_DAY")
exec "$PREFIX/bin/shutup-and-run" "${args[@]}"

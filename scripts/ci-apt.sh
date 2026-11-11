#!/usr/bin/env bash
# Install apt packages in GitHub Actions.
#
# ubuntu-latest resolves Ubuntu archives through azure.archive.ubuntu.com
# (directly, or via mirror+file:/etc/apt/apt-mirrors.txt). When that name does
# not resolve, apt retries for many minutes and the job fails. Each apt
# invocation is capped, and on failure the Azure mirror is removed so the
# retry talks to archive.ubuntu.com only.
#
# CI_APT_TIMEOUT_SECS   per apt-get invocation (default 90)
# CI_APT_FORCE_FALLBACK set to 1 to skip the default mirror
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: bash scripts/ci-apt.sh <package>..." >&2
  exit 1
fi

TIMEOUT="${CI_APT_TIMEOUT_SECS:-90}"

apt_get() {
  sudo env DEBIAN_FRONTEND=noninteractive \
    timeout --kill-after=10s "$TIMEOUT" \
    apt-get \
    -o Acquire::Retries=2 \
    -o Acquire::http::Timeout=20 \
    -o Acquire::https::Timeout=20 \
    "$@"
}

try_install() {
  apt_get update || return $?
  apt_get install -y "$@" || return $?
}

# Point every Ubuntu source at archive.ubuntu.com and drop the Azure mirror
# list. A runner that never mentioned Azure is left as apt already had it,
# plus a direct archive line if we had to rewrite something.
fallback_mirror() {
  echo "apt failed or timed out; installing from archive.ubuntu.com" >&2
  if [[ -f /etc/apt/apt-mirrors.txt ]]; then
    echo 'https://archive.ubuntu.com/ubuntu/' | sudo tee /etc/apt/apt-mirrors.txt >/dev/null
  fi
  local f
  local changed=0
  if [[ -f /etc/apt/sources.list ]]; then
    if grep -qE 'azure\.archive\.ubuntu\.com|apt-mirrors\.txt' /etc/apt/sources.list; then
      sudo sed -i \
        -e 's|http://azure.archive.ubuntu.com/ubuntu|https://archive.ubuntu.com/ubuntu|g' \
        -e 's|https://azure.archive.ubuntu.com/ubuntu|https://archive.ubuntu.com/ubuntu|g' \
        -e 's|mirror+file:/etc/apt/apt-mirrors.txt|https://archive.ubuntu.com/ubuntu|g' \
        /etc/apt/sources.list
      changed=1
    fi
  fi
  for f in /etc/apt/sources.list.d/*; do
    [[ -f "$f" ]] || continue
    if grep -qE 'azure\.archive\.ubuntu\.com|apt-mirrors\.txt' "$f"; then
      sudo sed -i \
        -e 's|http://azure.archive.ubuntu.com/ubuntu|https://archive.ubuntu.com/ubuntu|g' \
        -e 's|https://azure.archive.ubuntu.com/ubuntu|https://archive.ubuntu.com/ubuntu|g' \
        -e 's|mirror+file:/etc/apt/apt-mirrors.txt|https://archive.ubuntu.com/ubuntu|g' \
        "$f"
      changed=1
    fi
  done
  if [[ "$changed" -eq 0 && ! -f /etc/apt/sources.list.d/ci-direct.sources ]]; then
    # The failure was not a mirror name we know. Add one direct source so the
    # retry is not stuck on whatever just timed out.
    local codename
    codename=$(awk -F= '/^VERSION_CODENAME=/ { print $2 }' /etc/os-release)
    sudo tee /etc/apt/sources.list.d/ci-direct.sources >/dev/null <<EOF
Types: deb
URIs: https://archive.ubuntu.com/ubuntu
Suites: ${codename} ${codename}-updates ${codename}-backports ${codename}-security
Components: main universe restricted multiverse
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
EOF
  fi
}

if [[ "${CI_APT_FORCE_FALLBACK:-}" != 1 ]]; then
  if try_install "$@"; then
    exit 0
  fi
fi
fallback_mirror
try_install "$@"

#!/usr/bin/env bash
# Install apt packages in GitHub Actions.
#
# ubuntu-latest resolves Ubuntu archives through azure.archive.ubuntu.com
# (directly, or via mirror+file:/etc/apt/apt-mirrors.txt). When that name does
# not resolve, apt retries for many minutes and the job fails. Each apt
# invocation is capped. On failure the Azure mirror is dropped and the same
# install is tried against archive.ubuntu.com, then mirrors.kernel.org.
# One dead Ubuntu host is not enough to fail the job.
#
# CI_APT_TIMEOUT_SECS   per apt-get invocation (default 90)
# CI_APT_FORCE_FALLBACK set to 1 to skip the runner's default mirror
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

# Rewrite Ubuntu sources so they fetch $1 and nothing else we were stuck on.
point_apt_at() {
  local uri=$1
  local f
  local changed=0
  echo "apt failed or timed out; installing from ${uri}" >&2
  if [[ -f /etc/apt/apt-mirrors.txt ]]; then
    printf '%s/\n' "$uri" | sudo tee /etc/apt/apt-mirrors.txt >/dev/null
  fi
  for f in /etc/apt/sources.list /etc/apt/sources.list.d/*; do
    [[ -f "$f" ]] || continue
    if grep -qE 'azure\.archive\.ubuntu\.com|apt-mirrors\.txt|archive\.ubuntu\.com/ubuntu|mirrors\.kernel\.org/ubuntu' "$f"; then
      sudo sed -i \
        -e "s|http://azure.archive.ubuntu.com/ubuntu|${uri}|g" \
        -e "s|https://azure.archive.ubuntu.com/ubuntu|${uri}|g" \
        -e "s|http://archive.ubuntu.com/ubuntu|${uri}|g" \
        -e "s|https://archive.ubuntu.com/ubuntu|${uri}|g" \
        -e "s|http://mirrors.kernel.org/ubuntu|${uri}|g" \
        -e "s|https://mirrors.kernel.org/ubuntu|${uri}|g" \
        -e "s|mirror+file:/etc/apt/apt-mirrors.txt|${uri}|g" \
        "$f"
      changed=1
    fi
  done
  if [[ "$changed" -eq 0 ]]; then
    local codename
    codename=$(awk -F= '/^VERSION_CODENAME=/ { print $2 }' /etc/os-release)
    sudo tee /etc/apt/sources.list.d/ci-direct.sources >/dev/null <<EOF
Types: deb
URIs: ${uri}
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

for uri in https://archive.ubuntu.com/ubuntu https://mirrors.kernel.org/ubuntu; do
  point_apt_at "$uri"
  if try_install "$@"; then
    exit 0
  fi
done
exit 1

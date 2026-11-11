#!/usr/bin/env bash
# Install shutup-and-run on Ubuntu.
#
# Online, after you publish a release (replace OWNER):
#   curl -fsSL https://raw.githubusercontent.com/OWNER/shutup-and-run/main/scripts/install.sh \
#     | sudo SHUTUP_AND_RUN_URL=https://github.com/OWNER/shutup-and-run/releases/latest/download/shutup-and-run-x86_64-unknown-linux-musl bash
#
# Offline, with a binary you already have:
#   sudo SHUTUP_AND_RUN_BIN=./shutup-and-run bash scripts/install.sh
#
# Show the token once:  sudo SHUTUP_AND_RUN_SHOW_TOKEN=1 bash scripts/install.sh
# Skip ufw:             sudo SHUTUP_AND_RUN_NO_UFW=1 bash scripts/install.sh
# Remove it:            sudo bash scripts/install.sh --uninstall
# Remove config too:    sudo bash scripts/install.sh --uninstall --purge
set -euo pipefail

PREFIX=/usr/local/bin
NAME=shutup-and-run
ETC=/etc/shutup-and-run
LIB=/var/lib/shutup-and-run
UNIT=/etc/systemd/system/shutup-and-run.service
PORT="${SHUTUP_AND_RUN_PORT:-8787}"

if [[ "$(id -u)" -ne 0 ]]; then
  echo "run as root (sudo)" >&2
  exit 1
fi

if [[ "${1:-}" == "--uninstall" ]]; then
  systemctl disable --now "$NAME" >/dev/null 2>&1 || true
  rm -f "$UNIT" "$PREFIX/$NAME"
  systemctl daemon-reload >/dev/null 2>&1 || true
  if [[ "${2:-}" == "--purge" ]]; then
    rm -rf "$ETC" "$LIB"
    echo "removed $NAME, its config, and its data"
  else
    echo "removed $NAME. kept $ETC and $LIB (pass --purge to delete them)"
  fi
  exit 0
fi

export DEBIAN_FRONTEND=noninteractive
if ! command -v docker >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1; then
  apt-get update
  apt-get install -y docker.io openssl curl ca-certificates
fi
if ! docker info >/dev/null 2>&1; then
  systemctl enable --now docker
  docker info >/dev/null
fi

install_bin() {
  if [[ -n "${SHUTUP_AND_RUN_BIN:-}" ]]; then
    install -m 0755 "$SHUTUP_AND_RUN_BIN" "$PREFIX/$NAME"
    return
  fi
  if [[ -n "${SHUTUP_AND_RUN_URL:-}" ]]; then
    tmp=$(mktemp)
    curl -fsSL "$SHUTUP_AND_RUN_URL" -o "$tmp"
    install -m 0755 "$tmp" "$PREFIX/$NAME"
    rm -f "$tmp"
    return
  fi
  echo "set SHUTUP_AND_RUN_BIN to a local binary, or SHUTUP_AND_RUN_URL to the musl release asset" >&2
  exit 1
}
install_bin

cpus=$(nproc)
mem_kb=$(awk '/MemTotal:/ {print $2}' /proc/meminfo)
mem_gb=$((mem_kb / 1024 / 1024))
by_cpu=$cpus
if [[ "$cpus" -gt 1 ]]; then
  by_cpu=$((cpus - 1))
fi
by_mem=$((mem_gb / 2))
if [[ "$by_mem" -lt 1 ]]; then
  by_mem=1
fi
slots=$by_cpu
if [[ "$by_mem" -lt "$slots" ]]; then
  slots=$by_mem
fi
if [[ "$slots" -lt 1 ]]; then
  slots=1
fi
if [[ "$slots" -gt 16 ]]; then
  slots=16
fi
if [[ -n "${SHUTUP_AND_RUN_MAX_SLOTS:-}" ]]; then
  slots=$SHUTUP_AND_RUN_MAX_SLOTS
fi
echo "suggested max_slots=${slots} (${cpus} CPUs, ${mem_gb} GiB RAM; about 1 CPU and 2 GiB per slot)"

mkdir -p "$ETC" "$LIB"
chmod 700 "$ETC" "$LIB"

if [[ ! -f "$ETC/env" ]] || ! grep -q '^SHUTUP_AND_RUN_TOKEN=.\+' "$ETC/env"; then
  umask 077
  printf 'SHUTUP_AND_RUN_TOKEN=%s\n' "$(openssl rand -hex 32)" >"$ETC/env"
  chmod 600 "$ETC/env"
fi

ip=$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "src") { print $(i + 1); exit }}')
if [[ -z "${ip}" ]]; then
  ip=127.0.0.1
fi

if [[ ! -f "$ETC/cert.pem" || ! -f "$ETC/key.pem" ]]; then
  cat >"$ETC/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = shutup-and-run
[v3]
subjectAltName = DNS:localhost,IP:127.0.0.1,IP:${ip}
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
EOF
  openssl req -x509 -newkey rsa:2048 \
    -keyout "$ETC/key.pem" -out "$ETC/cert.pem" \
    -days 825 -nodes -config "$ETC/openssl.cnf" >/dev/null 2>&1
  chmod 600 "$ETC/key.pem"
  chmod 644 "$ETC/cert.pem"
fi
fingerprint=$(openssl x509 -in "$ETC/cert.pem" -noout -fingerprint -sha256 | sed 's/^[^=]*=//')

if [[ ! -f "$ETC/config.toml" ]]; then
  umask 077
  cat >"$ETC/config.toml" <<EOF
listen = "0.0.0.0:${PORT}"
max_slots = ${slots}
allowlist = [
  "ghcr.io/owner/selenium-worker",
]
tls_cert = "${ETC}/cert.pem"
tls_key = "${ETC}/key.pem"
history_path = "${LIB}/jobs.sqlite"
EOF
  chmod 600 "$ETC/config.toml"
else
  echo "kept existing $ETC/config.toml"
fi

cat >"$UNIT" <<EOF
[Unit]
Description=shutup-and-run — on-demand container slots
After=docker.service
Requires=docker.service

[Service]
Type=simple
ExecStart=${PREFIX}/${NAME}
Environment=SHUTUP_AND_RUN_CONFIG=${ETC}/config.toml
Environment=HOME=/root
EnvironmentFile=-${ETC}/env
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable "$NAME" >/dev/null
systemctl restart "$NAME"

if [[ "${SHUTUP_AND_RUN_NO_UFW:-}" != 1 ]] && command -v ufw >/dev/null 2>&1; then
  if ufw status | grep -q 'Status: active'; then
    ufw allow "${PORT}/tcp" || true
  fi
fi

token=$(sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' "$ETC/env" | head -n 1)
health=0
for _ in $(seq 1 50); do
  if curl -fsS --cacert "$ETC/cert.pem" \
    -H "Authorization: Bearer ${token}" \
    "https://127.0.0.1:${PORT}/health" >/dev/null; then
    health=1
    break
  fi
  sleep 0.2
done

echo
echo "shutup-and-run"
echo "  url:           https://${ip}:${PORT}"
echo "  token file:    ${ETC}/env"
echo "  certificate:   ${ETC}/cert.pem"
echo "  fingerprint:   ${fingerprint}"
echo "  config:        ${ETC}/config.toml"
if [[ "$health" == 1 ]]; then
  echo "  health:        ok"
else
  echo "  health:        not answering (journalctl -u ${NAME} -n 40)"
  exit 1
fi
if [[ "${SHUTUP_AND_RUN_SHOW_TOKEN:-}" == 1 ]]; then
  echo "  token:         ${token}"
fi
echo
echo "Pin the fingerprint, or trust ${ETC}/cert.pem."
echo "Kestra: options.ssl.insecureTrustAllCertificates: true"
echo "n8n: allowUnauthorizedCerts: true on the HTTP Request node"

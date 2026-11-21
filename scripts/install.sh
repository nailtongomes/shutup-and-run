#!/usr/bin/env bash
# Install shutup-and-run on Ubuntu.
#
# Online:
#   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash
#
# Offline, with a binary you already have:
#   sudo SHUTUP_AND_RUN_BIN=./shutup-and-run bash scripts/install.sh
#
# Allowlist on first install (comma-separated; later, edit config.toml):
#   curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
#
# Show the token once:  sudo SHUTUP_AND_RUN_SHOW_TOKEN=1 bash scripts/install.sh
# Skip ufw:             sudo SHUTUP_AND_RUN_NO_UFW=1 bash scripts/install.sh
# Lend the internet connection too, through the operator's relay (made-up values):
#   curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_RELAY=relay.example.com:7000 \
#     SHUTUP_AND_RUN_RELAY_TOKEN=<token> SHUTUP_AND_RUN_INTERNET_LOGIN=<id>:<password> bash
# Or let your poll URL say where the relay is (docs/INTEGRATION-CONTRACT.md):
#   curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_LEND_INTERNET=1 \
#     SHUTUP_AND_RUN_POLL_URL=https://orchestrator.example/poll bash
# Only the internet connection, no Docker and no worker: add SHUTUP_AND_RUN_INTERNET_ONLY=1.
# The operator's relay for internet sharing, as the systemd unit shutup-and-run-relay
# (no Docker needed; PCs dial in on 7000, containers use the proxy on Docker's
# bridge address, 172.17.0.1:8080; docs/SERVIDOR.pt-BR.md):
#   curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 bash
# Run the plain line too, before or after, for a worker on the same server.
# Remove it:            sudo bash scripts/install.sh --uninstall
# Remove config too:    sudo bash scripts/install.sh --uninstall --purge
set -euo pipefail

PREFIX=/usr/local/bin
NAME=shutup-and-run
ETC=/etc/shutup-and-run
LIB=/var/lib/shutup-and-run
UNIT=/etc/systemd/system/shutup-and-run.service
RELAY_UNIT=/etc/systemd/system/shutup-and-run-relay.service
PORT="${SHUTUP_AND_RUN_PORT:-8787}"

if [[ "$(id -u)" -ne 0 ]]; then
  echo "run as root (sudo)" >&2
  exit 1
fi

if [[ "${1:-}" == "--uninstall" ]]; then
  if [[ -x "$PREFIX/$NAME" ]]; then
    "$PREFIX/$NAME" internet off >/dev/null 2>&1 || true
  fi
  systemctl disable --now "$NAME" "$NAME-relay" >/dev/null 2>&1 || true
  rm -f "$UNIT" "$RELAY_UNIT" "$PREFIX/$NAME"
  systemctl daemon-reload >/dev/null 2>&1 || true
  if [[ "${2:-}" == "--purge" ]]; then
    rm -rf "$ETC" "$LIB"
    echo "removed $NAME, its config, and its data"
  else
    echo "removed $NAME. kept $ETC and $LIB (pass --purge to delete them)"
  fi
  exit 0
fi

# The person's yes to lending the internet: a relay given, or the poll URL
# to ask for one.
lend=
if [[ -n "${SHUTUP_AND_RUN_RELAY:-}" ]]; then
  lend=relay
elif [[ "${SHUTUP_AND_RUN_LEND_INTERNET:-}" == 1 && -n "${SHUTUP_AND_RUN_POLL_URL:-}" ]]; then
  lend=discover
fi
internet_only=
if [[ -n "$lend" && "${SHUTUP_AND_RUN_INTERNET_ONLY:-}" == 1 ]]; then
  internet_only=1
fi
relay_server=
if [[ "${SHUTUP_AND_RUN_RELAY_SERVER:-}" == 1 ]]; then
  relay_server=1
fi

export DEBIAN_FRONTEND=noninteractive
if [[ -n "$internet_only" || -n "$relay_server" ]]; then
  if ! command -v curl >/dev/null 2>&1; then
    apt-get update
    apt-get install -y curl ca-certificates
  fi
else
  if ! command -v docker >/dev/null 2>&1 || ! command -v openssl >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1; then
    apt-get update
    apt-get install -y docker.io openssl curl ca-certificates
  fi
  if ! docker info >/dev/null 2>&1; then
    systemctl enable --now docker
    docker info >/dev/null
  fi
fi

install_bin() {
  if [[ -n "${SHUTUP_AND_RUN_BIN:-}" ]]; then
    install -m 0755 "$SHUTUP_AND_RUN_BIN" "$PREFIX/$NAME"
    return
  fi
  local url asset dir tmp expected actual
  url="${SHUTUP_AND_RUN_URL:-https://github.com/nailtongomes/shutup-and-run/releases/latest/download/shutup-and-run-x86_64-unknown-linux-musl}"
  asset=$(basename "$url")
  dir=$(mktemp -d)
  tmp="$dir/$asset"
  curl -fsSL "$url" -o "$tmp"
  curl -fsSL "${url%/*}/SHA256SUMS" -o "$dir/SHA256SUMS"
  # GNU sha256sum writes "<hash>  <filename>". v0.1.0 recorded the path
  # dist/<filename> because the hash was taken from outside dist/. Match the
  # base name so that release still installs. A wrong hash still stops us.
  expected=$(awk -v asset="$asset" '
    {
      name = $2
      sub(/^.*\//, "", name)
      if (name == asset) { print $1; exit }
    }
  ' "$dir/SHA256SUMS")
  actual=$(sha256sum "$tmp" | awk '{ print $1 }')
  if [[ -z "$expected" || "$expected" != "$actual" ]]; then
    echo "checksum mismatch for ${asset}" >&2
    rm -rf "$dir"
    exit 1
  fi
  install -m 0755 "$tmp" "$PREFIX/$NAME"
  rm -rf "$dir"
}
install_bin
# The relay runs this same binary: an update starts it again on the new one.
if [[ -f "$RELAY_UNIT" && -z "$relay_server" ]]; then
  systemctl restart "$NAME-relay" || true
fi

# The operator's relay: a token, a unit, and what to tell the clients.
if [[ -n "$relay_server" ]]; then
  mkdir -p "$ETC"
  chmod 700 "$ETC"
  if [[ ! -s "$ETC/relay_token" ]]; then
    umask 077
    head -c 24 /dev/urandom | base64 | tr -d '/+=\n' >"$ETC/relay_token"
    chmod 600 "$ETC/relay_token"
  fi
  # The proxy on Docker's bridge: the worker's containers reach it, the
  # internet does not. Without Docker yet, on loopback: run this again
  # after installing the worker.
  bridge=$(ip -4 -o addr show docker0 2>/dev/null | awk '{ split($4, a, "/"); print a[1]; exit }' || true)
  if [[ -z "$bridge" ]] && command -v docker >/dev/null 2>&1; then
    bridge=$(docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null || true)
  fi
  proxy="${SHUTUP_AND_RUN_RELAY_PROXY:-${bridge:-127.0.0.1}:8080}"
  agents="${SHUTUP_AND_RUN_RELAY_AGENTS:-0.0.0.0:7000}"
  run=("$PREFIX/$NAME" relay --agents "$agents" --proxy "$proxy" --token-file "$ETC/relay_token")
  if [[ -d /run/systemd/system ]]; then
    cat >"$RELAY_UNIT" <<UNIT_END
[Unit]
Description=shutup-and-run relay: PCs lend their internet connection
After=network-online.target docker.service
Wants=network-online.target

[Service]
ExecStart=${run[*]}
Restart=always
RestartSec=5
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
UNIT_END
    systemctl daemon-reload
    systemctl enable "$NAME-relay" >/dev/null
    systemctl restart "$NAME-relay"
  else
    echo "no systemd here: keep this running: ${run[*]}"
  fi
  if [[ "${SHUTUP_AND_RUN_NO_UFW:-}" != 1 ]] && command -v ufw >/dev/null 2>&1; then
    if ufw status | grep -q 'Status: active'; then
      ufw allow "${agents##*:}/tcp" || true
    fi
  fi
  public=$(curl -fsS --max-time 5 https://api.ipify.org 2>/dev/null || echo SERVER_ADDRESS)
  if [[ "${SHUTUP_AND_RUN_SHOW_TOKEN:-}" == 1 ]]; then
    token_text=$(cat "$ETC/relay_token")
  else
    token_text=TOKEN
  fi
  echo
  echo "shutup-and-run relay"
  echo "  PCs dial in:   ${public}:${agents##*:}   (open this port in the provider's firewall too)"
  echo "  proxy:         ${proxy}   (for the worker's containers; never publish it)"
  echo "  token file:    $ETC/relay_token"
  echo "  logs:          journalctl -u $NAME-relay -f"
  echo
  echo "On a client's Windows PC (PowerShell, no administrator; made-up login):"
  echo "  \$env:SHUTUP_AND_RUN_RELAY = \"${public}:${agents##*:}\""
  echo "  \$env:SHUTUP_AND_RUN_RELAY_TOKEN = \"${token_text}\""
  echo "  \$env:SHUTUP_AND_RUN_INTERNET_LOGIN = \"pc-client-01:a-password-of-16-or-more\""
  echo "  irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex"
  if [[ "$token_text" == TOKEN ]]; then
    echo "TOKEN is in $ETC/relay_token (sudo cat it, or run this again with SHUTUP_AND_RUN_SHOW_TOKEN=1)."
  fi
  echo
  echo "In a task:   HTTPS_PROXY=http://pc-client-01:PASSWORD@${proxy}"
  echo "Is it there: curl http://pc-client-01:PASSWORD@${proxy}/status"
  if [[ "$proxy" == 127.0.0.1:* ]]; then
    echo
    echo "No Docker here yet, so the proxy is on loopback: install the worker (the plain"
    echo "install line), then run this one again to move it to Docker's bridge."
  fi
  exit 0
fi

# Internet sharing: `internet on` waits up to 60 s for the relay. Not
# connected yet is not a failed install: it says why and keeps trying.
if [[ -n "$lend" ]]; then
  if [[ "$lend" == relay ]]; then
    on=(internet on --relay "$SHUTUP_AND_RUN_RELAY" --wait 60)
  else
    on=(internet on --discover "$SHUTUP_AND_RUN_POLL_URL" --wait 60)
  fi
  if [[ -n "${SHUTUP_AND_RUN_RELAY_TOKEN:-}" ]]; then
    on+=(--token "$SHUTUP_AND_RUN_RELAY_TOKEN")
  fi
  if [[ -n "${SHUTUP_AND_RUN_INTERNET_LOGIN:-}" ]]; then
    on+=(--login "$SHUTUP_AND_RUN_INTERNET_LOGIN")
  fi
  set +e
  "$PREFIX/$NAME" "${on[@]}"
  code=$?
  set -e
  if [[ "$code" == 2 ]]; then
    exit 2
  fi
  if [[ -n "$internet_only" ]]; then
    exit 0
  fi
fi

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

ip=$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for (i = 1; i <= NF; i++) if ($i == "src") { print $(i + 1); exit }}' || true)
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
  # The service does not see this variable. It only fills the first config.toml.
  if [[ -z "${SHUTUP_AND_RUN_ALLOWLIST:-}" ]]; then
    placeholder=1
  fi
  allow=
  IFS=',' read -ra images <<<"${SHUTUP_AND_RUN_ALLOWLIST:-ghcr.io/owner/selenium-worker}"
  for image in "${images[@]}"; do
    image="${image//[[:space:]]/}"
    [[ -z "$image" ]] && continue
    if [[ ! "$image" =~ ^[a-z0-9._:/@-]+$ ]]; then
      echo "SHUTUP_AND_RUN_ALLOWLIST: invalid image name: ${image}" >&2
      exit 1
    fi
    allow+="  \"${image}\","$'\n'
  done
  umask 077
  cat >"$ETC/config.toml" <<EOF
listen = "0.0.0.0:${PORT}"
max_slots = ${slots}
allowlist = [
${allow}]
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
LimitNOFILE=65536

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
    "https://127.0.0.1:${PORT}/health" >/dev/null 2>&1; then
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
echo "  allowlist:     $(tr -d '\n"' <"$ETC/config.toml" | sed -n 's/.*allowlist = \[ *\([^]]*[^], ]\)[, ]*\].*/\1/p' | tr -s ' ')"
if [[ "$health" == 1 ]]; then
  echo "  health:        ok"
else
  echo "  health:        not answering (journalctl -u ${NAME} -n 40)"
  exit 1
fi
if [[ "${placeholder:-}" == 1 ]]; then
  echo
  echo "WARNING: SHUTUP_AND_RUN_ALLOWLIST was not set. The allowlist is the placeholder"
  echo "  ghcr.io/owner/selenium-worker, which is not yours. No job of yours will start."
  echo "  Edit ${ETC}/config.toml, then: sudo systemctl restart ${NAME}"
fi
if [[ "${SHUTUP_AND_RUN_SHOW_TOKEN:-}" == 1 ]]; then
  echo "  token:         ${token}"
fi
echo
echo "Pin the fingerprint, or trust ${ETC}/cert.pem."
echo "Kestra: options.ssl.insecureTrustAllCertificates: true"
echo "n8n: allowUnauthorizedCerts: true on the HTTP Request node"

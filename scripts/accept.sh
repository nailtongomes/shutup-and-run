#!/usr/bin/env bash
# Prova o n3-launcher numa Ubuntu com Docker, sem ler o código.
#
#   sudo bash scripts/accept.sh
#   sudo bash scripts/accept.sh /caminho/do/n3-launcher
#
# Não rode numa máquina que já está servindo jobs: o teste sobe containers
# alpine de verdade e, se já existir outro launcher, os dois compartilham o
# label n3.launcher=1 no mesmo Docker.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
PORT=18787
BASE="http://127.0.0.1:${PORT}"
FAILURES=0
MODE=none
PID=
IDS=()
WORK=

pass() { echo "PASS: $1"; }
fail() { echo "FAIL: $1"; FAILURES=$((FAILURES + 1)); }

dump_logs() {
  if [[ "$MODE" == systemd ]]; then
    journalctl -u n3-launcher-accept.service -n 80 --no-pager || true
  elif [[ -n "$WORK" && -f "$WORK/launcher.log" ]]; then
    echo "---- $WORK/launcher.log ----"
    cat "$WORK/launcher.log" || true
  fi
}

cleanup() {
  if [[ "$MODE" == systemd ]]; then
    systemctl stop n3-launcher-accept.service >/dev/null 2>&1 || true
    rm -f /etc/systemd/system/n3-launcher-accept.service
    systemctl daemon-reload >/dev/null 2>&1 || true
  elif [[ -n "${PID}" ]]; then
    kill "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
  fi
  if ((${#IDS[@]} > 0)); then
    docker rm -f "${IDS[@]}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

remember() {
  local id="${1:-}"
  if [[ -n "$id" ]]; then
    IDS+=("$id")
  fi
}

if ! command -v curl >/dev/null 2>&1; then
  echo "FAIL: instale curl"
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "FAIL: 'docker info' não funcionou. Rode com sudo, ou com um usuário no grupo docker."
  exit 1
fi

if [[ $# -gt 1 ]]; then
  echo "uso: sudo bash scripts/accept.sh [caminho-do-binario]"
  exit 1
fi

if [[ -n "${1:-}" ]]; then
  BIN=$(readlink -f "$1")
elif [[ -x "$ROOT/target/release/n3-launcher" ]]; then
  BIN=$ROOT/target/release/n3-launcher
elif command -v cargo >/dev/null 2>&1; then
  echo "compilando (cargo build --release)..."
  (cd "$ROOT" && cargo build --release)
  BIN=$ROOT/target/release/n3-launcher
else
  echo "FAIL: não achei o binário. Passe o caminho, ou instale o Rust e deixe o script compilar."
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "FAIL: $BIN não é executável"
  exit 1
fi

echo "== n3-launcher acceptance =="
echo "binary: $BIN"
echo "ATENÇÃO: containers de teste usam o label n3.launcher=1 neste Docker."

if ! docker pull alpine:latest >/dev/null; then
  if docker image inspect alpine:latest >/dev/null 2>&1; then
    echo "NOTE: docker pull falhou; usando a imagem alpine que já está na máquina."
  else
    echo "FAIL: não consegui obter alpine:latest"
    exit 1
  fi
fi

if [[ $(id -u) -eq 0 ]]; then
  WORK=/var/lib/n3-launcher-accept
else
  WORK=/tmp/n3-accept-${UID}
fi
mkdir -p "$WORK" || { echo "FAIL: não criei $WORK"; exit 1; }
chmod 700 "$WORK"

if command -v openssl >/dev/null 2>&1; then
  TOKEN=$(openssl rand -hex 24)
else
  TOKEN=$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')
fi

umask 077
cat >"$WORK/config.toml" <<EOF
listen = "127.0.0.1:${PORT}"
token = "${TOKEN}"
max_slots = 2
allowlist = ["alpine"]
default_timeout_secs = 60
max_timeout_secs = 120
reaper_interval_secs = 1
EOF
BODY=$WORK/body.json

HAVE_SYSTEMD=0
if [[ $(id -u) -eq 0 ]] && [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null 2>&1; then
  HAVE_SYSTEMD=1
fi

if [[ "$HAVE_SYSTEMD" -eq 1 ]]; then
  systemctl stop n3-launcher-accept.service >/dev/null 2>&1 || true
  install -m 0755 "$BIN" /usr/local/bin/n3-launcher || { echo "FAIL: não instalei o binário em /usr/local/bin"; exit 1; }
  cat >/etc/systemd/system/n3-launcher-accept.service <<EOF
[Unit]
Description=n3-launcher acceptance
After=docker.service
Requires=docker.service

[Service]
Type=simple
ExecStart=/usr/local/bin/n3-launcher
Environment=N3_CONFIG=${WORK}/config.toml
Environment=HOME=/root
Restart=no

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  if ! systemctl start n3-launcher-accept.service; then
    echo "FAIL: systemctl não subiu n3-launcher-accept.service"
    MODE=systemd
    dump_logs
    exit 1
  fi
  MODE=systemd
  echo "mode: systemd (unit n3-launcher-accept.service, binário em /usr/local/bin/n3-launcher)"
else
  if [[ $(id -u) -eq 0 ]]; then
    install -m 0755 "$BIN" /usr/local/bin/n3-launcher
    BIN_RUN=/usr/local/bin/n3-launcher
  else
    BIN_RUN=$BIN
  fi
  N3_CONFIG="$WORK/config.toml" HOME="${HOME:-/tmp}" "$BIN_RUN" >"$WORK/launcher.log" 2>&1 &
  PID=$!
  MODE=background
  echo "mode: background pid ${PID} (sem systemd neste ambiente; numa Ubuntu com sudo o script instala o unit)"
fi

ready=0
for _ in $(seq 1 50); do
  if curl -sf -m 2 "$BASE/health" >/dev/null; then
    ready=1
    break
  fi
  sleep 0.2
done
if [[ "$ready" -ne 1 ]]; then
  echo "FAIL: o serviço não respondeu em $BASE/health"
  dump_logs
  exit 1
fi

health=$(curl -sS -m 5 "$BASE/health")
total=$(printf '%s' "$health" | sed -n 's/.*"slots_total"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
used=$(printf '%s' "$health" | sed -n 's/.*"slots_used"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1)
if [[ "$total" != 2 || "$used" != 0 ]]; then
  echo "FAIL: health inicial inesperado: $health"
  echo "Se slots_used não é 0, já existe container com label n3.launcher=1. Rode numa máquina limpa."
  exit 1
fi

post() {
  CODE=$(curl -sS -m 180 -o "$BODY" -w '%{http_code}' \
    -X POST "$BASE/v1/jobs" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H 'Content-Type: application/json' \
    --data "$1") || CODE=000
}

job_id() {
  sed -n 's/.*"id"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F][0-9a-fA-F]*\)".*/\1/p' "$BODY" | head -n 1
}

slots_free() {
  local h
  h=$(curl -sS -m 5 "$BASE/health" || true)
  printf '%s' "$h" | sed -n 's/.*"slots_free"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n 1
}

post '{"image":"alpine","command":["sleep","4"]}'
c1=$CODE
id1=$(job_id || true)
remember "$id1"
post '{"image":"alpine","command":["sleep","4"]}'
c2=$CODE
id2=$(job_id || true)
remember "$id2"
free=
for _ in 1 2 3 4 5; do
  free=$(slots_free || true)
  [[ "$free" == 0 ]] && break
  sleep 0.3
done
if [[ "$c1" == 202 && "$c2" == 202 && -n "$id1" && -n "$id2" && "$free" == 0 ]]; then
  pass "accepted until full"
else
  fail "accepted until full (codes ${c1} ${c2}, slots_free=${free:-?}, body=$(cat "$BODY" 2>/dev/null || true))"
fi

post '{"image":"alpine","command":["sleep","30"]}'
if [[ "$CODE" == 429 ]]; then
  pass "429 when full"
else
  fail "429 when full (code ${CODE}, body=$(cat "$BODY" 2>/dev/null || true))"
  remember "$(job_id || true)"
fi

sleep 10
post '{"image":"alpine","command":["sleep","1"]}'
remember "$(job_id || true)"
if [[ "$CODE" == 202 ]]; then
  pass "slot freed after a job ends"
else
  fail "slot freed after a job ends (code ${CODE}, body=$(cat "$BODY" 2>/dev/null || true))"
fi

post '{"image":"alpine","command":["sleep","300"],"timeout_secs":3}'
tid=$(job_id || true)
remember "$tid"
if [[ "$CODE" != 202 || -z "$tid" ]]; then
  fail "timeout kill (não aceitou o job: ${CODE} $(cat "$BODY" 2>/dev/null || true))"
else
  sleep 12
  state=$(docker inspect -f '{{.State.Status}}' "$tid" 2>/dev/null || echo gone)
  if [[ "$state" == "running" || "$state" == "restarting" || "$state" == "paused" ]]; then
    fail "timeout kill (container ainda ${state})"
  else
    pass "timeout kill"
  fi
fi

echo
if [[ "$FAILURES" -eq 0 ]]; then
  echo "RESULT: all passed"
  exit 0
fi
echo "RESULT: ${FAILURES} failed"
dump_logs
exit 1

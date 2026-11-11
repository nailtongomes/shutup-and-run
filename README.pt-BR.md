# fairlead

Binário estático, uma máquina. Um `POST` sobe um container se houver slot livre (`fairlead=1`) e responde na hora. Sem slot, recusa. Sem fila, sem escalonador, sem interface. O histórico fica num SQLite local, então `GET /v1/jobs/{id}` continua válido depois de reiniciar o processo.

A documentação completa está em [README.md](README.md), em inglês.

## Segurança

Quem tem o token manda a máquina rodar qualquer imagem da allowlist. Trate o token como senha de root.

**Escute só numa interface privada ou no IP Tailscale.** Não use `0.0.0.0`. O padrão é `127.0.0.1:8787`.

## Instalar

```bash
sudo install -m 0755 fairlead-x86_64-unknown-linux-musl /usr/local/bin/fairlead
sudo mkdir -p /etc/fairlead /var/lib/fairlead
sudo tee /etc/fairlead/config.toml >/dev/null <<'EOF'
listen = "192.0.2.11:8787"
max_slots = 2
allowlist = ["ghcr.io/owner/selenium-worker"]
default_timeout_secs = 3600
max_timeout_secs = 21600
history_path = "/var/lib/fairlead/jobs.sqlite"
EOF
echo "FAIRLEAD_TOKEN=$(openssl rand -hex 32)" | sudo tee /etc/fairlead/env >/dev/null
sudo chmod 600 /etc/fairlead/env /etc/fairlead/config.toml
sudo cp deploy/fairlead.service /etc/systemd/system/fairlead.service
sudo systemctl daemon-reload
sudo systemctl enable --now fairlead
```

Troque `192.0.2.11` pelo IP privado desta máquina. Para imagem privada: `sudo docker login ghcr.io` como root.

Prova numa Ubuntu com Docker:

```bash
sudo bash scripts/accept.sh
```

# Seu servidor: o relay para emprestar a internet dos clientes

Este guia é para você, operador: o servidor que recebe as conexões dos PCs dos clientes e por onde suas tarefas saem com o IP da casa de cada cliente. Os detalhes técnicos, as escolhas e os riscos estão em [INTERNET.md](INTERNET.md).

## Por que um servidor

O PC do cliente está atrás do roteador de casa e, muitas vezes, do CGNAT da operadora: ninguém consegue abrir uma conexão até ele. Então é o contrário: **o PC abre conexões até um servidor seu, com IP público**, e o relay devolve os pedidos das suas tarefas por elas. O relay é o mesmo programa (`shutup-and-run relay`), nada a mais para baixar.

```
 CASA DO CLIENTE                     SEU SERVIDOR (VPS com IP público)                INTERNET
┌──────────────────┐               ┌─────────────────────────────────────────┐
│ PC Windows       │  porta 7000   │  relay ─ porta 8080 (proxy, só interna) │
│ shutup-and-run   │──────────────►│    ▲                                    │
│ internet on      │ (o PC abre)   │    │ HTTPS_PROXY=http://ID:SENHA@...    │
└────────┬─────────┘               │  worker shutup-and-run (opcional)       │
         │                         │    └─ suas tarefas: requests, Chrome    │
         │                         └─────────────────────────────────────────┘
         └───────────── sai para o site com o IP da casa ─────────────────────►  site
```

- **7000** é a única porta aberta para a internet: os PCs chegam por ela, com o token.
- **8080** é o proxy. Nunca na internet: quem chega nela usa a conexão de um cliente. Ela fica no endereço interno do Docker, `172.17.0.1`, que as tarefas do worker no mesmo servidor alcançam e a internet não.

## O tamanho do servidor

- Uma VPS Ubuntu pequena basta: o relay usa menos de 128 MB e quase nada de processador. Com o worker junto, o que as suas tarefas precisarem.
- O que pesa é a **banda**: todo o tráfego que as tarefas fazem pelos clientes passa pelo servidor. Com as tarefas no mesmo servidor, cada byte passa uma vez (do PC para o relay); o trecho relay → tarefa é local.
- Um PC de cliente atende bem algumas tarefas ao mesmo tempo. O limite é o **upload da internet do cliente**: tudo que os sites mandam entra no PC e sai de novo para o relay.

## Montar, passo a passo

1. **Um servidor Linux com IP público** (uma VPS serve; para o jeito *a* ou *b* abaixo, com Docker). No painel do provedor, libere a porta **7000/TCP** de entrada. Se ele usa `ufw`, a linha sem Docker abre a 7000 nele; com Docker, o Docker já publica a porta.

2. **O worker, se as tarefas vão rodar neste servidor** (antes do relay, para o Docker já existir):

   ```bash
   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/voce/seu-robo bash
   ```

3. **O relay**, de um destes três jeitos (escolha um):

   **a) Docker, um comando** (o mais simples, a imagem [`ghcr.io/nailtongomes/shutup-and-run-relay`](https://github.com/nailtongomes/shutup-and-run/pkgs/container/shutup-and-run-relay), uns 20 MB, publicada a cada release):

   ```bash
   TOKEN=$(head -c 24 /dev/urandom | base64 | tr -d '/+='); echo "guarde o token: $TOKEN"
   docker run -d --name relay --restart unless-stopped \
     -p 7000:7000 -p 172.17.0.1:8080:8080 \
     -e SHUTUP_AND_RUN_RELAY_TOKEN="$TOKEN" \
     ghcr.io/nailtongomes/shutup-and-run-relay
   docker logs -f relay     # "relay ...: PCs on 0.0.0.0:7000 with a token"
   ```

   **b) Num `docker compose` que você já tem**: cole este serviço nele e ponha `RELAY_TOKEN=...` no `.env` ao lado (o arquivo completo é [examples/relay/compose.yml](../examples/relay/compose.yml)):

   ```yaml
     relay:
       image: ghcr.io/nailtongomes/shutup-and-run-relay:latest
       environment:
         SHUTUP_AND_RUN_RELAY_TOKEN: ${RELAY_TOKEN:?put RELAY_TOKEN in .env}
       ports:
         - "7000:7000"
         - "172.17.0.1:8080:8080"
       restart: unless-stopped
       read_only: true
       mem_limit: 128m
   ```

   Os outros serviços desse compose usam o proxy em `relay:8080`; as tarefas do worker, em `172.17.0.1:8080`.

   **c) Sem Docker, em uma linha** (serviço systemd `shutup-and-run-relay`):

   ```bash
   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 bash
   ```

   Ela cria o token (`/etc/shutup-and-run/relay_token`), o serviço (volta sozinho depois de um reinício ou de uma queda) e imprime o que passar aos clientes. O token aparece só com `SHUTUP_AND_RUN_SHOW_TOKEN=1`; senão, `sudo cat /etc/shutup-and-run/relay_token`.

4. **O login de cada cliente.** O seu backend escolhe um id e uma senha por PC (a senha com 16 a 128 letras, números, `-`, `_` ou `.`), por exemplo:

   ```bash
   echo "pc-cliente-01:$(head -c 18 /dev/urandom | base64 | tr -d '/+=')"
   ```

   e guarda junto com o cliente. Assim nada precisa voltar do PC.

5. **No PC do cliente** (ele, ou a IA dele, num PowerShell comum, sem administrador):

   ```powershell
   $env:SHUTUP_AND_RUN_RELAY = "IP_DO_SERVIDOR:7000"
   $env:SHUTUP_AND_RUN_RELAY_TOKEN = "O_TOKEN"
   $env:SHUTUP_AND_RUN_INTERNET_LOGIN = "pc-cliente-01:A_SENHA"
   irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
   ```

   Ou, se o seu backend responde a [descoberta](#a-url-de-poll-diz-onde-está-o-relay), só o sim do cliente e a URL:

   ```powershell
   $env:SHUTUP_AND_RUN_POLL_URL = "https://seu-backend.exemplo/poll"
   $env:SHUTUP_AND_RUN_POLL_TOKEN = "O_TOKEN_DESTE_CLIENTE"   # opcional: diz ao backend quem é
   $env:SHUTUP_AND_RUN_LEND_INTERNET = "1"
   irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
   ```

   Termina com `connected` em alguns segundos. A partir daí o PC conecta sozinho a cada logon; a pessoa pausa ou desliga em http://127.0.0.1:8788/. Num PC com WSL2 e folga, a mesma linha também instala o worker (containers no PC); num PC sem, só a internet.

6. **Nas suas tarefas**, o proxy vai no `env`:

   ```json
   "env": {"HTTPS_PROXY": "http://pc-cliente-01:A_SENHA@172.17.0.1:8080",
           "HTTP_PROXY": "http://pc-cliente-01:A_SENHA@172.17.0.1:8080"}
   ```

   Antes de uma tarefa longa, veja se o PC está lá: `curl http://pc-cliente-01:A_SENHA@172.17.0.1:8080/status` responde `"connected":true`. Código para requests, Selenium, Playwright, Puppeteer, Node e n8n: [INTERNET.md, Examples](INTERNET.md#examples-curl-requests-selenium-playwright-puppeteer-node-n8n).

   Todos os PCs de uma vez, com o token do relay: `curl -s -H "Authorization: Bearer $TOKEN" http://172.17.0.1:8080/pcs` lista id, se está conectado, conexões abertas agora e bytes desde que o relay subiu (nunca senha nem endereço).

## A URL de poll diz onde está o relay

Para não passar endereço, token e login a cada cliente, o seu backend pode responder isso. Quando a pessoa liga a internet com `SHUTUP_AND_RUN_LEND_INTERNET=1` (ou `shutup-and-run internet on --discover URL`), o PC faz **um** `POST` na sua URL de poll:

```json
{"worker": "DESK-07", "os": "windows", "arch": "x86_64", "version": "0.2.28", "lend": "internet"}
```

com `Authorization: Bearer <token do poll>` quando o cliente tem um. O `"lend": "internet"` diferencia do poll de tarefas. Responda `200`:

```json
{"relay": "IP_DO_SERVIDOR:7000", "token": "O_TOKEN_DO_RELAY", "login": "pc-cliente-01:A_SENHA"}
```

- `relay` é obrigatório; `token`, se o seu relay pede; `login`, o que o backend escolheu para esse PC (guarde-o: é o que as tarefas usam).
- Qualquer outra resposta, e o PC não liga nada e diz o motivo.
- É perguntado uma vez, quando a pessoa liga. Para mudar o relay, a linha de instalação roda de novo.
- O contrato completo: [INTEGRATION-CONTRACT.md, Internet discovery](INTEGRATION-CONTRACT.md#internet-discovery).

**Sem escrever backend: n8n neste servidor.** O fluxo [examples/n8n/worker-orchestrator.json](../examples/n8n/worker-orchestrator.json) já é uma URL de poll (aprovação de PCs, fila, callbacks) e responde também a descoberta: gera e guarda um login por PC e devolve relay, token e login. Para mandar uma tarefa pela internet de um PC, enfileire com `"via_internet": "ID_DO_PC"`: o fluxo põe o `HTTPS_PROXY` daquele PC na tarefa e a entrega ao worker deste servidor. Passo a passo em [examples/n8n/README.md, "Internet sharing"](../examples/n8n/README.md#9-internet-sharing).

## O teste de dois minutos

Com um PC conectado (passo 5), no servidor:

```bash
curl -s https://api.ipify.org; echo        # o IP do servidor
docker run --rm curlimages/curl -s -x http://pc-cliente-01:A_SENHA@172.17.0.1:8080 https://api.ipify.org; echo
                                           # o IP da casa do cliente
journalctl -u shutup-and-run-relay -n 20   # "PC pc-cliente-01 connected", uma linha por pedido
```

O segundo `curl` roda num container na rede padrão do Docker, como uma tarefa do worker: se ele mostra o IP do cliente, as tarefas também vão.

## Atualizar e remover

- **Atualizar:** com Docker, `docker pull ghcr.io/nailtongomes/shutup-and-run-relay` e recrie o container (`docker compose pull && docker compose up -d`, no compose). Sem Docker, rode de novo a linha do relay (ou a do worker: ela reinicia o relay no binário novo). Atualize o relay e os PCs juntos: um relay 0.2.27 recusa um PC 0.2.26 com `update it`, e o PC se atualiza rodando a linha de instalação dele de novo.
- **Remover:** `docker rm -f relay`; sem Docker, `curl -fsSL .../install.sh | sudo bash -s -- --uninstall` tira o worker e o relay (com `--purge`, também a configuração e o token).

## Tarefas em outro servidor

O proxy nunca vai para a internet. Ponha o relay e os outros servidores numa VPN (Tailscale, WireGuard) e o proxy no endereço da VPN: no Docker, `-p 100.64.0.5:8080:8080` no lugar de `-p 172.17.0.1:8080:8080`; sem Docker,

```bash
curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 SHUTUP_AND_RUN_RELAY_PROXY=100.64.0.5:8080 bash
```

As tarefas de lá usam `http://ID:SENHA@100.64.0.5:8080`.

## Quando algo não funciona

| O que acontece | Por quê, e o que fazer |
| --- | --- |
| O PC diz `INTERNET_RELAY_UNREACHABLE` | A 7000 não está liberada no firewall do provedor, ou o relay está parado: `systemctl status shutup-and-run-relay`. |
| O PC diz `INTERNET_REFUSED` com `wrong or missing relay token` | O token do PC não é o do relay (`SHUTUP_AND_RUN_RELAY_TOKEN` do container, ou `/etc/shutup-and-run/relay_token`). |
| `internet on --discover` diz `answered 204 with no relay for this PC` | O seu backend não respondeu `200` com `relay` ao corpo com `"lend": "internet"`. |
| O PC diz `INTERNET_REFUSED` com `update it` | O PC está numa versão antiga: rode a linha de instalação dele de novo. |
| `/status` responde `401` | O id ou a senha da tarefa não são os do PC. |
| A tarefa recebe `503` | O PC não está conectado agora (desligado, dormindo, pausado, fora do horário, bateria baixa, rede limitada). Tente de novo mais tarde. |
| A tarefa não alcança `172.17.0.1:8080` | O relay foi instalado antes do Docker e ficou em `127.0.0.1`: rode a linha do relay de novo. |

## Segurança, em uma linha

Só a 7000 aberta, o token em todo PC, a 8080 nunca na internet. A ligação PC → relay ainda não é criptografada (HTTPS continua criptografado de ponta a ponta, mas a senha do proxy e os nomes dos sites passam legíveis): os riscos e os próximos passos estão em [INTERNET.md, Risks](INTERNET.md#risks-and-what-is-left-out).

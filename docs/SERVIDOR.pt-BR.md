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

1. **Uma VPS Ubuntu com IP público.** No painel do provedor, libere a porta **7000/TCP** de entrada. Se a VPS usa `ufw`, a instalação abre a 7000 nele.

2. **O worker, se as tarefas vão rodar neste servidor** (antes do relay, para o Docker já existir):

   ```bash
   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/voce/seu-robo bash
   ```

3. **O relay, em uma linha:**

   ```bash
   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 bash
   ```

   Ela cria o token (`/etc/shutup-and-run/relay_token`), a unidade systemd `shutup-and-run-relay` (volta sozinha depois de um reinício ou de uma queda) e imprime o que passar aos clientes: o endereço `IP:7000`, a linha de instalação do Windows e o `HTTPS_PROXY` das tarefas. O token aparece só com `SHUTUP_AND_RUN_SHOW_TOKEN=1`; senão, `sudo cat /etc/shutup-and-run/relay_token`.

   Prefere Docker? `examples/relay` com `docker compose up -d --build` faz o mesmo, com a 8080 também em `172.17.0.1`.

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

   Termina com `connected` em alguns segundos. A partir daí o PC conecta sozinho a cada logon; a pessoa pausa ou desliga em http://127.0.0.1:8788/.

6. **Nas suas tarefas**, o proxy vai no `env`:

   ```json
   "env": {"HTTPS_PROXY": "http://pc-cliente-01:A_SENHA@172.17.0.1:8080",
           "HTTP_PROXY": "http://pc-cliente-01:A_SENHA@172.17.0.1:8080"}
   ```

   Antes de uma tarefa longa, veja se o PC está lá: `curl http://pc-cliente-01:A_SENHA@172.17.0.1:8080/status` responde `"connected":true`. Código para requests, Selenium, Playwright, Puppeteer, Node e n8n: [INTERNET.md, Examples](INTERNET.md#examples-curl-requests-selenium-playwright-puppeteer-node-n8n).

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

- **Atualizar:** rode de novo a linha do relay (ou a do worker: ela reinicia o relay no binário novo). Atualize o relay e os PCs juntos: um relay 0.2.27 recusa um PC 0.2.26 com `update it`, e o PC se atualiza rodando a linha de instalação dele de novo.
- **Remover:** `curl -fsSL .../install.sh | sudo bash -s -- --uninstall` tira o worker e o relay; com `--purge`, também a configuração e o token.

## Tarefas em outro servidor

O proxy nunca vai para a internet. Ponha o relay e os outros servidores numa VPN (Tailscale, WireGuard) e o proxy no endereço da VPN:

```bash
curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 SHUTUP_AND_RUN_RELAY_PROXY=100.64.0.5:8080 bash
```

As tarefas de lá usam `http://ID:SENHA@100.64.0.5:8080`.

## Quando algo não funciona

| O que acontece | Por quê, e o que fazer |
| --- | --- |
| O PC diz `INTERNET_RELAY_UNREACHABLE` | A 7000 não está liberada no firewall do provedor, ou o relay está parado: `systemctl status shutup-and-run-relay`. |
| O PC diz `INTERNET_REFUSED` com `wrong or missing relay token` | O token do PC não é o de `/etc/shutup-and-run/relay_token`. |
| O PC diz `INTERNET_REFUSED` com `update it` | O PC está numa versão antiga: rode a linha de instalação dele de novo. |
| `/status` responde `401` | O id ou a senha da tarefa não são os do PC. |
| A tarefa recebe `503` | O PC não está conectado agora (desligado, dormindo, pausado, fora do horário, bateria baixa, rede limitada). Tente de novo mais tarde. |
| A tarefa não alcança `172.17.0.1:8080` | O relay foi instalado antes do Docker e ficou em `127.0.0.1`: rode a linha do relay de novo. |

## Segurança, em uma linha

Só a 7000 aberta, o token em todo PC, a 8080 nunca na internet. A ligação PC → relay ainda não é criptografada (HTTPS continua criptografado de ponta a ponta, mas a senha do proxy e os nomes dos sites passam legíveis): os riscos e os próximos passos estão em [INTERNET.md, Risks](INTERNET.md#risks-and-what-is-left-out).

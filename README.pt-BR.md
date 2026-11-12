# shutup-and-run

[English](README.md)

Runner de jobs em container, auto-hospedado, para CPU e RAM sobrando num PC de casa ou do escritório. No Linux, o Kestra ou o n8n envia um container Docker para uma API HTTP. No Windows 11 o serviço roda dentro do WSL2 e puxa as tarefas. Os slots limitam quantas rodam ao mesmo tempo. Um timeout mata a tarefa que passa do prazo. Máquina cheia responde `429`.

Para quem empresta máquinas ociosas a workers de RPA ou Selenium, ou roda alguns workers de borda e está comparando Nomad, GitLab Runner e Tork.

Não é fila, não é escalonador, não é CI e não é sandbox. Não tem interface web. O binário inicia uma imagem da allowlist e não sabe o que a imagem faz.

## Projetos parecidos

Ferramentas próximas. Esta é um processo só, numa máquina que você já tem. O orquestrador escolhe a máquina.

- **Nomad** (HashiCorp) escalona jobs num cluster. Use quando um plano de controle deve decidir onde o trabalho roda.
- **HTCondor** faz o matchmaking de jobs de alto volume num pool. Use para um pool de batch ou de pesquisa.
- **GitLab Runner** roda jobs de CI do GitLab, com executor shell, Docker ou Kubernetes. Use quando o trabalho é um pipeline do GitLab.
- **GitHub Actions self-hosted runner** roda workflows do GitHub Actions no seu hardware. Use quando o trabalho é um workflow do GitHub.
- **Tork** é um motor de workflow: um coordenador enfileira tarefas em container e os workers executam, numa máquina ou em várias, por um broker. Use quando você quer essa fila, as retentativas e jobs feitos de várias tarefas.
- **BOINC** é computação voluntária. Um servidor de projeto entrega trabalho a um cliente que as pessoas instalam para doar CPU. Use quando estranhos rodam uma carga publicada. Este projeto roda as suas imagens em máquinas que você configura.

Envie um job para uma máquina. Ela roda o seu container ou avisa que está cheia. O seu orquestrador (Kestra, n8n, um script de shell) guarda a lista de máquinas e faz um POST do job para a primeira:

| Resposta | Significado | O que o chamador faz |
| --- | --- | --- |
| `202` | Aceito. Um slot foi ocupado e o container está sendo iniciado. | Nada. Está feito. |
| `429` | Todos os slots estão ocupados. | Tenta a próxima máquina. |
| `503` | A máquina está em drenagem para manutenção. | Tenta a próxima máquina. |

O launcher não sabe o que a sua imagem faz. Ele inicia uma imagem que está na allowlist, aplica o timeout e o limite de slots, guarda o histórico e chama você de volta quando o job termina.

```text
 Kestra / n8n                 máquina A                    máquina B
 ------------                 ---------                    ---------
 POST /v1/jobs  ------------> 429 (cheia)
 POST /v1/jobs  -----------------------------------------> 202 (aceito)
                                                           baixa a imagem, roda o container
 webhook        <----------------------------------------- callback quando termina
```

Uma máquina atrás de NAT pode pedir trabalho em vez de ser chamada. Veja [Pull](#pull). Uma máquina Windows 11 roda a mesma coisa em modo pull dentro do WSL2: [docs/WINDOWS.md](docs/WINDOWS.md).

## Início rápido

Tudo aqui é para copiar e colar. Troque `ghcr.io/you/worker` por uma imagem sua e `203.0.113.10` pelo endereço da máquina.

### 1. Instalar, na máquina

Ubuntu, com endereço público. Precisa de uma release publicada (uma tag `v*`).

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
```

O instalador instala o Docker se faltar, sobe o serviço na porta 8787 em HTTPS e mostra a URL e a impressão digital (fingerprint) do certificado. O token não é mostrado. Para ler:

```bash
sudo sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' /etc/shutup-and-run/env
```

### 2. Primeiro job, de qualquer lugar

```bash
export SAR=https://203.0.113.10:8787
```

```bash
export SAR_TOKEN=cole-o-token-aqui
```

```bash
curl -sk -X POST "$SAR/v1/jobs" -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"image":"ghcr.io/you/worker","timeout_secs":600,"env":{"JOB_NAME":"hello"}}'
```

A resposta traz um `id`. Status e as últimas linhas de log:

```bash
curl -sk -H "Authorization: Bearer $SAR_TOKEN" "$SAR/v1/jobs/COLE_O_ID?logs=true"
```

`-k` pula a verificação do certificado. Para fixar o certificado do instalador, veja [docs/RECIPES.md](docs/RECIPES.md) (em inglês).

### 3. Kestra

Crie o secret `SHUTUP_AND_RUN_TOKEN`. Uma task por máquina. A segunda só roda quando a primeira respondeu cheia ou em drenagem. O fluxo completo, com retentativas, é `examples/kestra-dispatch.yml`.

```yaml
id: push_job
namespace: example.jobs

tasks:
  - id: host_1
    type: io.kestra.plugin.core.http.Request
    uri: https://203.0.113.10:8787/v1/jobs
    method: POST
    contentType: application/json
    allowFailure: true
    options:
      allowFailed: true
      ssl:
        insecureTrustAllCertificates: true
    headers:
      Authorization: "Bearer {{ secret('SHUTUP_AND_RUN_TOKEN') }}"
    body: |
      {{ { "image": "ghcr.io/you/worker", "job_key": execution.id, "timeout_secs": 3600 } }}

  - id: host_2
    type: io.kestra.plugin.core.http.Request
    runIf: "{{ outputs.host_1.code == 429 or outputs.host_1.code == 503 }}"
    uri: https://203.0.113.11:8787/v1/jobs
    method: POST
    contentType: application/json
    options:
      ssl:
        insecureTrustAllCertificates: true
    headers:
      Authorization: "Bearer {{ secret('SHUTUP_AND_RUN_TOKEN') }}"
    body: |
      {{ { "image": "ghcr.io/you/worker", "job_key": execution.id, "timeout_secs": 3600 } }}
```

### 4. n8n

Copie e cole no canvas. É um nó HTTP Request. Defina `SHUTUP_AND_RUN_TOKEN` no processo do n8n (com `N8N_BLOCK_ENV_ACCESS_IN_NODE=false`), ou troque o header por uma credencial Header Auth. O workflow com duas máquinas é `examples/n8n-dispatch.json`.

```json
{
  "nodes": [
    {
      "name": "Push job",
      "type": "n8n-nodes-base.httpRequest",
      "typeVersion": 4.2,
      "position": [260, 300],
      "retryOnFail": true,
      "maxTries": 3,
      "waitBetweenTries": 2000,
      "parameters": {
        "method": "POST",
        "url": "https://203.0.113.10:8787/v1/jobs",
        "sendHeaders": true,
        "headerParameters": {
          "parameters": [
            { "name": "Authorization", "value": "={{ 'Bearer ' + $env.SHUTUP_AND_RUN_TOKEN }}" },
            { "name": "Content-Type", "value": "application/json" }
          ]
        },
        "sendBody": true,
        "specifyBody": "json",
        "jsonBody": "={{ JSON.stringify({ image: 'ghcr.io/you/worker', job_key: $execution.id, timeout_secs: 3600 }) }}",
        "options": {
          "allowUnauthorizedCerts": true,
          "timeout": 20000,
          "response": { "response": { "fullResponse": true, "neverError": true } }
        }
      }
    }
  ],
  "connections": {}
}
```

A saída do nó traz `statusCode`. Encaminhe `429` e `503` para uma cópia do nó que aponta para a próxima máquina.

### 5. Fiquei bloqueado

Três tokens errados banem o endereço do chamador por 48 horas. Na máquina, por SSH:

```bash
sudo shutup-and-run bans
```

```bash
sudo shutup-and-run bans lift 203.0.113.5
```

### 6. Conferir a máquina

Doze verificações contra o serviço em execução. Veja [Testes](#testes) para a imagem usada.

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/smoke.sh | sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker bash
```

### 7. Parar tudo

O serviço e, depois, todos os jobs que ele iniciou:

```bash
sudo systemctl stop shutup-and-run
```

```bash
sudo docker ps -q --filter label=shutup-and-run=1 | xargs -r sudo docker kill
```

Outros casos estão em [docs/RECIPES.md](docs/RECIPES.md) (em inglês): a regra de firewall, máquina atrás de NAT, máquina emprestada por um cliente, drenagem, troca do token.

## Segurança em um minuto

- O token é uma senha de root da máquina. Quem tem o token roda qualquer imagem da allowlist com qualquer variável de ambiente. Todas as rotas exigem o token, inclusive `/health` e `/metrics`.
- Três tokens errados do mesmo endereço banem esse endereço por 48 horas. Um endereço banido recebe `403` com corpo vazio, mesmo com o token certo.
- O serviço fala HTTPS com um certificado autoassinado gerado pelo instalador. Fixe a impressão digital que ele mostra.
- Um job não consegue montar o host, publicar porta nem rodar como privilegiado. Mesmo assim não é um sandbox: o container tem os privilégios e a rede padrão do Docker.
- A porta fica aberta para a internet se você não usar firewall. Libere só o seu orquestrador. O comando está em [docs/RECIPES.md](docs/RECIPES.md).

Como o processo se comporta está em [SECURITY.md](SECURITY.md). O que você aceita ao rodá-lo está em [docs/CONVENTIONS.md](docs/CONVENTIONS.md). Os dois estão em inglês.

## API

Todas as rotas exigem `Authorization: Bearer <token>`. A exceção é a página de status local, quando `status_page` está ligado.

Um script na máquina lê `status.json` e não chama HTTP. No Linux o arquivo fica ao lado do histórico, por padrão `/var/lib/shutup-and-run/status.json`, e o diretório é só do root. No Windows o supervisor grava `C:\ProgramData\shutup-and-run\status.json`, que o usuário logado lê sem ser administrador. São os mesmos fatos da página de status. `state` é `active` (uma tarefa ocupa um slot), `idle`, `draining`, `failed` ou `stopped`. Os outros campos são `slots_total`, `slots_used`, `job_ids`, `last_job_at`, `last_error`, `version`, `pid` e `updated_at` (UTC, `YYYY-MM-DDThh:mm:ssZ`). O arquivo é trocado pelo menos a cada 30 segundos e em até um segundo depois de uma mudança, gravando um arquivo temporário e renomeando por cima do antigo. Se `updated_at` tem mais de 60 segundos, o worker está morto. `stopped` com `updated_at` recente é uma parada limpa. No Windows, quando a distribuição ou o serviço morreu, `state` é `failed` e `last_error` diz o motivo.

```sh
sudo sed -n 's/.*"state":"\([^"]*\)".*/\1/p' /var/lib/shutup-and-run/status.json
```

```powershell
(Get-Content C:\ProgramData\shutup-and-run\status.json -Raw | ConvertFrom-Json).state
```

### `POST /v1/jobs`

```json
{
  "image": "ghcr.io/you/worker:latest",
  "job_key": "order-1001",
  "env": {"JOB_NAME": "example"},
  "command": ["python", "main.py"],
  "timeout_secs": 3600,
  "cpu": 1,
  "memory": "2g",
  "shm": "2g",
  "callback_url": "https://n8n.example/webhook/done"
}
```

| Campo | |
| --- | --- |
| `image` | Obrigatório. Precisa estar na allowlist. Em minúsculas. |
| `job_key` | Opcional. Torna a retentativa segura: a mesma chave devolve o mesmo job. De 1 a 200 caracteres: letras, dígitos, `.`, `_`, `:`, `-`. |
| `env` | Opcional. Objeto de strings. Coloque os segredos aqui, não na imagem. |
| `command` | Opcional. O `Cmd` do Docker. Sem ele, a imagem roda como foi construída. |
| `timeout_secs` | Opcional. O container é morto depois desse tempo. Padrão 3600, no máximo `max_timeout_secs`. |
| `cpu` | Opcional. Quantidade de CPUs, como `0.5` ou `2`. |
| `memory`, `shm` | Opcional. Bytes, ou `512m`, `1g`. Sem `memory` o container não tem limite. |
| `callback_url` | Opcional. Recebe o JSON final do job quando ele termina. |

Campos desconhecidos são recusados.

| Código | Corpo | Significado |
| --- | --- | --- |
| `202` | o job | Aceito. O slot foi ocupado. A imagem é baixada e o container iniciado depois desta resposta. |
| `200` | o job | Essa `job_key` já tinha sido aceita. Nada novo é iniciado. |
| `429` | `{"error":"no free slots","slots_total":2,"slots_used":2,"slots_free":0}` | Cheia. |
| `503` | `{"error":"draining","draining":true}` | Em drenagem. |
| `403` | `{"error":"image is not allowlisted"}` | |
| `400` | `{"error":"..."}` | Corpo, timeout, chave ou URL de callback inválidos. |
| `401` | `{"error":"unauthorized"}` | Conta para o banimento. |
| `502` | `{"error":"docker is unavailable"}` | O Docker ou o arquivo de histórico não respondeu. |

O job:

```json
{"id":"5f479abd3087cf193ed4c1da8460784d","status":"running","image":"ghcr.io/you/worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":null,"exit_code":null,"reason":null,"duration_secs":null}
```

O `202` chega em milissegundos. Ele não espera o registry. Se o download da imagem ou a partida falhar depois, o job termina com `reason` `start_failed`, os `logs` dizem o motivo e o callback é enviado. A `job_key` de um job que falhou continua ocupada; use uma chave nova para tentar de novo. O `timeout_secs` começa a contar quando o container inicia.

### `GET /v1/jobs/{id}`

Acrescente `?logs=true` para ver as últimas 200 linhas de log.

```json
{"id":"5f479abd3087cf193ed4c1da8460784d","status":"exited","image":"ghcr.io/you/worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":1700000012,"exit_code":0,"reason":"exited","duration_secs":12,"logs":"..."}
```

`status` é `running` ou `exited`. `reason` é `exited`, `timeout_kill` ou `start_failed`. O histórico fica em SQLite, então isso funciona depois de um restart, por 30 dias por padrão. Um id desconhecido é `404` `{"id":"...","status":"not_found"}`.

Não existe chamada de cancelamento. Para encerrar um job antes da hora, mate o container na máquina: o nome dele é `shutup-and-run-<id>`.

### Callback

Quando o job termina, o mesmo JSON (sem os logs) é enviado por POST para `callback_url`. Três tentativas, sem seguir redirecionamentos. Aponte para um webhook do Kestra ou do n8n.

### `GET /health`

```json
{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}
```

Um slot usado é um container em execução com o label `shutup-and-run=1`, ou um job aceito que ainda está baixando a imagem. `503` quando o Docker não responde.

### `GET /metrics`

Texto no formato Prometheus: `shutup_and_run_slots_total`, `_slots_used`, `_slots_free`, `_draining`, `_jobs_started_total`, `_jobs_finished_total`, `_jobs_timeout_total`.

### `POST /v1/drain`

`{"draining": true}` recusa jobs novos com `503` e deixa os que estão rodando terminarem. `{"draining": false}` volta ao normal. O estado sobrevive a um restart.

## Pull

Para uma máquina que não pode ser alcançada de fora. Defina `poll_url` e a máquina pede uma tarefa a essa URL a cada `poll_interval_secs`, só quando há slot livre e ela não está em drenagem. Sem `poll_url`, o processo não faz nenhuma chamada de saída.

```http
POST {poll_url}
Authorization: Bearer {SHUTUP_AND_RUN_POLL_TOKEN}
Content-Type: application/json

{"worker": "desk-07", "os": "linux", "arch": "x86_64", "version": "0.2.0", "max_slots": 2, "free_slots": 1, "cpus": 8, "memory_bytes": 16624934912}
```

O corpo diz quem está pedindo, para que uma URL atenda todas as máquinas: `worker` é o `worker_name` (padrão: o hostname), `os` é o `worker_os` (padrão: o sistema para o qual o binário foi compilado; o worker Windows diz `windows`) e `free_slots` é quantas tarefas a máquina aceitaria agora. `arch`, `version`, `max_slots`, `cpus` e `memory_bytes` dizem o que a máquina é e o que pode emprestar, para o servidor decidir se a fila pode atendê-la. O endereço da máquina não vai no corpo: leia da conexão. Um `401` ou `403` significa "não liberada": a máquina não inicia nada e pergunta de novo, espaçando até cinco minutos.

- Nada a fazer: `204`, ou `200` com corpo vazio, `null`, `{}`, `{"task": null}` ou `{"manifest_url": null}`.
- Uma tarefa: `200` com o mesmo JSON do `POST /v1/jobs`, ou `{"task": { ... }}`.
- Um manifesto: `200` com `{"manifest_url": "https://..."}`. A máquina busca essa URL uma vez com `GET`, e o corpo precisa ser uma tarefa em um dos dois formatos acima. Um manifesto não pode apontar para outro manifesto. O token de poll só vai junto no `GET` quando o manifesto está no mesmo esquema, host e porta da `poll_url`; um manifesto em storage de objetos ou CDN é buscado sem ele. Mesmo limite de 64 KiB, sem redirecionamento.

A tarefa passa pelas mesmas verificações de um POST. Oferecer uma tarefa não é uma confirmação: a máquina pode recusar (o slot acabou de ser ocupado, a imagem não está na allowlist). Continue oferecendo a mesma `job_key` até o callback chegar. Depois de aceita, essa chave nunca inicia um segundo container.

`poll_any_image = true` deixa uma tarefa vinda do poll usar qualquer imagem, na allowlist ou não. O nome da imagem continua validado, e um job empurrado por POST continua preso à allowlist, que então pode ficar vazia. Exige `poll_url`. É para uma máquina cuja única fonte de trabalho é o seu orquestrador, como o worker Windows; em um servidor que também recebe POST, deixe desligado.

Erros (falha de conexão, redirecionamento, qualquer outro status, corpo acima de 64 KiB) vão para o log e a espera dobra, até 5 minutos. O processo não encerra. O token de poll é lido só do ambiente. Exemplos: `examples/kestra-poll.yml`, `examples/n8n-poll.json`. Os comandos de configuração estão em [docs/RECIPES.md](docs/RECIPES.md).

O contrato completo para um orquestrador (push, poll, manifesto e callback), com JSON Schemas e o que é e não é garantido, está em [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md), em inglês. Uma URL de poll com aprovação de máquinas, manifesto e receptor de callback feita no n8n, e um teste de fumaça para Linux e Windows, estão em [examples/n8n](examples/n8n/README.md).

## Configuração

`/etc/shutup-and-run/config.toml`, ou o arquivo indicado em `SHUTUP_AND_RUN_CONFIG`. As variáveis de ambiente têm prioridade sobre o arquivo. Uma chave desconhecida impede a partida. Depois de alterar: `sudo systemctl restart shutup-and-run`. Os jobs em execução sobrevivem ao restart.

| toml | ambiente | significado |
| --- | --- | --- |
| `listen` | `SHUTUP_AND_RUN_LISTEN` | `ip:porta`. Padrão `127.0.0.1:8787`. O instalador grava `0.0.0.0:8787`. |
| `token` | `SHUTUP_AND_RUN_TOKEN` | Obrigatório. Pelo menos 16 caracteres. O instalador guarda em `/etc/shutup-and-run/env`. |
| `max_slots` | `SHUTUP_AND_RUN_MAX_SLOTS` | Obrigatório. De 1 a 256. O instalador sugere um por CPU e por 2 GiB. |
| `allowlist` | `SHUTUP_AND_RUN_ALLOWLIST` | Obrigatório, a menos que `poll_any_image` esteja ligado. Imagens que podem rodar. Separadas por vírgula no ambiente. |
| `default_timeout_secs` | `SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS` | Padrão 3600. |
| `max_timeout_secs` | `SHUTUP_AND_RUN_MAX_TIMEOUT_SECS` | Padrão 21600. No máximo 7 dias. |
| `history_path` | `SHUTUP_AND_RUN_HISTORY_PATH` | Arquivo SQLite. Padrão `/var/lib/shutup-and-run/jobs.sqlite`. |
| `history_days` | `SHUTUP_AND_RUN_HISTORY_DAYS` | Jobs terminados mais antigos que isso são apagados. Padrão 30. |
| `log_lines` | `SHUTUP_AND_RUN_LOG_LINES` | Linhas de log guardadas por job. Padrão 200. |
| `prune_interval_secs` | `SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS` | Intervalo para remover containers terminados e imagens sem uso. Padrão 0, desligado. |
| `auth_failures` | `SHUTUP_AND_RUN_AUTH_FAILURES` | Tokens errados antes do banimento. Padrão 3. |
| `ban_hours` | `SHUTUP_AND_RUN_BAN_HOURS` | Padrão 48. |
| `tls_cert`, `tls_key` | `SHUTUP_AND_RUN_TLS_CERT`, `SHUTUP_AND_RUN_TLS_KEY` | Arquivos PEM. Obrigatórios, a não ser com `http = true`. |
| `http` | `SHUTUP_AND_RUN_HTTP` | `true` serve sem TLS. Só para localhost. |
| `trust_x_forwarded_for` | `SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR` | Só atrás de um proxy seu. Padrão false. |
| `status_page` | `SHUTUP_AND_RUN_STATUS_PAGE` | Página de status em `/`, sem token, só para um navegador na própria máquina. Padrão false; a instalação no Windows liga. |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Opcional. Veja [Pull](#pull). |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Padrão 15. |
| `poll_any_image` | `SHUTUP_AND_RUN_POLL_ANY_IMAGE` | Uma tarefa do poll pode usar qualquer imagem. Padrão false. Exige `poll_url`. Permite `allowlist` vazia. |
| `worker_name` | `SHUTUP_AND_RUN_WORKER_NAME` | Enviado no corpo do poll. Padrão: o hostname. |
| `worker_os` | `SHUTUP_AND_RUN_WORKER_OS` | Enviado no corpo do poll. Padrão: o sistema da compilação. |
| (nenhum) | `SHUTUP_AND_RUN_POLL_TOKEN` | Só no ambiente. |
| `reaper_interval_secs` | `SHUTUP_AND_RUN_REAPER_INTERVAL_SECS` | Intervalo para matar containers que passaram do prazo. Padrão 2. |
| `purge_interval_secs` | `SHUTUP_AND_RUN_PURGE_INTERVAL_SECS` | Intervalo para apagar histórico antigo. Padrão 3600. |
| `docker_config` | `SHUTUP_AND_RUN_DOCKER_CONFIG` | Caminho do `config.json` do Docker, para imagens privadas. |

**Allowlist.** `ghcr.io/you/worker` permite qualquer tag ou digest desse repositório e mais nada: nem `ghcr.io/you/worker-evil`, nem `ghcr.io/you/other`. Uma entrada com tag, como `ghcr.io/you/worker:1.4`, permite exatamente esse texto.

**Imagens privadas.** `sudo docker login ghcr.io -u VOCE` na máquina. O chamador não consegue enviar credenciais de registry.

**Logs.** `journalctl -u shutup-and-run -f`. O token e as variáveis de ambiente dos jobs nunca vão para o log.

## Instalador

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
```

| Variável | |
| --- | --- |
| `SHUTUP_AND_RUN_ALLOWLIST` | Imagens do primeiro `config.toml`. Sem ela, o instalador grava um valor de exemplo e avisa. |
| `SHUTUP_AND_RUN_MAX_SLOTS` | Substitui a quantidade de slots sugerida. |
| `SHUTUP_AND_RUN_PORT` | Padrão 8787. |
| `SHUTUP_AND_RUN_NO_UFW=1` | Não abre a porta no ufw. |
| `SHUTUP_AND_RUN_SHOW_TOKEN=1` | Mostra o token. |
| `SHUTUP_AND_RUN_BIN=./shutup-and-run` | Instala um binário que você já tem. Sem download e sem checksum. |

O download é conferido com o `SHA256SUMS` da mesma release, e uma diferença interrompe a instalação. Rodar de novo atualiza o binário e mantém o token, o certificado e a configuração. `--uninstall` remove o serviço e o binário. `--uninstall --purge` também remove `/etc/shutup-and-run` e `/var/lib/shutup-and-run`.

Também é publicada uma imagem em `ghcr.io/nailtongomes/shutup-and-run`, para quem roda tudo em Docker. Ela precisa do socket do Docker do host montado, o que equivale a root no host. A instalação por systemd é o caminho suportado.

### Windows 11

A release também traz `shutup-and-run-x86_64-pc-windows-msvc.exe`. Ele é um supervisor: importa uma distribuição WSL2 só dele, instala o Docker Engine e o binário Linux lá dentro e mantém o serviço rodando ali em modo pull. Sem Docker Desktop, sem porta aberta. Com o WSL2 habilitado (`wsl --install --no-distribution` como administrador, depois reiniciar):

```powershell
$env:SHUTUP_AND_RUN_POLL_TOKEN = "<token>"   # opcional: sem ele a máquina cria o próprio e espera a aprovação
$env:SHUTUP_AND_RUN_POLL_URL = "https://orchestrator.example/poll"
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

`shutup-and-run wsl doctor` confere o WSL, a distribuição, o Docker, roda um container e confere o serviço. Sem a URL e sem o token, `shutup-and-run wsl install` deixa a máquina instalada e ociosa, só com o smoke test. `wsl logs`, `wsl stop`, `wsl start`, `wsl uninstall`. Tudo, inclusive o que uma máquina Windows emprestada abre mão, está em [docs/WINDOWS.md](docs/WINDOWS.md). O teste manual passo a passo está em [docs/TESTING.pt-BR.md](docs/TESTING.pt-BR.md).

Um PC que não consegue rodar WSL2 não consegue rodar o `.exe`. [`scripts/windows/sar-launcher.bat`](scripts/windows/sar-launcher.bat) é o recurso para esse caso: o `cmd` baixa um script Python de uma URL que você define, guarda em cache e executa. Não usa a allowlist, os slots nem a API HTTP, e não confere checksum. Use HTTPS e um host que você controla. Detalhes: [docs/WINDOWS.md](docs/WINDOWS.md#when-the-exe-cannot-run).

## Testes

O `scripts/smoke.sh` confere uma instância em execução pela API. Precisa de `curl` e `python3` onde ele roda, e de uma imagem na allowlist da instância:

- **O seu próprio worker**, se ele tiver `sh` e `sleep`. É a melhor verificação, porque é o que você vai rodar.
- **`ghcr.io/nailtongomes/shutup-and-run-smoke`**, o padrão. Uma imagem Python pequena, construída a partir de `examples/smoke-image`. Sem comando, ela imprime `python ok` e termina.
- **`hello-world`**. Não tem shell, então as cinco verificações que precisam de um job em execução são puladas.

Na máquina, como root. `SMOKE_LOCAL=1` lê a URL, o token e a impressão digital em `/etc/shutup-and-run`:

```bash
sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker SMOKE_RESTART_CMD="systemctl restart shutup-and-run" bash scripts/smoke.sh
```

De outra máquina:

```bash
SMOKE_URL=https://203.0.113.10:8787 SMOKE_TOKEN=... SMOKE_FINGERPRINT=AA:BB:... SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh
```

Um único job com todos os campos preenchidos, e mais nada:

```bash
SMOKE_URL=... SMOKE_TOKEN=... SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh preflight
```

A saída do script é em inglês:

```text
PASS: 1 health returns slot counts; 401 without a token
PASS: 2 job accepted, running, exited 0, logs returned
PASS: 3 3 slots filled, then 429, then 202 after one job ended
PASS: 4 job past timeout_secs was killed, reason timeout_kill
PASS: 5 image off the allowlist is 403 and nothing started
PASS: 6 same job_key is 202 then 200 with the same id
PASS: 7 drain is 503 for new jobs; undrain is 202 again
PASS: 8 restart kept history, kept the running job, and freed its slot when it ended
PASS: 9 three wrong tokens from 127.0.0.2 is 403 even with the right token; bans lift restores it
PASS: 10 poll took one task, started exactly one container, then got empty answers
PASS: 11 callback_url received the final job JSON once
PASS: 12 preflight: one job end to end (env, cpu, memory, shm, job_key, callback), slot freed

RESULT: 12 passed, 0 failed, 0 skipped
```

É seguro em uma máquina em produção. Os jobs do teste só imprimem e dormem, cada um com `timeout_secs` de no máximo 120. Ele nunca envia token errado a partir do seu endereço. Uma verificação que precisa de algo que você não informou aparece como `SKIP`, com o motivo. Duas coisas que uma máquina em produção vai sentir: a verificação 3 ocupa todos os slots livres por alguns segundos, e a 7 deixa a máquina em drenagem por cerca de um segundo. O cabeçalho do script lista todas as variáveis.

O `scripts/accept.sh` é o teste de quem desenvolve. Ele sobe a própria instância contra um Docker de verdade. Com `SHUTUP_AND_RUN_SMOKE=1` ele roda o `smoke.sh` nessa instância em seguida. O CI roda os dois a cada push, e roda o `smoke.sh` mais uma vez contra um serviço instalado pelo `install.sh`.

## Limites

- Um processo por daemon do Docker. Dois processos podem passar de `max_slots`.
- Parar o serviço não para os containers, e enquanto ele está parado ninguém aplica os timeouts.
- O timeout é SIGKILL. Não há período de tolerância.
- A `job_key` vale por máquina. Outra máquina não a conhece.
- Sem cancelamento, sem fila, sem nova tentativa de um job que falhou.
- No Windows o serviço Linux roda dentro do WSL2, só em modo pull, enquanto o usuário que instalou estiver logado. Parar o serviço mata os containers dentro da VM. Um PC que não roda WSL2 não roda o `.exe`; o fallback em shell está em [docs/WINDOWS.md](docs/WINDOWS.md#when-the-exe-cannot-run). macOS está planejado: [docs/ROADMAP-windows-macos.md](docs/ROADMAP-windows-macos.md).

## Desenvolvimento

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy --all-targets --locked -- -D warnings
```

```bash
cargo test --locked
```

O binário estático que vai nas releases:

```bash
rustup target add x86_64-unknown-linux-musl && sudo apt-get install -y musl-tools build-essential && CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl
```

Uma release é uma tag `v*` enviada a partir da `main`. Ela anexa o binário e o `SHA256SUMS`, publica a imagem e depois roda a instalação de uma linha e o smoke test em um runner limpo.

Licença MIT. Para ler mais (em inglês):

- [docs/RECIPES.md](docs/RECIPES.md): respostas de copiar e colar para casos comuns.
- [docs/CONVENTIONS.md](docs/CONVENTIONS.md): o que o operador aceita.
- [SECURITY.md](SECURITY.md): como o processo se defende.
- [docs/DECISIONS.md](docs/DECISIONS.md): por que foi construído assim.
- [docs/HANDOFF.md](docs/HANDOFF.md): notas para quem for manter ou revisar.
- [docs/SIZING.md](docs/SIZING.md): RAM e CPU do processo do launcher.
- [CONTRIBUTING.md](CONTRIBUTING.md).
- [AGENTS.md](AGENTS.md): notas para um agente de código.

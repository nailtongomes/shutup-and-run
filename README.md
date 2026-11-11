# n3-launcher

Serviço pequeno, um binário estático por máquina, que empresta a capacidade ociosa de um Ubuntu com Docker. O Kestra ou o n8n fazem `POST` numa máquina. Se ela tem slot livre, o launcher sobe o container e responde na hora. Se não tem, responde 429 e o orquestrador tenta o próximo IP da lista dele. Não há fila, não há banco, não há escalonador entre máquinas e o launcher não busca o resultado do job: o container já grava no MinIO, no Mongo ou no Redis.

Cada máquina roda o seu próprio launcher. Reiniciar o processo não zera a contagem: o slot é um container em execução com o label `n3.launcher=1`.

## Segurança

Este processo fala com o socket do Docker. Quem tem o token manda a máquina rodar uma imagem da allowlist, com o ambiente que quiser. Trate o token como senha de root.

**Escute só numa interface privada ou no IP Tailscale da máquina.** Não use `0.0.0.0`. O valor padrão, se você não configurar `listen`, é `127.0.0.1:8787`.

O token não está no código. A comparação é em tempo constante (hash SHA-256 e `subtle`). Não mande segredo de registry no pedido: o pull usa o `docker login` que já existe na máquina.

O unit sobe como root porque o socket do Docker já é equivalente a root. O arquivo do token fica em `/etc/n3-launcher/env` com modo `600`.

## Instalar

Binário estático de Linux x86_64 (musl), publicado no release quando a tag `v*` é enviada. O asset se chama `n3-launcher-x86_64-unknown-linux-musl`. Confira o `SHA256SUMS` do mesmo release se quiser.

Na máquina, como root, com o Docker já instalado e no ar:

```bash
sudo install -m 0755 n3-launcher-x86_64-unknown-linux-musl /usr/local/bin/n3-launcher
sudo mkdir -p /etc/n3-launcher
sudo tee /etc/n3-launcher/config.toml >/dev/null <<'EOF'
listen = "100.64.0.11:8787"
max_slots = 2
allowlist = [
  "ghcr.io/nailtongomes/rpa-worker-selenium",
]
default_timeout_secs = 3600
max_timeout_secs = 21600
EOF
echo "N3_TOKEN=$(openssl rand -hex 32)" | sudo tee /etc/n3-launcher/env >/dev/null
sudo chmod 600 /etc/n3-launcher/env /etc/n3-launcher/config.toml
sudo tee /etc/systemd/system/n3-launcher.service >/dev/null <<'EOF'
[Unit]
Description=n3-launcher — slots de containers RPA sob demanda
After=docker.service
Requires=docker.service

[Service]
Type=simple
ExecStart=/usr/local/bin/n3-launcher
Environment=N3_CONFIG=/etc/n3-launcher/config.toml
Environment=HOME=/root
EnvironmentFile=-/etc/n3-launcher/env
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl daemon-reload
sudo systemctl enable --now n3-launcher
sudo systemctl status n3-launcher
curl -sS http://100.64.0.11:8787/health
```

Troque `100.64.0.11` pelo IP Tailscale (ou outro IP privado) **desta** máquina. O `curl` do health tem que ser feito nesse mesmo IP, não em `0.0.0.0`.

O `openssl rand` imprime o token uma vez no terminal. Copie para o secret `N3_LAUNCHER_TOKEN` do Kestra e para o ambiente do n8n. Quem perde o token gera outro e reinicia o serviço: `sudo systemctl restart n3-launcher`.

Se você tem o repositório em vez do release, o unit também está em `deploy/n3-launcher.service` e um config de exemplo em `deploy/config.toml.example`.

### Imagem privada no GHCR

O pedido não carrega usuário nem senha. Na máquina, como root (o unit usa `HOME=/root`):

```bash
sudo docker login ghcr.io -u SEU_USUARIO
```

Isso grava `/root/.docker/config.json`. Um PAT do GitHub com `read:packages` basta. Não use um credential helper de desktop (`pass`, `secretservice`, `osxkeychain`) num servidor sem sessão: se o `config.json` apontar para um helper, o launcher chama `docker-credential-<helper> get` com o nome do registry na entrada padrão, e o helper precisa funcionar sem terminal. O caminho do arquivo pode ser outro com `docker_config` no toml ou `N3_DOCKER_CONFIG`. O arquivo é relido a cada pull, então um `docker login` novo vale sem reiniciar o launcher.

Esse login é o da registry. Não é o `N3_TOKEN` do launcher.

## Configuração

Arquivo toml em `N3_CONFIG` ou, se essa variável não existir, `/etc/n3-launcher/config.toml`. Variável de ambiente ganha do arquivo. Campo desconhecido no toml faz o processo recusar subir.

| toml | ambiente | o que é |
| --- | --- | --- |
| `listen` | `N3_LISTEN` | `ip:porta`. Padrão `127.0.0.1:8787`. |
| `token` | `N3_TOKEN` | Obrigatório, mínimo 16 caracteres, sem espaço. Prefira o env. |
| `max_slots` | `N3_MAX_SLOTS` | Obrigatório, de 1 a 256. |
| `allowlist` | `N3_ALLOWLIST` | Lista de imagens. No env, separe por vírgula. |
| `default_timeout_secs` | `N3_DEFAULT_TIMEOUT_SECS` | Se o pedido não mandar timeout. Padrão 3600. |
| `max_timeout_secs` | `N3_MAX_TIMEOUT_SECS` | Teto do pedido. Padrão 21600. Máximo 7 dias. |
| `reaper_interval_secs` | `N3_REAPER_INTERVAL_SECS` | De quanto em quanto um container vencido é morto. Padrão 2, entre 1 e 300. |
| `docker_config` | `N3_DOCKER_CONFIG` | `config.json` do Docker. Padrão `$DOCKER_CONFIG/config.json` ou `$HOME/.docker/config.json`. |

Allowlist, do jeito estrito:

- `ghcr.io/nailtongomes/rpa-worker-selenium` aceita qualquer tag ou digest desse repositório.
- `ghcr.io/nailtongomes/rpa-worker-selenium:1.4.2` aceita só essa tag (e o digest dessa mesma tag).
- Não aceita o vizinho `...-selenium-evil`. Nome com maiúscula é recusado.

Log: `journalctl -u n3-launcher -f`. O nível sai de `RUST_LOG` (padrão `info`). O token e as variáveis do job não são escritos no log.

## API

Token no header `Authorization: Bearer ...`. Health não pede token: só devolve a contagem de slots.

| método | caminho | sucesso | outros |
| --- | --- | --- | --- |
| `GET` | `/health` | 200 | 503 se o Docker não listar |
| `POST` | `/v1/jobs` | 202 com `id` | 401 token, 403 imagem, 429 cheio, 400 pedido, 502 pull ou start |
| `GET` | `/v1/jobs/{id}` | 200 `running` ou `exited` | 404 `not_found`, 401 |

O `id` é o id do container. O container sobe detached, com `--rm`: quando sai, o Docker apaga. Por isso, depois que ele some, o 200 `exited` só existe enquanto o processo do launcher ainda lembra o código de saída. Reiniciou o launcher, o id vira `not_found`.

```bash
export N3_TOKEN='cole-o-token'
export N3_URL='http://100.64.0.11:8787'

curl -sS "$N3_URL/health"

curl -sS -D - -o - -X POST "$N3_URL/v1/jobs" \
  -H "Authorization: Bearer $N3_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "image": "ghcr.io/nailtongomes/rpa-worker-selenium:latest",
    "env": { "JOB_NAME": "consulta-tj" },
    "timeout_secs": 3600,
    "cpu": 1,
    "memory": "2g",
    "shm": "2g"
  }'
```

202:

```json
{"id":"0123abcd..."}
```

429:

```json
{"error":"sem slot livre","slots_total":2,"slots_used":2,"slots_free":0}
```

401 é `{"error":"não autorizado"}`. 403 é `{"error":"imagem não permitida"}`.

Status:

```bash
curl -sS -H "Authorization: Bearer $N3_TOKEN" "$N3_URL/v1/jobs/0123abcd..."
```

```json
{"id":"0123abcd...","status":"running"}
```

```json
{"id":"0123abcd...","status":"exited","exit_code":0}
```

Corpo do `POST`, tudo opcional menos `image`:

| campo | formato |
| --- | --- |
| `image` | referência já listada na allowlist |
| `env` | objeto de strings. Sem `env`, vale o da imagem. |
| `timeout_secs` | segundos até o launcher mandar SIGKILL. Sem o campo, vale o default. Acima do máximo, 400. |
| `cpu` | número de CPUs, por exemplo `0.5` ou `1` |
| `memory` | bytes inteiros ou string no estilo Docker: `512m`, `1g`, `64mb` |
| `shm` | igual a `memory`. Chrome/Selenium costuma precisar de `1g` ou `2g`. |
| `command` | lista de strings, o `Cmd` do Docker. Omita para a imagem de RPA usar o entrypoint dela. O teste de aceitação manda `["sleep","4"]` numa `alpine`. |

Não publique porta, não monte volume e não peça privileged: a API não tem esses campos. A API não é idempotente. Não ligue retry de POST no Kestra nem no n8n para o mesmo IP: se a resposta se perder depois do container ter subido, o retry abre outro.

`memory` iguala `Memory` e `MemorySwap`, então o container não ganha swap além do limite.

## Kestra

Crie o secret `N3_LAUNCHER_TOKEN` com o mesmo valor de `N3_TOKEN`. O `timeout: PT20S` de cada task é quanto o Kestra espera a **resposta** do launcher (ele responde assim que o container sobe). A duração do RPA é `timeout_secs` dentro do JSON.

O `runIf` segue para a próxima máquina quando a anterior não devolveu 202: 429, 502, ou a máquina nem respondeu (`allowFailure` deixa o flow continuar e o output não existe). Não aumente o retry de POST do plugin HTTP; o padrão do Kestra já não retenta POST.

Duplique o bloco `maquina_N` para cada IP novo e inclua essa máquina na condição do `Fail`. O arquivo também está em `examples/kestra-disparar-rpa.yml`.

```yaml
id: disparar_rpa
namespace: n3.rpa

description: >
  Envia um job para a primeira máquina n3-launcher com slot livre.
  429 ou máquina fora do ar segue para o próximo IP.
  O flow falha só se nenhuma devolver 202.

inputs:
  - id: job_name
    type: STRING
    defaults: consulta-tj

tasks:
  - id: maquina_1
    type: io.kestra.plugin.core.http.Request
    uri: http://100.64.0.11:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
    headers:
      Authorization: "Bearer {{ secret('N3_LAUNCHER_TOKEN') }}"
    body: |
      {{ {
        "image": "ghcr.io/nailtongomes/rpa-worker-selenium:latest",
        "env": { "JOB_NAME": inputs.job_name },
        "timeout_secs": 3600,
        "shm": "2g"
      } }}

  - id: maquina_2
    type: io.kestra.plugin.core.http.Request
    runIf: "{{ outputs.maquina_1 is not defined or outputs.maquina_1.code != 202 }}"
    uri: http://100.64.0.12:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
    headers:
      Authorization: "Bearer {{ secret('N3_LAUNCHER_TOKEN') }}"
    body: |
      {{ {
        "image": "ghcr.io/nailtongomes/rpa-worker-selenium:latest",
        "env": { "JOB_NAME": inputs.job_name },
        "timeout_secs": 3600,
        "shm": "2g"
      } }}

  - id: maquina_3
    type: io.kestra.plugin.core.http.Request
    runIf: "{{ (outputs.maquina_1 is not defined or outputs.maquina_1.code != 202) and (outputs.maquina_2 is not defined or outputs.maquina_2.code != 202) }}"
    uri: http://100.64.0.13:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
    headers:
      Authorization: "Bearer {{ secret('N3_LAUNCHER_TOKEN') }}"
    body: |
      {{ {
        "image": "ghcr.io/nailtongomes/rpa-worker-selenium:latest",
        "env": { "JOB_NAME": inputs.job_name },
        "timeout_secs": 3600,
        "shm": "2g"
      } }}

  - id: nenhuma_aceitou
    type: io.kestra.plugin.core.execution.Fail
    runIf: "{{ (outputs.maquina_1 is not defined or outputs.maquina_1.code != 202) and (outputs.maquina_2 is not defined or outputs.maquina_2.code != 202) and (outputs.maquina_3 is not defined or outputs.maquina_3.code != 202) }}"
    errorMessage: "Nenhuma máquina aceitou o job (slots cheios ou launchers inacessíveis)."
```

## n8n

A mesma ideia, com um nó HTTP Request por IP, em ordem. Não use o retry do próprio nó: isso martelaria a mesma máquina cheia. O 429 (e qualquer status que não seja 202) sai pela saída falsa do IF e cai no próximo HTTP Request. Erro de conexão não derruba o workflow porque o nó está com "continuar" (`onError: continueRegularOutput`) e a resposta completa vem com `neverError`, então um 429 ainda traz `statusCode`.

Defina `N3_LAUNCHER_TOKEN` no processo do n8n e libere a leitura de ambiente (`N8N_BLOCK_ENV_ACCESS_IN_NODE=false`), ou troque a expressão do header por uma credencial Header Auth.

Para um terceiro IP, duplique o par HTTP Request + IF, ligue a saída falsa do IF anterior no novo HTTP Request, a saída verdadeira no nó Aceito, e a saída falsa do último IF no Stop and Error.

Importe `examples/n8n-disparar-rpa.json` (Workflows → Import from File). O JSON:

```json
{
  "name": "n3 — disparar RPA",
  "nodes": [
    {
      "parameters": {},
      "id": "a1b2c3d4-0001-4000-8000-000000000001",
      "name": "Quando clicar",
      "type": "n8n-nodes-base.manualTrigger",
      "typeVersion": 1,
      "position": [0, 300]
    },
    {
      "parameters": {
        "method": "POST",
        "url": "http://100.64.0.11:8787/v1/jobs",
        "sendHeaders": true,
        "headerParameters": {
          "parameters": [
            {
              "name": "Authorization",
              "value": "={{ 'Bearer ' + $env.N3_LAUNCHER_TOKEN }}"
            },
            {
              "name": "Content-Type",
              "value": "application/json"
            }
          ]
        },
        "sendBody": true,
        "specifyBody": "json",
        "jsonBody": "={{ JSON.stringify({ image: 'ghcr.io/nailtongomes/rpa-worker-selenium:latest', env: { JOB_NAME: 'exemplo' }, timeout_secs: 3600, shm: '2g' }) }}",
        "options": {
          "timeout": 20000,
          "response": {
            "response": {
              "fullResponse": true,
              "neverError": true
            }
          }
        }
      },
      "id": "a1b2c3d4-0001-4000-8000-000000000002",
      "name": "Launcher 1",
      "type": "n8n-nodes-base.httpRequest",
      "typeVersion": 4.2,
      "position": [280, 300],
      "onError": "continueRegularOutput"
    },
    {
      "parameters": {
        "conditions": {
          "options": {
            "caseSensitive": true,
            "leftValue": "",
            "typeValidation": "loose",
            "version": 2
          },
          "conditions": [
            {
              "id": "cond-202-1",
              "leftValue": "={{ $json.statusCode }}",
              "rightValue": 202,
              "operator": { "type": "number", "operation": "equals" }
            }
          ],
          "combinator": "and"
        }
      },
      "id": "a1b2c3d4-0001-4000-8000-000000000003",
      "name": "Aceito no 1?",
      "type": "n8n-nodes-base.if",
      "typeVersion": 2.2,
      "position": [560, 300]
    },
    {
      "parameters": {
        "method": "POST",
        "url": "http://100.64.0.12:8787/v1/jobs",
        "sendHeaders": true,
        "headerParameters": {
          "parameters": [
            {
              "name": "Authorization",
              "value": "={{ 'Bearer ' + $env.N3_LAUNCHER_TOKEN }}"
            },
            {
              "name": "Content-Type",
              "value": "application/json"
            }
          ]
        },
        "sendBody": true,
        "specifyBody": "json",
        "jsonBody": "={{ JSON.stringify({ image: 'ghcr.io/nailtongomes/rpa-worker-selenium:latest', env: { JOB_NAME: 'exemplo' }, timeout_secs: 3600, shm: '2g' }) }}",
        "options": {
          "timeout": 20000,
          "response": {
            "response": {
              "fullResponse": true,
              "neverError": true
            }
          }
        }
      },
      "id": "a1b2c3d4-0001-4000-8000-000000000004",
      "name": "Launcher 2",
      "type": "n8n-nodes-base.httpRequest",
      "typeVersion": 4.2,
      "position": [840, 460],
      "onError": "continueRegularOutput"
    },
    {
      "parameters": {
        "conditions": {
          "options": {
            "caseSensitive": true,
            "leftValue": "",
            "typeValidation": "loose",
            "version": 2
          },
          "conditions": [
            {
              "id": "cond-202-2",
              "leftValue": "={{ $json.statusCode }}",
              "rightValue": 202,
              "operator": { "type": "number", "operation": "equals" }
            }
          ],
          "combinator": "and"
        }
      },
      "id": "a1b2c3d4-0001-4000-8000-000000000005",
      "name": "Aceito no 2?",
      "type": "n8n-nodes-base.if",
      "typeVersion": 2.2,
      "position": [1120, 460]
    },
    {
      "parameters": {},
      "id": "a1b2c3d4-0001-4000-8000-000000000006",
      "name": "Aceito",
      "type": "n8n-nodes-base.noOp",
      "typeVersion": 1,
      "position": [1400, 200]
    },
    {
      "parameters": {
        "errorMessage": "Nenhuma máquina aceitou o job (slots cheios ou launchers inacessíveis)."
      },
      "id": "a1b2c3d4-0001-4000-8000-000000000007",
      "name": "Nenhuma aceitou",
      "type": "n8n-nodes-base.stopAndError",
      "typeVersion": 1,
      "position": [1400, 620]
    }
  ],
  "connections": {
    "Quando clicar": { "main": [[{ "node": "Launcher 1", "type": "main", "index": 0 }]] },
    "Launcher 1": { "main": [[{ "node": "Aceito no 1?", "type": "main", "index": 0 }]] },
    "Aceito no 1?": {
      "main": [
        [{ "node": "Aceito", "type": "main", "index": 0 }],
        [{ "node": "Launcher 2", "type": "main", "index": 0 }]
      ]
    },
    "Launcher 2": { "main": [[{ "node": "Aceito no 2?", "type": "main", "index": 0 }]] },
    "Aceito no 2?": {
      "main": [
        [{ "node": "Aceito", "type": "main", "index": 0 }],
        [{ "node": "Nenhuma aceitou", "type": "main", "index": 0 }]
      ]
    }
  },
  "active": false,
  "settings": { "executionOrder": "v1" },
  "pinData": {},
  "meta": { "templateCredsSetupCompleted": true },
  "tags": []
}
```

A saída de cima do IF (verdadeiro, 202) vai para Aceito e para. A de baixo (falso: 429, 401, 403, ou conexão sem `statusCode`) vai para a próxima máquina.

## Teste de aceitação

Numa Ubuntu com Docker, de preferência sem outro n3-launcher e sem container já etiquetado `n3.launcher=1`:

```bash
sudo bash scripts/accept.sh
```

Se o binário do release já estiver em disco:

```bash
sudo bash scripts/accept.sh /caminho/n3-launcher-x86_64-unknown-linux-musl
```

Sem argumento e sem `target/release/n3-launcher`, o script compila com `cargo build --release` se o Rust estiver instalado. Com systemd de verdade (sudo numa Ubuntu), ele instala o binário em `/usr/local/bin/n3-launcher`, sobe o unit `n3-launcher-accept.service` e, no fim, para e remove esse unit. Os containers `alpine sleep` do teste são apagados. O binário em `/usr/local/bin/n3-launcher` fica.

O script escreve quatro linhas. As quatro precisam aparecer:

```text
PASS: accepted until full
PASS: 429 when full
PASS: slot freed after a job ends
PASS: timeout kill
RESULT: all passed
```

Ele usa dois slots, a imagem `alpine` e `sleep`. Não aponta para o GHCR e não mexe no unit de produção `n3-launcher.service`.

## Limitações

- Um processo por daemon. Dois launchers no mesmo Docker podem passar de `max_slots`, porque o cadeado é do processo. A contagem em si é a lista do Docker.
- Parar o serviço não mata os containers. Eles seguem até sair ou até o timeout, e um launcher novo volta a enxergá-los.
- Timeout é SIGKILL, sem SIGTERM. Um Selenium travado não descarrega o que estava no meio.
- Código de saída fica na memória do processo (os últimos 256). Container com `--rm` some do Docker.
- Todo job faz pull. Se a registry falha e a imagem já está local, essa cópia local é usada.
- O log json-file do Docker pode encher o disco. Ligue a rotação no daemon.
- Não há fila, retentativa, restart de container falho, UI, banco nem coordenação entre máquinas.

## Desenvolvimento

```bash
cargo test
cargo build --release
```

O workflow `.github/workflows/ci.yml` roda os testes e confirma que o binário musl é estático. `.github/workflows/release.yml`, numa tag `v*`, anexa `n3-launcher-x86_64-unknown-linux-musl` ao GitHub Release.

Para gerar o binário na mão:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools
cargo build --release --target x86_64-unknown-linux-musl
```

O socket usado é `DOCKER_HOST`, se for `unix://...`, ou `/var/run/docker.sock`. Host TCP não é aceito.

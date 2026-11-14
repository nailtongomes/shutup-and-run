# shutup-and-run

[English](README.md)

Um runner de jobs Docker pequeno e auto-hospedado, em Rust, para a capacidade ociosa de computadores que as pessoas já têm.

Um cliente tem PCs que passam a maior parte do tempo parados. As automações dele (RPA, Selenium, scripts em container) precisam de um lugar para rodar. O shutup-and-run transforma esse PC, Windows 11 ou Linux, num worker: um orquestrador que você já usa, como n8n ou Kestra, decide o que roda e onde; este programa inicia o container na máquina do próprio cliente, limita, aplica o timeout e informa como terminou.

Ele foi feito para um **PC compartilhado**. Alguém pode estar trabalhando na máquina enquanto um job roda, e o trabalho dessa pessoa vem primeiro: um job por vez, limite de CPU e de memória em todo container, e nenhum job novo enquanto faltar memória ou disco.

Também foi feito para ser operado pelo **agente de IA** do próprio cliente. Todo comando é não interativo, imprime JSON e tem código de saída fixo. A configuração é um arquivo só, que nunca se edita à mão. Um doctor aponta cada problema com o comando que resolve.

## O que é, e o que não é

| Faz | Não faz |
| --- | --- |
| Inicia uma imagem da allowlist com o env, o comando, o timeout e os limites que a tarefa informa | Fila, agendamento, retry ou encadeamento de tarefas. Isso é do seu orquestrador |
| Recebe trabalho de dois jeitos: **push** (`POST /v1/jobs`) e **pull** (o worker consulta uma URL sua) | Coordena máquinas. Cada worker só conhece a si mesmo |
| Limita concorrência (`max_slots`), tempo (`timeout_secs`, depois SIGKILL), CPU e memória por job | Isola o job como sandbox. O container tem os privilégios e a rede padrão do Docker |
| Recusa trabalho quando está cheio, em drenagem, ou com pouca memória ou disco | Monta pastas do host, publica portas ou roda privilegiado. Esses campos não existem |
| Guarda o histórico dos jobs em SQLite e chama um webhook quando o job termina | Devolve a saída do job. O job informa um código de saída; os dados, o próprio container envia |
| Mostra uma página local com status e configuração, só na própria máquina | Oferece dashboard, ou qualquer página acessível pela rede |

## Como funciona

```text
            seu orquestrador (n8n, Kestra, sua própria API)
            -----------------------------------------------
              guarda a fila, escolhe a máquina, recebe o callback
                 |                                   ^         ^
   push: POST /v1/jobs                  pull: POST poll_url    | callback_url
   202 aceito / 429 cheio / 503         "tem trabalho?"        | quando o job termina
                 v                                   |         |
   Servidor Linux (seu, confiável)      PC Windows 11 (do cliente, compartilhado)
   -------------------------------      -----------------------------------------
   shutup-and-run.service               shutup-and-run.exe: supervisor, tarefa de logon
   HTTPS :8787, token bearer              distribuição WSL2 "shutup-and-run"
   Docker Engine                            o mesmo binário Linux, em modo pull
                                            Docker Engine (sem Docker Desktop)
                                            página local em 127.0.0.1:8787
```

- **Linux** roda o serviço sob systemd. Normalmente recebe push por HTTPS. Também pode fazer pull, para uma máquina atrás de NAT.
- **Windows 11** roda o mesmo binário Linux dentro de uma distribuição WSL2 própria, só em modo pull. Nenhuma porta é aberta no PC e nada é enviado para ele. O `.exe` é um supervisor: instala a distribuição, mantém o serviço rodando enquanto o usuário está logado, e nunca mostra janela. Detalhes: [docs/WINDOWS.md](docs/WINDOWS.md).
- **Um job** é um container. `202` quer dizer que a máquina aceitou e reservou um slot; a imagem é baixada depois. Quando termina, sozinho, pelo timeout, ou porque não conseguiu iniciar, o resultado vai por POST para a `callback_url` da tarefa.
- **O contrato** para um orquestrador, com JSON Schemas, é [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md). Um exemplo completo em n8n, com aprovação de workers, está em [examples/n8n](examples/n8n/README.md).

## Início rápido

### PC Windows 11, modo pull

O WSL2 precisa ser habilitado uma vez, como administrador, seguido de um reinício: `wsl --install --no-distribution`. Depois, no PowerShell, como o usuário que fica logado:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

```powershell
shutup-and-run wsl install --poll-url "https://orchestrator.example/poll?user=ana"
```

A primeira linha baixa o `.exe`, confere o SHA-256 e coloca no `PATH` do usuário (abra outro terminal se `shutup-and-run` não for encontrado). A segunda leva alguns minutos e termina com o doctor. Pode rodar de novo sem risco. Sem `--poll-url` a máquina é instalada ociosa, para conferir antes.

```powershell
shutup-and-run doctor
```

O worker passa a consultar a sua URL com um token próprio. O seu orquestrador vê um token desconhecido e aprova a máquina ou não: [cadastro](docs/WINDOWS.md#install). A pessoa no PC tem um atalho na área de trabalho para a página local, `http://127.0.0.1:8787/`.

### Servidor Linux, modo push

Ubuntu com endereço público:

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
```

Instala o Docker se faltar, sobe o serviço na porta 8787 em HTTPS com certificado autoassinado, e imprime a URL e a impressão digital do certificado. Leia o token e envie um job de qualquer lugar:

```bash
sudo sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' /etc/shutup-and-run/env
```

```bash
curl -sk -X POST https://203.0.113.10:8787/v1/jobs -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"image":"ghcr.io/you/worker","timeout_secs":600,"job_key":"hello-1","env":{"JOB_NAME":"hello"}}'
```

```bash
curl -sk -H "Authorization: Bearer $SAR_TOKEN" "https://203.0.113.10:8787/v1/jobs/COLE_O_ID?logs=true"
```

`-k` pula a verificação do certificado; como fixar a impressão digital está em [docs/RECIPES.md](docs/RECIPES.md). Fluxos prontos: [examples/kestra-dispatch.yml](examples/kestra-dispatch.yml), [examples/n8n-dispatch.json](examples/n8n-dispatch.json), e para pull [examples/kestra-poll.yml](examples/kestra-poll.yml), [examples/n8n-poll.json](examples/n8n-poll.json).

## Primeira execução, sem orquestrador

Cinco comandos, todos não interativos, todos com `--json`. Nenhum altera a configuração do worker nem o histórico de tarefas.

```
shutup-and-run version --json                # o build, a plataforma, o runtime (docker ou wsl2), a versão da API
shutup-and-run config check                  # pré-checagem: config, Docker, armazenamento, limites, allowlist, host do poll
shutup-and-run demo                          # um container pelo caminho do próprio worker
shutup-and-run job validate task.json        # esta máquina aceitaria esta tarefa? Nada roda
shutup-and-run demo sign                     # um container assina um PDF fictício; a assinatura é verificada
```

- **`demo`** inicia `hello-world` (ou `--image`) do jeito que uma tarefa inicia: a allowlist, os limites por job e a admissão valem. Ele confere que o container saiu com 0, que o Docker aplicou mesmo os limites de CPU e memória, e que nada sobrou. Uma imagem que a máquina não pode rodar é uma falha com o comando que a libera, não algo que a demo contorna.
- **`job validate`** faz a mesma checagem do `POST /v1/jobs` num arquivo com uma tarefa (`-` para a entrada padrão, `--poll` para conferir como tarefa de poll). Sai com 3 e um código (`JOB_INVALID_JSON`, `JOB_INVALID_FIELD`, `JOB_IMAGE_NOT_ALLOWED`) quando a API responderia `400` ou `403`. Não baixa imagem, não ocupa slot, não grava histórico, e uma tarefa válida ainda pode ser recusada depois: slots, pausa, memória e disco livres e o download da imagem só são conferidos no envio.
- **`config check`** sai com 3 quando a própria configuração é inválida e com 1 quando algo em volta está errado. Uma URL de poll cujo host não responde em 8 segundos é um aviso (`POLL_UNREACHABLE`), não uma configuração inválida.
- **`demo sign`** precisa do programa openssl e da imagem `python:3.12-slim` na allowlist. Usa um certificado descartável, a menos que receba `--configured`. O PDF assinado vai para `--out` (padrão `shutup-and-run-demo-signed.pdf`).
- **`doctor --json`** dá a cada checagem um `code` estável ao lado de `name`, `status`, `detail` e `fix`: `DOCKER_UNAVAILABLE`, `CONFIG_INVALID`, `SERVICE_NOT_RUNNING`, `POLL_NOT_APPROVED`, `LOW_MEMORY`, e `NOME_OK` para a checagem que passou. A lista está em [docs/FOR-AGENTS.md](docs/FOR-AGENTS.md#diagnostic-codes).

## Protegendo o PC

O trabalho interativo do cliente tem prioridade sobre a vazão de jobs. Num PC compartilhado, estas camadas garantem isso:

| Camada | Padrão do `shared-pc` | Onde se ajusta |
| --- | --- | --- |
| A VM do WSL2 (só Windows) | cerca de 40% da RAM, metade dos processadores lógicos | `%UserProfile%\.wslconfig`, por `shutup-and-run wsl limits` |
| Cada job | 1 CPU, 2 GiB de memória, sem swap além disso | `max_job_cpus`, `max_job_memory` |
| Jobs ao mesmo tempo | 1 | `max_slots` |
| Iniciar um job | só com 2 GiB de memória e 5 GiB de disco livres | `min_free_memory`, `min_free_disk` |

- Quem aplica os limites é o Docker (`NanoCpus`, `Memory`, `MemorySwap`), num job de push e num de pull. Tarefa que não informa `cpu` ou `memory` recebe o limite da máquina; a que informa menos recebe o que pediu; a que informa mais é recusada, com o limite na resposta.
- Enquanto faltar memória ou disco, um push recebe `503` `insufficient resources` e um worker em pull simplesmente não pede trabalho, então nenhuma tarefa é retirada da fila e depois descartada. Ele volta sozinho. O `status` mostra `waiting_for`.
- Onde o serviço enxerga menos de 4 GiB (um PC de 8 GiB dá ao WSL2 cerca de 3,2 GiB), os dois valores de memória são a metade do que ele enxerga. Nem todo PC aguenta um job de 2 GiB.
- O perfil `shared-pc` é o padrão no Windows, inclusive para um worker instalado antes da 0.2.6. Num desktop Linux: `shutup-and-run config set resource_profile shared-pc --apply`. Um servidor Linux não muda, a menos que você configure.
- O `.wslconfig` é do usuário e vale para todas as distribuições WSL2. A instalação só o escreve quando ele não existe e o PC não tem outra distribuição. Um arquivo existente só muda com `shutup-and-run wsl limits --apply`, que preserva todas as outras linhas e guarda um backup. A mudança vale quando o WSL reinicia; este programa nunca roda `wsl --shutdown`.

```
shutup-and-run status --json          # "limits" em vigor, e "waiting_for"
shutup-and-run doctor                 # a linha "resources"; no Windows também "wsl-limits"
shutup-and-run config set max_job_memory 1g --apply
shutup-and-run wsl limits             # Windows: o PC, o orçamento, o que o .wslconfig diz
```

**Isso reduz o risco; não elimina.** A memória livre é medida dentro da VM do WSL2, o que não é o mesmo que memória livre no Windows, e só no momento em que o job inicia. O uso de disco e de rede de um job não é limitado. Não há detecção de ociosidade, nem daemon de monitoramento, e um job em execução nunca é interrompido para abrir espaço. O PC ainda pode ficar lento. O botão de pausa na página local, ou `shutup-and-run pause`, é como a pessoa no PC diz "agora não". Mais: [docs/WINDOWS.md](docs/WINDOWS.md#sharing-the-pc).

## A página local

Com `status_page = true` (a instalação no Windows liga; a do Linux não), o serviço responde a um navegador **na própria máquina** em `http://127.0.0.1:8787/`, o endereço de `listen`. No Windows a instalação põe um atalho `shutup-and-run` para ela na área de trabalho.

| Página | O que tem | Login |
| --- | --- | --- |
| `/` | Rodando, pausado ou Docker parado. Se o PC pode ser desligado. Tarefas agora, a última tarefa, se a fila responde, o nome da máquina. Um botão para pausar ou voltar a aceitar tarefas. Recarrega a cada 10 segundos | nenhum |
| Configuração (`/local/config`) | URL de poll, intervalo de poll, tarefas ao mesmo tempo, nome da máquina | nenhum |
| Configuração avançada (`/local/advanced`) | Imagens permitidas, qualquer imagem, limites de tempo, os dois tokens, o login de admin | `admin` / `admin` até ser trocado |
| Certificado (`/local/certificate`) | [Repasse de certificado](#repasse-de-certificado-opcional), opcional: importar um `.pfx` e a senha, validar, testar assinatura de PDF, desligar. A senha nunca é mostrada | o mesmo login, e uma confirmação |

- A página edita o mesmo `config.json` que o CLI, pela mesma validação. Uma mudança salva reinicia o serviço com o arquivo novo; os containers em execução não são tocados. Um valor inválido é recusado e nada é gravado.
- Troque a senha de admin com `shutup-and-run config set admin_password "NOVA" --apply`.
- Os limites de recursos e as demais chaves são só pelo CLI. `shutup-and-run config keys` diz onde cada chave pode ser editada.
- Ela fica em inglês ou em português, um idioma por vez; um link troca e a escolha é lembrada.

A segurança da página, sem rodeios. Ela não pede token, então só responde a uma conexão de loopback que nomeia um host de loopback, e só aceita um formulário da própria origem. Nunca mostra token, env de job nem logs. O login de admin afasta um curioso e **não** é uma barreira de segurança: quem usa o PC consegue ler o arquivo de configuração. Nada nela é acessível pela rede, e ela não foi feita para ficar atrás de um proxy. Numa máquina configurada com `config.toml` a página só mostra o status.

## Repasse de certificado (opcional)

Desligado por padrão. Algumas tarefas assinam documentos com o certificado digital do dono, um arquivo PKCS#12 (`.pfx`, `.p12`) e a senha dele. O dono da máquina pode importar um, e a partir daí **todo container que esta máquina iniciar** consegue ler os dois. Não há lista por imagem, por decisão: ou o dono confia em toda tarefa, ou deixa desligado.

> Quando ativado, todo container executado por este worker pode acessar o certificado configurado e a senha dele. A montagem somente leitura protege o arquivo original contra alteração, mas não impede copiar, exportar ou usar a chave privada. Ative este recurso apenas se você confia em toda carga de trabalho que pode rodar nesta máquina.

```
SHUTUP_AND_RUN_CERT_PASSWORD='...' shutup-and-run certificate set --file "C:\Users\Ana\cert.pfx" --accept-risk --apply
shutup-and-run certificate status          # ligado ou não, e qual arquivo foi importado
shutup-and-run certificate validate        # abre com a senha, tem chave, não expirou
shutup-and-run demo sign --configured      # assina um PDF fictício com ele, num container
shutup-and-run certificate disable --forget --apply
```

O mesmo está na página local, em Configuração avançada, **Certificado**: escolha o arquivo, digite a senha, marque a confirmação. A senha nunca é mostrada de novo, e nunca é um argumento de linha de comando: vem de `SHUTUP_AND_RUN_CERT_PASSWORD` ou, com `--password-stdin`, da entrada padrão.

O que o container recebe, qualquer que fosse o nome do arquivo original:

| | |
| --- | --- |
| `/run/shutup-and-run/certificate/client.pfx` | O certificado, somente leitura. Também em `SHUTUP_AND_RUN_CERT_PATH`. |
| `/run/shutup-and-run/secrets/certificate-password` | A senha, como único conteúdo do arquivo, somente leitura. Também em `SHUTUP_AND_RUN_CERT_PASSWORD_FILE`. |

As duas variáveis contêm caminhos, nunca a senha. Uma tarefa não muda nenhum dos caminhos, não tem campo que monte nada, e os valores que ela mesma der a essas duas variáveis são descartados.

Como fica guardado. `certificate set` confere o arquivo primeiro (precisa abrir com a senha, ter chave privada e não estar expirado) e depois o copia para junto da configuração, em `/etc/shutup-and-run/certificate/`, modo 600, com a senha num arquivo próprio. Nenhum dos dois está no `config.json`, em log, no histórico de tarefas, em callback, em relatório, nem em nada enviado à URL de poll. O arquivo original é só lido. Cada tarefa recebe suas próprias cópias em `/run/shutup-and-run/jobs/<id>/`, que é memória e não disco, e elas são apagadas quando a tarefa termina, também depois de timeout, falha ao iniciar ou reinício. A troca de certificado vale para a próxima tarefa; a que está rodando fica com o que tinha.

O que isto não é. Não é isolamento nem cofre. A senha importada é um arquivo comum que o root da máquina lê; no Windows essa máquina é a distribuição WSL2, e o usuário dela é root ali. Apagar um arquivo não o elimina do disco. `certificate validate` diz que o arquivo é utilizável, não que o sistema de um tribunal ou de um banco o aceita. `demo sign` faz uma assinatura básica de PDF (`adbe.pkcs7.detached`) e a verifica com openssl; não é PAdES e não diz nada sobre ICP-Brasil.

## Configuração

Uma máquina tem uma única fonte de configuração.

**`config.json`** guarda todas as chaves, tokens inclusive, modo 600, sem variáveis de ambiente por cima. É `/etc/shutup-and-run/config.json`; no Windows esse caminho fica dentro da distribuição WSL, e os mesmos comandos chegam nele. A instalação no Windows o escreve. Só muda pelo CLI ou pela página local. Schema: [docs/schemas/worker-config.schema.json](docs/schemas/worker-config.schema.json).

```
shutup-and-run config show                      # o arquivo todo, segredos como ***
shutup-and-run config keys                      # cada chave, o tipo, onde se edita
shutup-and-run config get max_slots
shutup-and-run config set poll_url "https://queue.example/poll?user=ana" --apply
shutup-and-run config unset max_job_memory --apply
shutup-and-run config validate                  # sai com 3 e o motivo quando inválido
shutup-and-run config apply                     # reinicia com o arquivo; containers continuam
shutup-and-run config reset                     # config de fábrica; tokens, nome e endereço mantidos
shutup-and-run status                           # idle, active, draining, failed, stopped
shutup-and-run doctor [--report]                # toda linha com falha tem um fix
shutup-and-run pause | resume
```

Todos são não interativos e aceitam `--json`. Códigos de saída: `0` feito, `1` falhou ou não está saudável, `2` linha de comando errada, `3` configuração ou valor inválido (nada foi gravado). Um valor inválido nunca é salvo, e um arquivo danificado de outro jeito não derruba o serviço: ele roda com a cópia do último salvamento, ou com os padrões ociosos, e avisa.

**`config.toml` mais ambiente** é o que o instalador do Linux ainda escreve: `/etc/shutup-and-run/config.toml`, ou o arquivo indicado por `SHUTUP_AND_RUN_CONFIG`. Variáveis de ambiente sobrepõem o arquivo. Depois de mudar: `sudo systemctl restart shutup-and-run`; os jobs em execução sobrevivem. Numa máquina assim `shutup-and-run config` recusa e explica; `status`, `doctor`, `pause`, `resume` e `bans` funcionam. Uma máquina Linux usa `config.json` quando esse arquivo existe e `SHUTUP_AND_RUN_CONFIG` não aponta para um TOML. Não há conversor.

As chaves são as mesmas nos dois. Chave desconhecida é recusada.

| Chave | Ambiente | Significado |
| --- | --- | --- |
| `listen` | `SHUTUP_AND_RUN_LISTEN` | `ip:porta`. Padrão `127.0.0.1:8787`. O instalador do Linux escreve `0.0.0.0:8787`. |
| `token` | `SHUTUP_AND_RUN_TOKEN` | Obrigatório. Pelo menos 16 caracteres. O instalador do Linux guarda em `/etc/shutup-and-run/env`. |
| `max_slots` | `SHUTUP_AND_RUN_MAX_SLOTS` | Obrigatório. 1 a 256. O Windows instala 1. O instalador do Linux sugere um por CPU e por 2 GiB. |
| `allowlist` | `SHUTUP_AND_RUN_ALLOWLIST` | Imagens que podem rodar. Obrigatório, exceto com `poll_any_image`. Separadas por vírgula no ambiente. |
| `resource_profile` | `SHUTUP_AND_RUN_RESOURCE_PROFILE` | `shared-pc` ou `none`. Padrão `shared-pc` quando `worker_os` é `windows`, senão `none`. Veja [Protegendo o PC](#protegendo-o-pc). |
| `max_job_cpus` | `SHUTUP_AND_RUN_MAX_JOB_CPUS` | Máximo de CPU por job, e o padrão: `"1"`, `"0.5"`. Sem valor no perfil `none`: sem limite. |
| `max_job_memory` | `SHUTUP_AND_RUN_MAX_JOB_MEMORY` | O mesmo para memória: `"2g"`, `"512m"`. |
| `min_free_memory` | `SHUTUP_AND_RUN_MIN_FREE_MEMORY` | Nenhum job novo abaixo dessa memória disponível. `"0"`: não confere. |
| `min_free_disk` | `SHUTUP_AND_RUN_MIN_FREE_DISK` | O mesmo para o disco em que o Docker grava. |
| `default_timeout_secs` | `SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS` | Padrão 3600. |
| `max_timeout_secs` | `SHUTUP_AND_RUN_MAX_TIMEOUT_SECS` | Padrão 21600. No máximo 7 dias. |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Opcional. Liga o modo pull. |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Padrão 15. |
| `poll_token` (só JSON) | `SHUTUP_AND_RUN_POLL_TOKEN` | Quem esta máquina é para a URL de poll. Não é chave do TOML. |
| `poll_any_image` | `SHUTUP_AND_RUN_POLL_ANY_IMAGE` | Uma tarefa de poll pode indicar qualquer imagem. Precisa de `poll_url`. Permite `allowlist` vazia. Nunca vale para um push. |
| `worker_name`, `worker_os` | `SHUTUP_AND_RUN_WORKER_NAME`, `_WORKER_OS` | Vão no corpo do poll. Padrões: o hostname, o SO do binário. |
| `status_page` | `SHUTUP_AND_RUN_STATUS_PAGE` | A página local. Padrão false. |
| `admin_user`, `admin_password` (só JSON) | | O login da configuração avançada da página. |
| `http` | `SHUTUP_AND_RUN_HTTP` | `true` serve sem TLS. Só para localhost. |
| `tls_cert`, `tls_key` | `SHUTUP_AND_RUN_TLS_CERT`, `_TLS_KEY` | Arquivos PEM. Obrigatórios, exceto com `http = true`. |
| `auth_failures`, `ban_hours` | `SHUTUP_AND_RUN_AUTH_FAILURES`, `_BAN_HOURS` | Tokens errados antes do banimento (3), e a duração dele (48). |
| `trust_x_forwarded_for` | `SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR` | Só atrás de um proxy seu. Padrão false. |
| `history_path`, `history_days` | `SHUTUP_AND_RUN_HISTORY_PATH`, `_HISTORY_DAYS` | O arquivo SQLite, e por quanto tempo os jobs terminados ficam (30). |
| `log_lines` | `SHUTUP_AND_RUN_LOG_LINES` | Linhas de log guardadas por job. Padrão 200. |
| `prune_interval_secs` | `SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS` | Remove containers terminados e imagens sem uso nesse intervalo. Padrão 0, desligado. |
| `reaper_interval_secs`, `purge_interval_secs` | `SHUTUP_AND_RUN_REAPER_INTERVAL_SECS`, `_PURGE_INTERVAL_SECS` | De quanto em quanto os timeouts são aplicados (2) e o histórico antigo é apagado (3600). |
| `docker_config` | `SHUTUP_AND_RUN_DOCKER_CONFIG` | Caminho do `config.json` do Docker, para imagens privadas. |
| `certificate_enabled`, `certificate_consent`, `certificate_name` (só JSON) | | Repasse de certificado. Escritas por `shutup-and-run certificate set`, não à mão. Padrão desligado. |

**Allowlist.** `ghcr.io/you/worker` permite qualquer tag ou digest desse repositório e nada mais: nem `ghcr.io/you/worker-evil`, nem `ghcr.io/you/other`. Uma entrada com tag permite exatamente aquele texto. Um worker Windows usa `poll_any_image` no lugar: a única fonte de trabalho dele é a sua URL de poll.

**Imagens privadas.** `sudo docker login ghcr.io -u VOCE` na máquina. Uma tarefa não pode fornecer credenciais de registry.

**Logs.** Linux: `journalctl -u shutup-and-run -f`. Windows: `shutup-and-run wsl logs`. O token e o ambiente dos jobs nunca vão para o log.

## Para um agente de IA

[docs/FOR-AGENTS.md](docs/FOR-AGENTS.md) é o guia: instalação, os problemas conhecidos e as correções. O ciclo inteiro num PC Windows, sem nenhuma pergunta e sem editar nada à mão:

```powershell
# 1. Instalar. Sai com 0 só quando o worker está rodando.
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
shutup-and-run wsl install --poll-url "https://queue.example/poll?user=ana"

# 2. Configurar. Sai com 3 e o motivo num valor inválido; nada é gravado.
shutup-and-run config set max_job_memory 1g --apply --json

# 2b. Provar que funciona antes de qualquer orquestrador mandar trabalho.
shutup-and-run config check --json  # sai com 3: a config; com 1: Docker, armazenamento ou o host do poll
shutup-and-run demo --json          # um container, limites conferidos, nada deixado para trás
shutup-and-run job validate task.json --json   # sai com 3 e um código quando a tarefa seria recusada

# 3. Inspecionar o que está em vigor.
shutup-and-run status --json        # state, slots, limits, waiting_for, poll_error
shutup-and-run wsl limits --json    # o PC, o orçamento do WSL2, o .wslconfig

# 4. Diagnosticar. checks[] tem status ok|warn|fail, detail, e fix para cada problema.
shutup-and-run doctor --json

# 5. Corrigir com o comando em "fix" e rodar o doctor de novo. Ainda travado:
shutup-and-run doctor --report      # um relatório com segredos mascarados, e um link de issue

# 6. Reset de fábrica, do mais leve ao completo.
shutup-and-run config reset         # só a configuração; tokens mantidos
shutup-and-run wsl reset            # recria a distribuição; configuração mantida
shutup-and-run wsl reset --factory  # esquece a configuração também; precisa de nova aprovação
```

No Linux os mesmos comandos `status`, `doctor`, `pause`, `resume` e `config` rodam como root. Segredos aparecem como `***` em todo lugar, inclusive no `doctor --report`.

## Push, pull e a API

Toda rota exige `Authorization: Bearer <token>`. A única exceção é a página local.

| Rota | |
| --- | --- |
| `POST /v1/jobs` | Inicia um job. `202` aceito, `200` esta `job_key` já está aqui, `429` cheio, `503` em drenagem ou com pouca memória ou disco, `400` inválido ou acima dos limites da máquina, `403` imagem não permitida. |
| `GET /v1/jobs/{id}` | Status, código de saída, motivo. `?logs=true` acrescenta as últimas linhas de log. |
| `GET /health` | `{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}` |
| `GET /metrics` | Texto Prometheus. |
| `POST /v1/drain` | `{"draining": true}` recusa jobs novos e deixa os que estão rodando terminarem. |

Uma tarefa, o mesmo JSON num push, numa resposta de poll e num manifesto:

```json
{
  "image": "ghcr.io/you/worker:1.4.2",
  "job_key": "order-1001",
  "env": {"JOB_NAME": "example"},
  "command": ["python", "main.py"],
  "timeout_secs": 3600,
  "cpu": 1,
  "memory": "2g",
  "shm": "1g",
  "callback_url": "https://n8n.example/webhook/done"
}
```

Só `image` é obrigatório. Campos desconhecidos são recusados. `job_key` torna um retry seguro naquela máquina: a mesma chave devolve o mesmo job e não inicia nada. `reason` no resultado é `exited`, `timeout_kill` ou `start_failed`. Não existe chamada de cancelamento.

**Pull.** Com `poll_url` definida, a máquina faz POST nela a cada `poll_interval_secs`, só quando tem um slot livre, não está pausada e não está com pouca memória ou disco:

```json
{"worker": "DESK-07", "os": "windows", "arch": "x86_64", "version": "0.2.7", "max_slots": 1, "free_slots": 1, "cpus": 4, "memory_bytes": 3435973836}
```

A resposta é nada (`204`), uma tarefa, `{"task": {...}}`, ou `{"manifest_url": "https://..."}`, que é buscado uma vez e precisa ser uma tarefa. `401` ou `403` quer dizer "ainda não aprovada": o worker não inicia nada e continua perguntando, espaçando até cinco minutos. O servidor decide quais máquinas recebem trabalho; o token do worker é a identidade dele.

Limites de cada campo, todos os códigos de status, o callback, idempotência, e o que não é garantido: [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md).

**`status.json`** tem os mesmos fatos da página local, para um script que não deve chamar HTTP: `state` (`idle`, `active`, `draining`, `failed`, `stopped`), `slots_total`, `slots_used`, `job_ids`, `last_job_at`, `last_error`, `poll_error`, `waiting_for`, `limits`, `config_warning`, `version`, `pid`, `started_at`, `updated_at`. Linux: ao lado do arquivo de histórico, `/var/lib/shutup-and-run/status.json`, só root. Windows: `C:\ProgramData\shutup-and-run\status.json`, legível pelo usuário. Se `updated_at` tem mais de 60 segundos, o worker morreu. `shutup-and-run status --json` lê para você.

## Segurança

- **O token da API é uma senha de root da máquina.** Quem o tem pode rodar qualquer imagem da allowlist com qualquer ambiente. Toda rota exige o token, inclusive `/health` e `/metrics`.
- Três tokens errados do mesmo endereço o banem por 48 horas; um endereço banido recebe `403` mesmo com o token certo. `sudo shutup-and-run bans`, `sudo shutup-and-run bans lift 203.0.113.5`.
- Uma instalação Linux fala HTTPS com certificado autoassinado. Fixe a impressão digital que ela imprime, e libere a porta no firewall só para o seu orquestrador.
- Um job não pode montar o host, publicar porta nem rodar privilegiado. Ainda assim não é um sandbox.
- **Um worker Windows é um computador que você não controla.** O usuário dele consegue ler o token de poll, o env de cada tarefa e as camadas da imagem. Mande só segredos de vida curta, restritos a uma tarefa, e trate o que volta como uma alegação. Deixe o trabalho com segredos de verdade nas suas máquinas Linux.
- **O cliente está confiando no seu orquestrador.** Com `poll_any_image`, a URL de poll decide qual imagem roda no PC dele.
- A página local é para a pessoa na máquina e não é uma barreira de segurança. Veja [A página local](#a-página-local).
- **O repasse de certificado compartilha a chave privada do dono com toda tarefa.** Fica desligado até o dono ligar, na própria máquina, com uma confirmação. Veja [Repasse de certificado](#repasse-de-certificado-opcional).

Como o processo se defende: [SECURITY.md](SECURITY.md). O que o operador aceita ao rodar: [docs/CONVENTIONS.md](docs/CONVENTIONS.md).

## Instaladores

**Linux**, `scripts/install.sh`:

| Variável | |
| --- | --- |
| `SHUTUP_AND_RUN_ALLOWLIST` | Imagens do primeiro `config.toml`. Sem ela o instalador escreve um valor provisório e avisa. |
| `SHUTUP_AND_RUN_MAX_SLOTS` | Substitui a quantidade de slots sugerida. |
| `SHUTUP_AND_RUN_PORT` | Padrão 8787. |
| `SHUTUP_AND_RUN_NO_UFW=1` | Não abre a porta no ufw. |
| `SHUTUP_AND_RUN_SHOW_TOKEN=1` | Imprime o token. |
| `SHUTUP_AND_RUN_BIN=./shutup-and-run` | Instala um binário que você já tem. Sem download, sem checksum. |

O download é conferido com o `SHA256SUMS` da mesma release. Rodar de novo atualiza o binário e mantém token, certificado e configuração. `--uninstall` remove o serviço e o binário; `--uninstall --purge` também remove `/etc/shutup-and-run` e `/var/lib/shutup-and-run`. Existe uma imagem de container, `ghcr.io/nailtongomes/shutup-and-run`, para quem roda tudo sob Docker; ela precisa do socket do Docker do host, que é root no host. A instalação por systemd é o caminho suportado.

**Windows 11**, `scripts/install.ps1`, depois `shutup-and-run wsl install`:

| Comando | |
| --- | --- |
| `wsl install [--poll-url URL] [--max-slots N] [--worker-name NOME] [--wsl-memory 4GB] [--wsl-processors 2]` | Instala ou atualiza. Mantém a configuração; muda só as opções informadas. |
| `wsl doctor`, `doctor` | Confere tudo, com a correção de cada problema. |
| `wsl limits [--apply]` | Quanto do PC o WSL2 pode usar. |
| `wsl start`, `wsl stop`, `wsl logs` | A tarefa de logon e o log do supervisor. |
| `wsl reset [--factory]` | Recria uma distribuição quebrada. |
| `wsl uninstall [--purge]` | Remove a tarefa e a distribuição. |

Sem Docker Desktop e sem administrador, fora habilitar o WSL2 uma vez. O worker roda enquanto o usuário que instalou está logado. Todo o resto está em [docs/WINDOWS.md](docs/WINDOWS.md). Um PC que não roda WSL2 não roda o `.exe`; o [fallback em shell](docs/WINDOWS.md#when-the-exe-cannot-run) é outra coisa, mais fraca.

## Testes

`scripts/smoke.sh` confere uma instância em execução pela API, doze cenários, seguro numa máquina em produção. Na máquina, como root:

```bash
sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker SMOKE_RESTART_CMD="systemctl restart shutup-and-run" bash scripts/smoke.sh
```

De outra máquina:

```bash
SMOKE_URL=https://203.0.113.10:8787 SMOKE_TOKEN=... SMOKE_FINGERPRINT=AA:BB:... SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh
```

A imagem precisa de `sh` e `sleep`; a padrão é `ghcr.io/nailtongomes/shutup-and-run-smoke`, feita a partir de `examples/smoke-image`. Um teste que precisa de algo que você não informou sai como `SKIP`, com o motivo. O cabeçalho do script lista todas as variáveis.

`scripts/accept.sh` é o teste do desenvolvedor: uma instância própria contra um Docker de verdade, incluindo os limites por job no container e os comandos de primeira execução (`demo`, `job validate`, `demo sign` com um certificado montado). O CI roda ele e o `smoke.sh` a cada push, um ciclo de pull contra `tests/poll-mock`, e o `smoke.sh` de novo contra um serviço instalado pelo `install.sh`. Um teste manual de um PC Windows e de um servidor Linux, passo a passo, é [docs/TESTING.pt-BR.md](docs/TESTING.pt-BR.md).

## Limites

- Um processo por daemon do Docker. Dois podem passar de `max_slots`.
- No Linux, parar o serviço não para os containers, e enquanto ele está parado ninguém aplica os timeouts.
- Timeout é SIGKILL, sem período de tolerância.
- `job_key` vale por máquina. Outra máquina não a conhece.
- Sem cancelamento, sem fila, sem retry de job que falhou.
- No modo pull o servidor nunca fica sabendo que uma tarefa foi recusada ou descartada; o log do próprio worker fica. Veja [os limites do contrato](docs/INTEGRATION-CONTRACT.md#9-limits-of-the-current-contract).
- A proteção de recursos é controle de admissão e limites de container, não uma garantia. Veja [Protegendo o PC](#protegendo-o-pc).
- O Windows roda o serviço Linux dentro do WSL2, só em modo pull, enquanto o usuário que instalou está logado. Parar o serviço, sair da sessão ou desligar encerra os containers. macOS está planejado: [docs/ROADMAP-windows-macos.md](docs/ROADMAP-windows-macos.md).

## Projetos parecidos

Este é um processo só, numa máquina que você já tem, e o seu orquestrador escolhe a máquina.

- **Nomad** agenda jobs num cluster. Use quando um plano de controle deve decidir onde o trabalho roda.
- **HTCondor** casa jobs de alta vazão com máquinas de um pool.
- **GitLab Runner** e o **runner auto-hospedado do GitHub Actions** rodam os jobs dos seus próprios sistemas de CI.
- **Tork** é um motor de workflows com coordenador, fila e workers. Use quando você quer essa fila e os retries.
- **BOINC** é computação voluntária: desconhecidos rodam uma carga publicada. Aqui, as suas imagens rodam em máquinas que você configurou.

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

O binário estático que as releases publicam:

```bash
rustup target add x86_64-unknown-linux-musl && sudo apt-get install -y musl-tools build-essential && CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl
```

Uma release é uma tag `v*` enviada a partir do `main`. Ela anexa o binário Linux, o `.exe` do Windows e o `SHA256SUMS`, publica a imagem, e depois roda a instalação de uma linha e o smoke test num runner limpo.

Licença MIT. Mais leitura:

- [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md): o contrato para um orquestrador, com JSON Schemas em `docs/schemas`.
- [docs/WINDOWS.md](docs/WINDOWS.md): o worker Windows, e como o PC é compartilhado.
- [docs/FOR-AGENTS.md](docs/FOR-AGENTS.md): para um agente de IA que instala ou conserta um worker.
- [examples](examples/README.md): envio, job key, callback, status e falha, de ponta a ponta, para n8n e Kestra.
- [examples/n8n](examples/n8n/README.md): o n8n como orquestrador, e um smoke test.
- [docs/RECIPES.md](docs/RECIPES.md): respostas de copiar e colar para casos comuns.
- [docs/CONVENTIONS.md](docs/CONVENTIONS.md): o que o operador aceita.
- [SECURITY.md](SECURITY.md): como o processo se defende.
- [docs/DECISIONS.md](docs/DECISIONS.md): por que foi construído assim.
- [docs/HANDOFF.md](docs/HANDOFF.md): notas para o próximo mantenedor ou revisor.
- [docs/SIZING.md](docs/SIZING.md): RAM e CPU do processo do launcher.
- [CONTRIBUTING.md](CONTRIBUTING.md).
- [AGENTS.md](AGENTS.md): notas para um agente de IA que programa neste repositório.

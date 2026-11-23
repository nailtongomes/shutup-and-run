# shutup-and-run

[English](README.md)

[![CI](https://github.com/nailtongomes/shutup-and-run/actions/workflows/ci.yml/badge.svg)](https://github.com/nailtongomes/shutup-and-run/actions/workflows/ci.yml)
[![Última release](https://img.shields.io/github/v/release/nailtongomes/shutup-and-run)](https://github.com/nailtongomes/shutup-and-run/releases/latest)

**Rode os containers das suas automações nos PCs ociosos que seus clientes já têm, sem atrapalhar quem está usando.**

Um runner de jobs Docker pequeno e auto-hospedado, em Rust. Um orquestrador que você já usa (n8n, Kestra, sua própria API) entrega uma tarefa; ele inicia esse container na máquina Windows 11 ou Linux do cliente, mantém dentro dos limites, para no tempo limite e avisa você de como terminou.

![A página local de um worker rodando uma tarefa: o estado no topo, os dados, as últimas tarefas com os logs](docs/screenshots/pt-status.png)

- **Feito para um PC compartilhado.** Alguém pode estar trabalhando na máquina enquanto um job roda, e o trabalho dessa pessoa vem primeiro: limite de CPU e de memória em todo container, um job por vez por padrão, nenhum job novo enquanto faltar memória ou disco e, num PC pequeno, nada roda até o dono dizer sim.
- **Nada para abrir no PC do cliente.** Um worker Windows pede trabalho a uma URL sua. Nenhuma porta de entrada, sem Docker Desktop, sem administrador depois de habilitar o WSL2, e sem janela.
- **Feito para ser operado por um agente de IA.** Todo comando é não interativo, imprime JSON e tem código de saída fixo. A configuração é um arquivo só, que nunca se edita à mão. Um doctor aponta cada problema com o comando que resolve.
- **Ou só a internet.** Um PC fraco demais para containers empresta a conexão de casa: seus containers, em qualquer lugar, acessam os sites com o IP do cliente, por um relay no seu servidor (um `docker run`). Os dois podem rodar no mesmo PC.
- **Pequeno.** Um binário estático, um histórico em SQLite, uma página local em `127.0.0.1` para a pessoa no PC, e o que cada job imprimiu guardado na máquina por 7 dias.

**Dúvidas?** O [FAQ](docs/FAQ.pt-BR.md) responde o que esperar: PC lento, bateria, desligamento, uma tarefa que não começou.

**Conteúdo:** [Formas de emprestar um PC](#formas-de-emprestar-um-pc) · [Como funciona](#como-funciona) · [Início rápido](#início-rápido) · [A página local](#a-página-local) · [Protegendo o PC](#protegendo-o-pc) · [Containers gerenciados](#containers-gerenciados) · [Para um agente de IA](#para-um-agente-de-ia) · [A API](#push-pull-e-a-api) · [Segurança](#segurança) · [Limites](#limites)

## Formas de emprestar um PC

| Modo | Para | O cliente (ou a IA dele) | Você |
| --- | --- | --- | --- |
| **Containers, por pull** | Um PC com WSL2 e folga (`shutup-and-run fit` diz `both`) | A linha de instalação com a sua URL de poll. Nada para abrir no PC | Responde o poll com uma tarefa ([contrato](docs/INTEGRATION-CONTRACT.md#3-poll)) |
| **Containers, por push** | Um servidor Linux seu, que você alcança | `install.sh` | `POST /v1/jobs` com o token. Não serve para PC de cliente atrás de roteador |
| **Containers gerenciados** | Uma máquina que roda os próprios containers, sem URL | `managed apply` uma vez | Nada ([containers gerenciados](#containers-gerenciados)) |
| **Só a internet** | Um PC fraco, ou sem WSL2 (`fit` diz `internet`) | A linha de instalação com o seu relay, ou `SHUTUP_AND_RUN_LEND_INTERNET=1` e a sua URL de poll diz onde ele está | Um relay: um `docker run` ou uma linha ([seu servidor](docs/SERVIDOR.pt-BR.md)). Suas tarefas, em qualquer lugar, usam `HTTPS_PROXY` ([exemplos](docs/INTERNET.md#examples-curl-requests-selenium-playwright-puppeteer-node-n8n)) |
| **Os dois** | Um PC que roda containers e empresta a conexão para as suas outras máquinas | A linha de instalação com a URL de poll e `SHUTUP_AND_RUN_LEND_INTERNET=1` | Os dois acima; uma tarefa que não cabe no PC vai para o seu servidor e sai pela conexão do PC |

Os passos de um agente de IA para todos eles: [docs/FOR-AGENTS.md](docs/FOR-AGENTS.md#a-task-from-the-operator-run-it-here-lend-the-internet-or-both).

## O que é, e o que não é

| Faz | Não faz |
| --- | --- |
| Inicia uma imagem da allowlist com o env, o comando, o timeout e os limites que a tarefa informa | Fila, retry ou encadeamento de tarefas. Isso é do seu orquestrador |
| Ou, no [modo gerenciado](#containers-gerenciados), roda containers definidos na própria máquina: uma vez, em intervalo, ou sempre | Substitui Compose, Kubernetes ou um motor de workflow. Um container gerenciado é uma imagem e quando rodar |
| Recebe trabalho de dois jeitos: **push** (`POST /v1/jobs`) e **pull** (o worker consulta uma URL sua) | Coordena máquinas. Cada worker só conhece a si mesmo |
| Limita concorrência (`max_slots`), tempo (`timeout_secs`, depois SIGTERM e SIGKILL 10 segundos mais tarde), CPU e memória por job | Isola o job como sandbox. O container tem os privilégios e a rede padrão do Docker |
| Recusa trabalho quando está cheio, em drenagem, ou com pouca memória ou disco | Monta pastas do host, publica portas ou roda privilegiado. Esses campos não existem |
| Guarda o histórico dos jobs em SQLite e chama um webhook quando o job termina | Devolve a saída do job para você. O job informa um código de saída e os dados, o próprio container envia; o que ele imprimiu fica na máquina por 7 dias |
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
- **Um job** é um container. `202` quer dizer que a máquina aceitou e reservou um slot; a imagem é baixada depois, com 30 minutos no máximo para o download, e o timeout conta a partir do início do container. Uma imagem pedida pelo digest (`repo@sha256:...`) que já está na máquina não é pedida de novo ao registro. Quando termina, sozinho, pelo timeout, ou porque não conseguiu iniciar, o resultado vai por POST para a `callback_url` da tarefa.
- **O contrato** para um orquestrador, com JSON Schemas, é [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md). Um exemplo completo em n8n, com aprovação de workers, está em [examples/n8n](examples/n8n/README.md).

A vida de uma tarefa no modo pull, o jeito do Windows:

```mermaid
sequenceDiagram
    participant O as Seu orquestrador
    participant W as shutup-and-run no PC
    participant D as Docker
    W->>O: POST poll_url: quem sou, vagas livres, CPUs, memória
    O-->>W: uma tarefa: imagem, env, timeout, callback_url, job_key
    Note over W: allowlist, vaga livre, memória e disco,<br/>o consentimento do dono num PC pequeno
    W->>D: inicia o container, com limite de CPU e memória
    D-->>W: ele termina, ou é morto no tempo limite
    Note over W: guarda os últimos 256 KiB que ele imprimiu, 7 dias
    W->>O: POST callback_url: status, código de saída, job_key
```

No modo push os dois primeiros passos são um `POST /v1/jobs` do seu orquestrador para o worker, respondido com `202`.

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
- **`doctor --json`** dá a cada checagem um `code` estável ao lado de `name`, `status`, `detail` e `hint`: `DOCKER_UNAVAILABLE`, `CONFIG_INVALID`, `SERVICE_NOT_RUNNING`, `POLL_NOT_APPROVED`, `LOW_MEMORY`, e `NOME_OK` para a checagem que passou. A lista está em [docs/FOR-AGENTS.md](docs/FOR-AGENTS.md#diagnostic-codes).

## Protegendo o PC

O trabalho interativo do cliente tem prioridade sobre a vazão de jobs. Num PC compartilhado, estas camadas garantem isso:

| Camada | Padrão do `shared-pc` | Onde se ajusta |
| --- | --- | --- |
| A VM do WSL2 (só Windows) | cerca de 40% da RAM, metade dos processadores lógicos | `%UserProfile%\.wslconfig`, por `shutup-and-run wsl limits` |
| Cada job | 1 CPU, 2 GiB de memória, sem swap além disso | `max_job_cpus`, `max_job_memory` |
| Jobs ao mesmo tempo | 1 | `max_slots` |
| Iniciar um job | só com 2 GiB de memória e 5 GiB de disco livres | `min_free_memory`, `min_free_disk` |
| Iniciar um job (Windows, opcional) | só depois de o PC ficar N segundos sem uso; desligado por padrão | `min_idle_secs` |
| Iniciar um job, quando (opcional) | só dentro do horário em que a máquina é emprestada, como `mon-fri 19-07; sat,sun`; a qualquer hora por padrão | `lend_hours` |
| Iniciar um job (Windows) | não na bateria abaixo de 60%, não enquanto a pessoa apresenta ou tem um programa em tela cheia; na bateria acima de 60% os jobs começam e a página avisa | não é configurável |
| Iniciar um job (Windows) | não numa rede limitada (o roteador do celular, um plano móvel), a menos que quem usa o PC aceite gastar esses dados | `lend_on_metered` |
| Iniciar um job, a segurança do computador | nunca com memória, disco ou processador em nível crítico; num computador pequeno ou ocupado, só depois que o dono autorizar | não é configurável; o dono decide na página local |

- Quem aplica os limites é o Docker (`NanoCpus`, `Memory`, `MemorySwap`), num job de push e num de pull. Tarefa que não informa `cpu` ou `memory` recebe o limite da máquina; a que informa menos recebe o que pediu; a que informa mais é recusada, com o limite na resposta.
- Fora de `lend_hours`, na bateria abaixo de 60%, enquanto a pessoa apresenta ou tem um programa em tela cheia, ou numa rede limitada, um push recebe `503` `not lending now` e um worker em pull não pede trabalho. Um job que já está rodando continua em todos os casos.
- Enquanto faltar memória ou disco, um push recebe `503` `insufficient resources` e um worker em pull simplesmente não pede trabalho, então nenhuma tarefa é retirada da fila e depois descartada. Ele volta sozinho. O `status` mostra `waiting_for`.
- Antes de cada job o worker também avalia o computador em si, no Windows o PC e não só a VM do WSL2: **saudável**, **alerta** ou **bloqueado**. Bloqueado (menos de 10% de memória ou 2 GiB de disco, processadores saturados, uma leitura que não pôde ser feita, três jobs mortos por memória em meia hora) não inicia nada, responde um push com `503` `blocked for safety` e passa sozinho. Um alerta (menos de 8 GiB de memória ou 2 processadores ou menos, memória abaixo de 20%, processadores ocupados) só inicia um job depois que o dono do computador leu os riscos e disse sim, uma vez, na página local ou com `shutup-and-run safety authorize` em um terminal; até lá um push recebe `503` `authorization required`. Quando o dono já concordou no contrato, quem instala registra esse consentimento na própria máquina com `shutup-and-run wsl install --owner-consent` (ou `shutup-and-run safety authorize --owner-consent`), e o PC fica pronto na hora. Nenhuma chave de config ou campo da API diz sim, e o sim nunca libera um bloqueio. `shutup-and-run safety` mostra o estado; um job morto por memória é informado como tal (`oom_killed`). Os limiares e o que não é coberto estão em [docs/WINDOWS.md](docs/WINDOWS.md#sharing-the-pc).
- Onde o serviço enxerga menos de 4 GiB (um PC de 8 GiB dá ao WSL2 cerca de 3,2 GiB), os dois valores de memória são a metade do que ele enxerga. Nem todo PC aguenta um job de 2 GiB.
- O perfil `shared-pc` é o padrão no Windows, inclusive para um worker instalado antes da 0.2.6. Num desktop Linux: `shutup-and-run config set resource_profile shared-pc --apply`. Um servidor Linux não muda, a menos que você configure.
- O `.wslconfig` é do usuário e vale para todas as distribuições WSL2. A instalação só o escreve quando ele não existe e o PC não tem outra distribuição. Um arquivo existente só muda com `shutup-and-run wsl limits --apply`, que preserva todas as outras linhas e guarda um backup. A mudança vale quando o WSL reinicia; este programa nunca roda `wsl --shutdown`.

```
shutup-and-run status --json          # "limits" em vigor, e "waiting_for"
shutup-and-run doctor                 # a linha "resources"; no Windows também "wsl-limits"
shutup-and-run config set max_job_memory 1g --apply
shutup-and-run wsl limits             # Windows: o PC, o orçamento, o que o .wslconfig diz
```

**Isso reduz o risco; não elimina.** A memória livre é medida dentro da VM do WSL2, o que não é o mesmo que memória livre no Windows, e só no momento em que o job inicia. O uso de disco e de rede de um job não é limitado. Não há detecção de ociosidade, nem daemon de monitoramento, e um job em execução nunca é interrompido para abrir espaço. O PC ainda pode ficar lento. O botão de pausa na página local, ou `shutup-and-run pause`, é como a pessoa no PC diz "agora não"; ele também pausa por 1 hora, 3 horas ou pelo resto do dia (`pause --for 2h`, `--for day`, `--until 08:00`) e volta a aceitar sozinho. Mais: [docs/WINDOWS.md](docs/WINDOWS.md#sharing-the-pc).

## A página local

Com `status_page = true` (a instalação no Windows liga; a do Linux não), o serviço responde a um navegador **na própria máquina** em `http://127.0.0.1:8787/`, o endereço de `listen`. No Windows a instalação põe um atalho `shutup-and-run` para ela na área de trabalho.

A página de status está no [topo deste arquivo](#shutup-and-run). As outras:

| Esperando o dono | Tarefas e logs |
| --- | --- |
| ![Um PC pequeno espera a autorização do dono; o risco e o botão para decidir ficam no topo](docs/screenshots/pt-authorization.png) | ![Tarefas e logs: cada tarefa com status, início, duração, código de saída e log](docs/screenshots/pt-jobs.png) |
| **Configuração avançada** | **Tema escuro** |
| ![Configuração avançada: agrupada, cada campo com a ajuda, campos protegidos marcados](docs/screenshots/pt-advanced.png) | ![A página de status no tema escuro](docs/screenshots/pt-dark.png) |

<img src="docs/screenshots/pt-phone.png" alt="A página de status num celular" width="260" align="right">

Ela segue o tema claro ou escuro do sistema e funciona no celular, sem rede: um CSS só dentro do binário, sem fonte, sem framework. Os horários aparecem no fuso do próprio computador.

<br clear="right">

| Página | O que tem | Login |
| --- | --- | --- |
| `/` | O estado numa palavra e numa cor: rodando e livre, trabalhando, pausado, esperando a sua autorização, bloqueado, Docker parado. Logo abaixo, o que precisa de uma pessoa, cada coisa com o seu botão: autorizar, retomar, dar o segredo a um container gerenciado. Depois, se o PC pode ser desligado, tarefas agora, a última tarefa, a fila, as últimas tarefas com os logs e os recursos do computador. Recarrega a cada 10 segundos | nenhum |
| Tarefas e logs (`/local/logs`) | As últimas 100 tarefas: status, imagem, início no horário local, duração, código de saída, e o que cada uma imprimiu, para abrir ou baixar | nenhum |
| A tela de uma tarefa (`/local/screen/ID_DA_TAREFA`) | Com `screen_view` ligado: o que uma tarefa rodando mostra na tela, ao vivo e só para ver, quando a imagem serve noVNC na porta 6080. As listas de tarefas e os containers gerenciados têm o link (**Ver tela**) enquanto ela responde | nenhum |
| Segurança (`/local/safety`) | Se este computador está saudável, em alerta ou bloqueado, e por quê. Onde o dono lê os riscos e autoriza, ou retira uma autorização | nenhum: a decisão é do dono |
| Configuração (`/local/config`) | URL de poll, intervalo de poll, tarefas ao mesmo tempo, nome da máquina | nenhum |
| Configuração avançada (`/local/advanced`) | Imagens permitidas, qualquer imagem, limites de tempo, os limites de recursos, os dois tokens, o login de admin | `admin` / `admin` até ser trocado |
| Containers gerenciados (`/local/managed`) | [Containers gerenciados](#containers-gerenciados): a lista com estado e última execução; Iniciar, Parar, Ativar, Desativar | nenhum para os botões; o mesmo login para o editor, remover e o modo |
| Certificado (`/local/certificate`) | [Repasse de certificado](#repasse-de-certificado-opcional), opcional: importar um `.pfx` e a senha, validar, testar assinatura de PDF, desligar. A senha nunca é mostrada | o mesmo login, e uma confirmação |

- A página edita o mesmo `config.json` que o CLI, pela mesma validação. Uma mudança salva reinicia o serviço com o arquivo novo; os containers em execução não são tocados. Um valor inválido é recusado e nada é gravado.
- Troque a senha de admin com `shutup-and-run config set admin_password "NOVA" --apply`.
- Os limites de recursos (`resource_profile`, `max_job_cpus`, `max_job_memory`, `min_free_memory`, `min_free_disk`) ficam em Configuração avançada. Campo vazio usa o padrão do perfil, mostrado em cinza. `shutup-and-run config show` imprime os mesmos valores em "in force", e em `effective` com `--json`.
- As demais chaves são só pelo CLI. `shutup-and-run config keys` diz onde cada chave pode ser editada.
- Ela fica em inglês ou em português, um idioma por vez; um link troca e a escolha é lembrada.

**Ver a tela de uma tarefa.** Uma imagem que roda um navegador numa tela virtual pode servi-la com noVNC (por exemplo `ghcr.io/nailtongomes/rpa-worker-selenium` com `USE_XVFB=1`, `USE_VNC=1` e `USE_NOVNC=1`: noVNC na porta 6080, sem senha). Com `shutup-and-run config set screen_view true --apply` (ou Configuração avançada, com o login de administrador), a página abre o noVNC através dela mesma, conectado e só para ver, enquanto a tarefa roda. O endereço é o do container da tarefa na bridge do Docker, dado pelo Docker; uma requisição não escolhe porta nem outro container, e nada muda para a tarefa: nenhuma porta, nenhum campo de rede. A pessoa no PC vê o que a tarefa mostrar. O noVNC em si não tem senha, então outro container na mesma bridge também chega nessa porta. No Windows, o `127.0.0.1:8787` da página é o encaminhamento de uma porta TCP pelo WSL2, que leva o WebSocket como qualquer outra conexão. O `doctor` diz `SCREEN_VIEW_NO_PAGE` quando a chave está ligada e a página não. `tests/screen/linux.sh` é o teste à mão.

A segurança da página, sem rodeios. Ela não pede token, então só responde a uma conexão de loopback que nomeia um host de loopback, e só aceita um formulário da própria origem. Nunca mostra token nem env de job; mostra o que os jobs imprimiram (os logs, abaixo). O login de admin afasta um curioso e **não** é uma barreira de segurança: quem usa o PC consegue ler o arquivo de configuração. Nada nela é acessível pela rede, e ela não foi feita para ficar atrás de um proxy. Numa máquina configurada com `config.toml` a página só mostra o status.


### Saída dos jobs

Cada job que rodou um container deixa na máquina o que imprimiu: os últimos 256 KiB de stdout e stderr, um arquivo por job em `logs/` ao lado do histórico (`/var/lib/shutup-and-run/logs/`, dentro da distribuição WSL no Windows), apagados depois de 7 dias. Vale para push, pull e gerenciado, e para qualquer jeito de terminar. Nada é mascarado. `shutup-and-run jobs logs ID` (ou `--last`, `--tail N`, `--json`) mostra um, `jobs --json` diz quais jobs têm log, e a página local lista os últimos em `/local/logs` para abrir ou baixar. Um job ainda rodando não tem arquivo: `jobs logs ID` e o link **até agora** da página leem do Docker o que ele imprimiu até o momento. O próprio Docker guarda no máximo 10 MB da saída de um container (dois arquivos de 5 MB), então um job ou um container gerenciado que imprime por dias não enche o disco. Quando o último job falha, o `doctor` mostra o comando do log dele.

Cada job também registra o que usou: o tempo de CPU, o máximo de memória que ocupou, os bytes que recebeu e enviou pela rede e o que gravou dentro do container (`cpu_ms`, `memory_peak_bytes`, `network_bytes` e `disk_bytes` no `jobs --json`, uma linha `used:` no `jobs`, uma coluna na página). O pull da imagem não entra em `network_bytes`. Enquanto a imagem baixa, o `jobs` diz `downloading the image: 120 MiB of 450 MiB` no lugar de `used:` (`pull` no `jobs --json`, a mesma coluna na página, `pulling` no `status.json`); o segundo número é o que o Docker já nomeou das camadas, então pode crescer. Os números vêm da leitura feita a cada 30 segundos enquanto ele roda, e já aparecem durante a execução (`so far`), então um job mais curto que isso pode não ter nenhum, e um pico curto pode escapar. O disco é lido uma vez, quando o container é removido. `shutup-and-run jobs --summary` resume os últimos 20: o maior pico de memória e a média, e quantos processadores ocuparam, e depois o mesmo por imagem, com a rede e o disco por job e o `memory` que uma requisição dessa imagem pode pedir (o dobro do maior pico, a partir de três tarefas medidas). Com cinco jobs medidos ou mais e espaço na máquina, ele sugere um `max_job_memory` (o dobro do maior pico, arredondado para cima em 256 MiB) e o `max_slots` que caberia nesse tamanho, e o `doctor` diz o mesmo como `ROOM_FOR_MORE_SLOTS`. Nenhum dos dois muda um limite.

**Quando falta memória.** A página e o `doctor` (`MEMORY_TO_FREE`) dizem quanto liberar, mais ou menos, antes de a próxima tarefa começar, e onde: no PC ou na VM do WSL2 onde as tarefas rodam. No Windows eles também mostram os programas que mais ocupam a memória do PC, somados por programa; processos do próprio Windows, o antivírus, o WSL e o worker ficam de fora, e nada é fechado pela pessoa. Essa lista só é lida enquanto a memória do PC está apertada e fica no PC: não vai para o `status.json`, um log, o `doctor --report`, um callback nem o corpo do poll. O worker também conta quanto tempo as tarefas novas esperaram por memória, hoje e em 7 dias, e guarda a menor memória livre de cada hora da semana; quando ela ficou curta dentro do horário em que ele é emprestado, o `doctor` sugere um `lend_hours` que evita essas horas (`MEMORY_SHORT_HOURS`), depois de três dias de horas com leitura. Quando a última tarefa num PC Windows termina, a VM do WSL2 descarta o cache que ela encheu, e o Windows recebe essa memória de volta na hora; `wsl limits --apply` também põe `autoMemoryReclaim=dropcache` no `.wslconfig` (`WSL_MEMORY_NOT_RECLAIMED` enquanto falta). Veja [docs/FAQ.pt-BR.md](docs/FAQ.pt-BR.md#o-que-fecho-quando-falta-memória).

## Containers gerenciados

Para trabalho recorrente que não precisa de orquestrador, como um RPA em Selenium que roda a cada quinze minutos, o worker pode iniciar containers sozinho, a partir de definições guardadas na máquina. Não é um segundo Compose ou Kubernetes: a definição é o mesmo pedido de um job (imagem, comando, env, CPU, memória, shm, tempo limite), mais quando rodar.

| Política | Quando roda |
| --- | --- |
| `once` | Só quando alguém aperta Iniciar (ou `managed start NOME`) |
| `interval` | De novo a cada `interval_secs` (60 ou mais), contado de início a início. Nunca dois ao mesmo tempo: se a execução ainda não acabou quando chega a hora, a próxima vem depois dela. Depois de o serviço ficar parado, uma execução, não uma por intervalo perdido. Uma execução que falhou não é repetida antes da hora |
| `always` | Iniciado de novo sempre que termina. A espera é de 30 s e dobra até 15 min enquanto as execuções durarem menos de um minuto; uma execução que durou um minuto zera a contagem. Sem `timeout_secs` recebe `max_timeout_secs`, e é iniciado de novo depois disso |

**Um modo de trabalho por vez.** Containers gerenciados só rodam no **modo gerenciado**. Nesse modo o worker não faz poll, e `POST /v1/jobs` responde `409` `{"error":"managed mode","code":"MANAGED_MODE"}`. Fora dele, nenhum container gerenciado inicia. O modo só muda quando nada está rodando ou iniciando; senão a troca é recusada (`MANAGED_BUSY`) e nada é interrompido. O modo sobrevive a reinícios.

```bash
sudo shutup-and-run managed apply examples/managed/rpa-selenium.json
printf '%s' "$API_TOKEN" | sudo shutup-and-run managed secret rpa-selenium API_TOKEN
printf '%s' "$CERTIFICATE_ID" | sudo shutup-and-run managed secret rpa-selenium CERTIFICATE_ID
sudo shutup-and-run managed mode on
sudo shutup-and-run managed list
```

Ou na página local: **Containers gerenciados** lista cada um com estado, política, última execução e os botões Iniciar, Parar, Ativar e Desativar; o editor e a troca de modo pedem o login de administrador.

- **Cada execução é um job comum.** Precisa de imagem na allowlist, ocupa um dos `max_slots`, recebe os limites de CPU e memória da máquina, obedece à pausa, às checagens de memória e disco livres, ao `min_idle_secs` e às checagens de segurança do dono, tem tempo limite e aparece em `shutup-and-run jobs`. Quando a máquina recusa, o início **espera** e é tentado de novo 30 segundos depois; isso nunca conta como falha do container.
- **Segredos.** Os valores de `secret_env` ficam no arquivo de histórico do worker (modo 600) e só chegam ao env do container. Nenhum comando, página, log ou arquivo de status os imprime. Em um arquivo, `"API_TOKEN": null` significa "o valor que esta máquina já tem", então a definição pode ficar em um repositório. Um container com segredo sem valor não inicia.
- **Parar e desativar.** Parar encerra o container agora, e ele fica parado até alguém iniciar. Desativar não inicia mais nada e deixa a execução em andamento terminar.
- **Reinícios.** O container em execução é reencontrado pela linha do job, então um worker reiniciado não inicia um segundo. Um container que terminou com o worker ou o Docker parados fica registrado como `interrupted`, e a política decide o que vem depois.
- A definição não monta pastas, não publica portas e não roda privilegiada, como um job. Uma edição vale para a próxima execução.

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

## Emprestar a internet (alpha)

Um PC pode emprestar a **conexão de internet**, ao lado do worker ou sozinho quando é fraco demais para containers ou não tem WSL: os containers do operador num datacenter saem pela conexão de casa do cliente, e o site vê o IP residencial do cliente. O PC abre a conexão até um relay do operador (nenhuma porta aberta no PC ou no roteador, funciona atrás de CGNAT); o relay é um proxy HTTP para `requests`, `curl` e Chrome. Sem Docker, sem WSL: só o `.exe`.

```
shutup-and-run fit                                           # o que este PC pode emprestar: os dois, ou só a internet
shutup-and-run internet on --relay relay.example.com:7000 --login ID:SENHA --wait 60   # no PC: começa agora e a cada logon
shutup-and-run internet credentials                          # endereço do proxy e senha, para o operador
shutup-and-run internet status                               # conectado ou por que não, bytes; http://127.0.0.1:8788/
shutup-and-run relay                                         # no servidor do operador (examples/relay)
```

Novas conexões esperam enquanto o worker está pausado, fora de `--hours`, na bateria abaixo de 60%, enquanto a pessoa apresenta e em rede limitada; uma conexão já aberta nunca é cortada. Destinos na própria rede do PC são recusados. Nesta versão a ligação até o relay é TCP sem criptografia. A escolha, os riscos, um teste a partir de outra rede e os próximos passos: [docs/INTERNET.md](docs/INTERNET.md). Um celular também empresta o Wi-Fi, ou os dados móveis se a pessoa permitir, pelo Termux e sem root: [Android](docs/INTERNET.md#android-a-phone-in-termux).

Uma tarefa enviada a um worker seu em outro lugar sai pelo PC de um cliente pelo `env`: `HTTPS_PROXY=http://ID:SENHA@...` (`shutup-and-run internet credentials --env`). No Windows a linha de instalação faz tudo: `$env:SHUTUP_AND_RUN_RELAY = "relay.exemplo.com:7000"` (e `$env:SHUTUP_AND_RUN_POLL_URL` para o worker também) antes de `irm .../install.ps1 | iex`; o `install.sh` aceita o mesmo. A pessoa no PC pausa ou desliga pelos botões em http://127.0.0.1:8788/, ou com `shutup-and-run internet pause --for 1h` e `internet off`. Os passos de um agente: [docs/FOR-AGENTS.md](docs/FOR-AGENTS.md#a-task-from-the-operator-run-it-here-lend-the-internet-or-both). Seu servidor (o relay), passo a passo: [docs/SERVIDOR.pt-BR.md](docs/SERVIDOR.pt-BR.md); em uma linha, `docker run -d --restart unless-stopped -p 7000:7000 -p 172.17.0.1:8080:8080 -e SHUTUP_AND_RUN_RELAY_TOKEN=TOKEN ghcr.io/nailtongomes/shutup-and-run-relay`, ou sem Docker `curl -fsSL .../install.sh | sudo SHUTUP_AND_RUN_RELAY_SERVER=1 bash`. A sua URL de poll pode dizer a cada PC onde está o relay (`SHUTUP_AND_RUN_LEND_INTERNET=1` na instalação; [contrato](docs/INTEGRATION-CONTRACT.md#internet-discovery)). Como usar o proxy com curl, requests, Selenium, Playwright, Puppeteer, Node ou n8n: [exemplos](docs/INTERNET.md#examples-curl-requests-selenium-playwright-puppeteer-node-n8n).

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
shutup-and-run doctor [--report]                # toda linha com falha tem um hint
shutup-and-run pause [--for 2h|day | --until 08:00] | resume   # volta a aceitar sozinho no fim
shutup-and-run jobs [--limit N]                 # as últimas tarefas: status, exit code, duração, motivo da falha
```

Todos são não interativos e aceitam `--json`. Toda falha, e toda linha do doctor que não está ok, traz um `hint` com o próximo passo, em inglês ou português (`config set language pt`; a página segue a mesma chave). O `status` diz quando o worker perguntou por trabalho pela última vez (`last_poll_at`, `last_poll_status`). Os valores da query da poll URL aparecem como `***` (`config get poll_url --reveal` mostra). Códigos de saída: `0` feito, `1` falhou ou não está saudável, `2` linha de comando errada, `3` configuração ou valor inválido (nada foi gravado). Um valor inválido nunca é salvo, e um arquivo danificado de outro jeito não derruba o serviço: ele roda com a cópia do último salvamento, ou com os padrões ociosos, e avisa.

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
| `min_free_disk` | `SHUTUP_AND_RUN_MIN_FREE_DISK` | O mesmo para o disco em que o Docker grava: o `data-root` do `/etc/docker/daemon.json`, senão `/var/lib/docker`. |
| `min_idle_secs` | `SHUTUP_AND_RUN_MIN_IDLE_SECS` | Só Windows: um job novo espera até o PC ficar estes segundos sem uso. `0` (o padrão): desligado. |
| `lend_hours` | `SHUTUP_AND_RUN_LEND_HOURS` | Quando a máquina é emprestada, no relógio dela (no Windows, o relógio do Windows que a pessoa vê): `"19-07"`, `"mon-fri 19-07; sat,sun"`, `"mon-fri 12:00-13:30"`. Uma janela noturna pertence ao dia em que começa. Fora dela nenhum job novo começa e o worker não faz poll; a página, o `status` e o `status.json` (`lending.resumes_at`) dizem quando abre a próxima janela. Sem a chave: a qualquer hora. |
| `lend_on_metered` | `SHUTUP_AND_RUN_LEND_ON_METERED` | Só Windows. `false` (o padrão): numa rede limitada (o roteador do celular, um plano móvel, uma conexão marcada como limitada) nenhum job novo começa e o worker não faz poll. `true`: quem usa o PC aceita que os jobs gastem esses dados. |
| `default_timeout_secs` | `SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS` | Padrão 3600. |
| `max_timeout_secs` | `SHUTUP_AND_RUN_MAX_TIMEOUT_SECS` | Padrão 21600. No máximo 7 dias. |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Opcional. Liga o modo pull. |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Segundos entre polls. Padrão 30, mínimo 30, no máximo 3600. Um valor menor é recusado (saída 3). Um que ficou numa config de versão antiga ainda carrega e roda como 30, com aviso no `doctor`. |
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
| `prune_interval_secs` | `SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS` | Remove containers terminados e imagens sem uso nesse intervalo. Padrão 0, desligado. Fora isso, as imagens que nenhuma tarefa usou em 30 dias são removidas uma vez por dia, sempre. |
| `reaper_interval_secs`, `purge_interval_secs` | `SHUTUP_AND_RUN_REAPER_INTERVAL_SECS`, `_PURGE_INTERVAL_SECS` | De quanto em quanto os timeouts são aplicados (2) e o histórico antigo é apagado (3600). |
| `docker_config` | `SHUTUP_AND_RUN_DOCKER_CONFIG` | Caminho do `config.json` do Docker, para imagens privadas. |
| `certificate_enabled`, `certificate_consent`, `certificate_name` (só JSON) | | Repasse de certificado. Escritas por `shutup-and-run certificate set`, não à mão. Padrão desligado. |
| `screen_view` | | A página mostra a tela de uma tarefa rodando quando a imagem serve noVNC na porta 6080. Padrão desligado. Sem variável de ambiente: liga-se na máquina. |

**Allowlist.** `ghcr.io/you/worker` permite qualquer tag ou digest desse repositório e nada mais: nem `ghcr.io/you/worker-evil`, nem `ghcr.io/you/other`. Uma entrada com tag permite exatamente aquele texto. Um worker Windows usa `poll_any_image` no lugar: a única fonte de trabalho dele é a sua URL de poll.

**Imagens privadas.** `sudo docker login ghcr.io -u VOCE` na máquina. Uma tarefa não pode fornecer credenciais de registry.

**Logs.** Linux: `journalctl -u shutup-and-run -f`. Windows: `shutup-and-run wsl logs`. O token e o ambiente dos jobs nunca vão para o log.

## Para um agente de IA

[docs/FOR-AGENTS.md](docs/FOR-AGENTS.md) é o guia: instalação, os problemas conhecidos e as correções. O que o doctor responde numa máquina com `language` em `pt` cuja última tarefa falhou (no Windows, as mesmas linhas vêm depois das checagens do WSL e do Docker). As dicas saem no idioma configurado; o detalhe de cada linha fica em inglês:

```text
$ shutup-and-run doctor
ok   config                 /etc/shutup-and-run/config.json
ok   docker                 answers
ok   service                running (active), listening on 127.0.0.1:8787
ok   resources              profile shared-pc; per job: cpu 1, memory 2.0 GiB; a new job needs free memory 2.0 GiB, free disk 5.0 GiB; free now: memory 15.1 GiB, disk 5.2 GiB
ok   safety                 healthy: nothing in the way of a new job
--   last-job               job c914c6e4bfec26c1e58959f054a3d01c (alpine): the container exited with code 3
                            hint: Leia o que ele imprimiu: `shutup-and-run jobs logs c914c6e4bfec26c1e58959f054a3d01c`.
ok   poll                   https://queue.example/poll?user=*** answers the service
the worker is ready

$ shutup-and-run jobs logs c914 --tail 2
abrindo o portal
Traceback: timeout waiting for login
```

O ciclo inteiro num PC Windows, sem nenhuma pergunta e sem editar nada à mão:

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

Só `image` é obrigatório. Campos desconhecidos são recusados. `job_key` torna um retry seguro naquela máquina: a mesma chave devolve o mesmo job e não inicia nada. `reason` no resultado é `exited`, `timeout_kill`, `start_failed` ou `interrupted` (a máquina, o WSL, o Docker ou o worker parou com o job em andamento; ele não roda de novo). Não existe chamada de cancelamento.

**Pull.** Com `poll_url` definida, a máquina faz POST nela a cada `poll_interval_secs` (30 segundos, e nunca menos), só quando tem um slot livre, não está pausada e não está com pouca memória ou disco:

```json
{"worker": "DESK-07", "os": "windows", "arch": "x86_64", "version": "0.2.18", "max_slots": 1, "free_slots": 1, "cpus": 4, "memory_bytes": 3435973836}
```

A resposta é nada (`204`), uma tarefa, `{"task": {...}}`, ou `{"manifest_url": "https://..."}`, que é buscado uma vez e precisa ser uma tarefa. `401` ou `403` quer dizer "ainda não aprovada": o worker não inicia nada e continua perguntando, espaçando até cinco minutos. O servidor decide quais máquinas recebem trabalho; o token do worker é a identidade dele.

Limites de cada campo, todos os códigos de status, o callback, idempotência, e o que não é garantido: [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md).

**`status.json`** tem os mesmos fatos da página local, para um script que não deve chamar HTTP: `state` (`idle`, `active`, `draining`, `failed`, `stopped`), `slots_total`, `slots_used`, `job_ids`, `pulling` (as tarefas ainda baixando a imagem: `id`, `done_bytes`, `total_bytes`; até 30 segundos de atraso), `last_job_at`, `last_error`, `poll_error`, `waiting_for`, `paused_until` (o fim de uma pausa por um tempo), `cleanup` (a última passada diária pelas imagens que nenhuma tarefa usou em 30 dias: `at`, `images`, `freed_bytes`), `memory` (`short_of`, `to_free_bytes`, `waiting_since`, `waited_today_secs`, `waited_week_secs`, `short_hours`, `suggested_lend_hours`; nunca nomes de programas), `lending` (`lend_hours`, a hora no relógio dela, a espera que houver agora, `resumes_at` quando abre a próxima janela de `lend_hours`, `battery_percent` enquanto na bateria, `metered_network` no Windows), `callbacks` (jobs interrompidos, callbacks pendentes e desistidos), `limits`, `config_warning`, `version`, `pid`, `started_at`, `updated_at`. Linux: ao lado do arquivo de histórico, `/var/lib/shutup-and-run/status.json`, só root. Windows: `C:\ProgramData\shutup-and-run\status.json`, legível pelo usuário. Se `updated_at` tem mais de 60 segundos, o worker morreu. `shutup-and-run status --json` lê para você; `shutup-and-run status` mostra o mesmo em poucas linhas, cada uma só quando tem algo a dizer (um download, uma pausa com fim, `lend_hours`, uma espera, a bateria, memória a liberar, a última limpeza).

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
| `wsl update` | Depois de um `.exe` novo: o binário Linux da mesma versão, e o worker reiniciado. O `install.ps1` roda isso. [Atualizar e corrigir](docs/WINDOWS.md#update-and-repair). |
| `wsl doctor`, `doctor` | Confere tudo, com a correção de cada problema. |
| `wsl limits [--apply]` | Quanto do PC o WSL2 pode usar. |
| `wsl start`, `wsl stop`, `wsl logs` | A tarefa de logon e o log do supervisor. |
| `wsl reset [--factory]` | Recria uma distribuição quebrada. |
| `wsl uninstall [--purge]` | Remove a tarefa e a distribuição, e o `.wslconfig` que a instalação escreveu quando ninguém o mudou e não sobrou outra distribuição. |

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
- Timeout é SIGTERM, e SIGKILL 10 segundos depois; o `exit_code` do job é 137 nos dois casos.
- `job_key` vale por máquina. Outra máquina não a conhece.
- Sem cancelamento, sem fila, sem retry de job que falhou. Um container gerenciado não tem expressão cron, dependências nem captura de saída; um `always` que continua falhando é tentado a cada 15 minutos até alguém parar ou desativar.
- Modo gerenciado e push/pull são exclusivos em uma máquina. Para os dois, use duas máquinas.
- As definições gerenciadas, seus segredos e o modo ficam no arquivo de histórico dos jobs. No Windows, `shutup-and-run wsl reset` reconstrói a distribuição e esse arquivo junto: guarde os arquivos de definição (`managed show NOME` imprime um) e aplique de novo.
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

As imagens da página local neste README saem de `bash scripts/screenshots.sh`: um worker de demonstração com histórico próprio, três tarefas no Docker e o Playwright nos dois idiomas, claro, escuro e no celular. Rode de novo depois de mudar a página.

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

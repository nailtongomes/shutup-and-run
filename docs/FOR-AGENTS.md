# For agents

You are an AI agent setting up or repairing a shutup-and-run worker on someone's machine. Everything here is non-interactive. No command asks a question, and you never edit a file by hand.

When the person asks what to expect (a slow PC, a battery, a shutdown, a job that did not start, whether a job is paused), answer from the [FAQ](FAQ.md) ([Português](FAQ.pt-BR.md)).

## Rules of the CLI

- Add `--json` to `config`, `status`, `doctor`, `jobs`, `pause`, `resume`, `fit` and `internet`: one JSON object on stdout, `"ok": true|false`, and `"error"`, `"code"` and `"hint"` when false.
- `hint` is the next step: short, one concrete action, with the command when there is one. Run it if you have a shell; read it to the person if you are guiding one. Without `--json` it is the `hint:` line. Its language is the config key `language`: `shutup-and-run config set language pt` for Portuguese, `en` (the default) for English; `shutup-and-run wsl install --language pt` sets it at install. The local page opens in that language too, until the person presses its language button. The [catalogue](#codes-and-hints) lists every code.
- Exit codes: `0` done. `1` it failed, or the doctor found a problem. `2` wrong command line. `3` invalid config or value; nothing was written.
- `shutup-and-run config --help` documents every command. `shutup-and-run config keys` lists every key.
- The config is one file, `config.json` (schema: [schemas/worker-config.schema.json](schemas/worker-config.schema.json)). On Windows it is inside the WSL distribution; the same commands reach it. Change it only with `shutup-and-run config`.
- Secrets (`token`, `poll_token`, `admin_password`) print as `***`, and so do the values of the poll URL's query (`?cpf=***`). Do not put them in a report or an issue.

## Windows 11: install

PowerShell, as the logged-on user. No administrator is needed if WSL2 is already there.

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
shutup-and-run wsl install --poll-url "https://QUEUE/poll?user=NAME" --max-slots 1
```

The first line downloads `shutup-and-run.exe`, checks its SHA-256 and puts it on the user's PATH; open a new terminal if `shutup-and-run` is not found. The second takes a few minutes, ends with the doctor, and exits 0 only when the worker runs. It is safe to run again: a re-install keeps the config and changes only the options you pass. It also puts a `shutup-and-run` shortcut on the desktop that opens the local page.

If it says WSL is missing: `wsl --install --no-distribution` as administrator, reboot, run the install again.

To update, run the first line again: it stops the worker, swaps the `.exe`, and runs `shutup-and-run wsl update`, which puts the Linux binary of the same version in the distribution and keeps the config, token and history. On a failure it puts the previous version back. The doctor's `version` line (`VERSION_MISMATCH`) says when the two halves differ. A failed update, the files it can leave, and a recovery a person can follow one command at a time: [WINDOWS.md, Update and repair](WINDOWS.md#update-and-repair).

## Linux: install

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash
```

The Linux installer still writes `config.toml` and an env file, and on such a machine `shutup-and-run config` refuses and says so; `status`, `doctor`, `pause`, `resume` and `bans` work. A machine with `/etc/shutup-and-run/config.json` and no `config.toml` uses the JSON file and every command here. Run them as root.

## Set the poll URL

The poll URL is the setting that changes most. It may carry a query that pairs this machine with a person or an account: a tax id, an e-mail, a key the service gave them. Quote the URL, because of the `&`.

```
shutup-and-run config set poll_url "https://QUEUE/poll?cpf=00000000000&email=nobody@example.com" --apply --json
shutup-and-run config get poll_url            # https://QUEUE/poll?cpf=***&email=***
shutup-and-run config get poll_url --reveal   # the whole URL
```

The query is only a filter or a pairing. It does not approve the machine: the server approves the machine's poll token, and until it does the worker gets no task whatever the query says (`POLL_NOT_APPROVED`). The same URL works in `shutup-and-run wsl install --poll-url "..."` and in the Settings form of the local page. Its values may identify a person, so they print as `***` in `config show`, `status`, `doctor --report`, the page and the logs. Read them with `--reveal` only when the person asks, and do not paste them into an issue.

`--apply` restarts the service on the new file and waits until it is back. Running containers are not stopped. Other everyday keys: `max_slots`, `poll_interval_secs`, `worker_name`. `poll_interval_secs` is 30 by default and cannot be set lower: 1 to 29 exits 3 and nothing is written. An invalid value exits 3 with the reason and leaves the file as it was.

## Quick start: prove the machine works

After the install, and before any orchestrator sends work:

```
shutup-and-run version --json                 # which build, platform and runtime you are talking to
shutup-and-run config check --json            # exit 0 ready; 3 the config is invalid; 1 something around it
shutup-and-run demo --json                    # one container under the worker's rules; exit 0 when it passed
shutup-and-run job validate task.json --json  # exit 3 and a code when this machine would refuse the task
```

Each prints `ok`, a top-level `code`, and `checks[]` with `name`, `code`, `status`, `detail`, `hint`. Act on `code`, show `detail` to a person, and do what `hint` says: it is the next step, with the command for this machine, in the configured language. None of these changes the config or the job history. `demo` runs `hello-world`; with another image use `--image`, and if the answer is `DEMO_IMAGE_NOT_ALLOWED` its `hint` is the command that allows it. `job validate` is the static half of admission only: a `JOB_VALID` task can still meet a full or paused machine later.

## Codes and hints

`doctor --json`, `config check --json` and `demo --json` give every check a `code` that does not change between releases, and every CLI failure has one too. A check that passed is `NAME_OK` and has no hint. One that did not, and has no code of its own, is `NAME_WARNING` or `NAME_FAILED`.

Each code has one `hint`: the next step, in the configured language. `{}` is filled with what this machine needs: an address, an image, a number. This is the whole catalogue as the CLI prints it (`src/hints.rs`; a test keeps this table complete). A code that is not here gets the last line.

The JSON also carries `fix`. It is the same sentence under the name 0.2.8 used; read `hint`.

| Code | `hint` (en) | `hint` (pt) |
| --- | --- | --- |
| `USAGE` | Wrong command line. Run `shutup-and-run --help` and copy the command from there. | Linha de comando errada. Rode `shutup-and-run --help` e copie o comando de lá. |
| `INVALID_VALUE` | Nothing was written. Run `shutup-and-run config keys`, then set the key again with a valid value. | Nada foi gravado. Rode `shutup-and-run config keys` e defina a chave de novo com um valor válido. |
| `FAILED` | Run `shutup-and-run doctor` and follow the hint of each failed line. | Rode `shutup-and-run doctor` e siga a dica de cada linha que falhou. |
| `NOT_INSTALLED` | Run `shutup-and-run wsl install` in an administrator terminal. | Rode `shutup-and-run wsl install` em um terminal de administrador. |
| `CONFIG_INVALID` | Run `shutup-and-run config validate` to see the bad value; fix it with `shutup-and-run config set KEY VALUE`, or `shutup-and-run config reset`. | Rode `shutup-and-run config validate` para ver o valor errado; corrija com `shutup-and-run config set CHAVE VALOR`, ou `shutup-and-run config reset`. |
| `CONFIG_FAILED` | Run `shutup-and-run config validate`; if the file cannot be repaired, `shutup-and-run config reset`. | Rode `shutup-and-run config validate`; se o arquivo não tiver conserto, `shutup-and-run config reset`. |
| `CONFIG_FALLBACK_IN_USE` | The config file was damaged and a fallback runs. Run `shutup-and-run config reset`, then set the poll URL again. | O arquivo de config estava danificado e uma cópia reserva está em uso. Rode `shutup-and-run config reset` e defina a poll URL de novo. |
| `POLL_INTERVAL_RAISED` | Run `shutup-and-run config set poll_interval_secs 30 --apply`. | Rode `shutup-and-run config set poll_interval_secs 30 --apply`. |
| `POLL_INTERVAL_TOO_LOW` | Use 30 or more: `--poll-interval-secs 30`, or leave the option out. | Use 30 ou mais: `--poll-interval-secs 30`, ou não passe a opção. |
| `STORAGE_NOT_WRITABLE` | Run the command as root (Linux: `sudo`), or point `history_path` at a writable folder with `shutup-and-run config set history_path PATH`. | Rode o comando como root (Linux: `sudo`), ou aponte `history_path` para uma pasta gravável com `shutup-and-run config set history_path CAMINHO`. |
| `ALLOWLIST_WARNING` | Add the image: `shutup-and-run config set allowlist IMAGE --apply`, or let the poll URL choose with `shutup-and-run config set poll_any_image true --apply`. | Adicione a imagem: `shutup-and-run config set allowlist IMAGEM --apply`, ou deixe a poll URL escolher com `shutup-and-run config set poll_any_image true --apply`. |
| `ALLOWLIST_FAILED` | Run `shutup-and-run config set allowlist IMAGE --apply` with at least one image. | Rode `shutup-and-run config set allowlist IMAGEM --apply` com pelo menos uma imagem. |
| `DOCKER_UNAVAILABLE` | Start Docker. Windows: `shutup-and-run wsl stop` then `shutup-and-run wsl start`. Linux: `sudo systemctl start docker`. | Inicie o Docker. Windows: `shutup-and-run wsl stop` e depois `shutup-and-run wsl start`. Linux: `sudo systemctl start docker`. |
| `DOCKER_FAILED` | Windows: `shutup-and-run wsl stop` then `shutup-and-run wsl start`; still failing, `shutup-and-run wsl reset`. | Windows: `shutup-and-run wsl stop` e depois `shutup-and-run wsl start`; se continuar, `shutup-and-run wsl reset`. |
| `SERVICE_PORT_IN_USE` | Pick another port: `shutup-and-run config set listen 127.0.0.1:8788`, then start the service again. | Escolha outra porta: `shutup-and-run config set listen 127.0.0.1:8788` e inicie o serviço de novo. |
| `SERVICE_FAILED` | Follow the docker line first if it failed, then start the service: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. | Siga primeiro a linha do docker, se ela falhou, e inicie o serviço: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. |
| `SERVICE_NOT_RUNNING` | Start the service: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. | Inicie o serviço: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. |
| `WORKER_PAUSED` | Run `shutup-and-run resume`. | Rode `shutup-and-run resume`. |
| `LOCAL_ADDRESS_BANNED` | Run `shutup-and-run bans lift {}`, then check the token of whoever calls this machine. | Rode `shutup-and-run bans lift {}` e confira o token de quem chama esta máquina. |
| `ADDRESS_BANNED` | Only if one of the addresses is yours: `shutup-and-run bans lift IP`. | Só se um dos endereços for seu: `shutup-and-run bans lift IP`. |
| `RESOURCES_FAILED` | Free memory or disk on this machine, or lower the need: `shutup-and-run config set max_slots 1 --apply`. | Libere memória ou disco nesta máquina, ou reduza a exigência: `shutup-and-run config set max_slots 1 --apply`. |
| `RESOURCES_WARNING` | The worker waits until memory and disk are free again. Close heavy programs or free disk space. | O worker espera até haver memória e disco livres. Feche programas pesados ou libere espaço em disco. |
| `LOW_MEMORY` | It clears when programs close or a job ends. To ask for less: `shutup-and-run config set min_free_memory 1g --apply`. On Windows, `shutup-and-run wsl limits` shows what WSL2 has. | Passa quando programas fecham ou uma tarefa termina. Para exigir menos: `shutup-and-run config set min_free_memory 1g --apply`. No Windows, `shutup-and-run wsl limits` mostra quanto o WSL2 tem. |
| `LOW_DISK` | Free disk space: `docker image prune -af` (Windows: `wsl -d shutup-and-run -u root docker image prune -af`). To ask for less: `shutup-and-run config set min_free_disk 2g --apply`. | Libere espaço em disco: `docker image prune -af` (Windows: `wsl -d shutup-and-run -u root docker image prune -af`). Para exigir menos: `shutup-and-run config set min_free_disk 2g --apply`. |
| `USER_ACTIVE` | The PC is in use and `min_idle_secs` makes jobs wait; it clears by itself. To start jobs at any time: `shutup-and-run config set min_idle_secs 0 --apply`. | O PC está em uso e `min_idle_secs` faz as tarefas esperarem; passa sozinho. Para começar tarefas a qualquer hora: `shutup-and-run config set min_idle_secs 0 --apply`. |
| `OUTSIDE_LEND_HOURS` | It is outside `lend_hours`; jobs start again when the next window opens. To change it: `shutup-and-run config set lend_hours "mon-fri 19-07; sat,sun" --apply`, or `config unset lend_hours --apply` to lend at any time. | Está fora de `lend_hours`; as tarefas voltam quando a próxima janela abrir. Para mudar: `shutup-and-run config set lend_hours "mon-fri 19-07; sat,sun" --apply`, ou `config unset lend_hours --apply` para emprestar a qualquer hora. |
| `BATTERY_LOW` | The PC is on its battery below 60%; plug it in and jobs start again by themselves. Nothing changes this limit. | O PC está na bateria abaixo de 60%; ligue na tomada e as tarefas voltam sozinhas. Nada muda esse limite. |
| `ON_BATTERY` | Jobs still start; on the battery below 60% they wait. Plug the PC in to keep it from draining. | As tarefas ainda começam; na bateria abaixo de 60% elas esperam. Ligue o PC na tomada para não descarregar. |
| `USER_PRESENTING` | The person at the PC is presenting or has a program in full screen; jobs start again by themselves when that ends. | Quem usa o PC está apresentando ou com um programa em tela cheia; as tarefas voltam sozinhas quando isso acabar. |
| `METERED_NETWORK` | The PC is on a metered network (a phone's hotspot or a mobile plan); jobs start again by themselves on another network. If the PC's owner agrees to spend that data: `shutup-and-run config set lend_on_metered true --apply`. | O PC está numa rede limitada (o roteador do celular ou um plano móvel); as tarefas voltam sozinhas em outra rede. Se o dono do PC aceitar gastar esses dados: `shutup-and-run config set lend_on_metered true --apply`. |
| `AUTHORIZATION_REQUIRED` | When the owner agreed in the contract, record it: `shutup-and-run safety authorize --owner-consent`; otherwise ask the person at this computer to open http://127.0.0.1:8787/ and authorize, or to run `shutup-and-run safety authorize`. | Se o dono concordou no contrato, registre: `shutup-and-run safety authorize --owner-consent`; senão, peça à pessoa neste computador para abrir http://127.0.0.1:8787/ e autorizar, ou rodar `shutup-and-run safety authorize`. |
| `SAFETY_BLOCKED` | It clears by itself when the computer recovers; no authorization lifts it. `shutup-and-run safety` shows which reading is critical. | Passa sozinho quando o computador se recupera; nenhuma autorização libera. `shutup-and-run safety` mostra qual leitura está crítica. |
| `SAFETY_TELEMETRY_MISSING` | The worker could not read memory, disk or load, so it starts nothing. Run `shutup-and-run doctor --report` and open the issue link. | O worker não conseguiu ler memória, disco ou carga, então não inicia nada. Rode `shutup-and-run doctor --report` e abra o link de issue. |
| `SAFETY_MEMORY_CRITICAL` | Close programs to free memory; jobs start again by themselves once it is back above the recovery line. On Windows, `shutup-and-run wsl limits` shows what WSL2 has. | Feche programas para liberar memória; as tarefas voltam sozinhas quando ela passar da linha de recuperação. No Windows, `shutup-and-run wsl limits` mostra o que o WSL2 tem. |
| `SAFETY_DISK_CRITICAL` | Free disk space until at least 3 GiB are free: `docker image prune -af` (Windows: `wsl -d shutup-and-run -u root docker image prune -af`), or delete files on the PC. | Libere espaço em disco até sobrar pelo menos 3 GiB: `docker image prune -af` (Windows: `wsl -d shutup-and-run -u root docker image prune -af`), ou apague arquivos no PC. |
| `SAFETY_CPU_CRITICAL` | The processor is saturated by other programs. Wait or close them; jobs start again by themselves when the load drops. | O processador está saturado por outros programas. Aguarde ou feche-os; as tarefas voltam sozinhas quando a carga cair. |
| `SAFETY_OOM_BREAKER` | Jobs ran out of memory three times in 30 minutes, so new ones pause for 15. Give the task more memory or a smaller workload at the service that sends it; see `shutup-and-run jobs`. | Tarefas estouraram a memória três vezes em 30 minutos, então as novas pausam por 15. Dê mais memória à tarefa ou reduza a carga no serviço que a envia; veja `shutup-and-run jobs`. |
| `SAFETY_SMALL_MACHINE` | This computer has under 8 GiB of memory or at most 2 processors, so its owner has to authorize jobs: see the hint of `AUTHORIZATION_REQUIRED`. | Este computador tem menos de 8 GiB de memória ou no máximo 2 processadores, então o dono precisa autorizar as tarefas: veja a dica de `AUTHORIZATION_REQUIRED`. |
| `SAFETY_MEMORY_LOW` | Less than 20% of the memory is available. Close programs, or the owner authorizes jobs under this condition: see the hint of `AUTHORIZATION_REQUIRED`. | Menos de 20% da memória está disponível. Feche programas, ou o dono autoriza tarefas nessa condição: veja a dica de `AUTHORIZATION_REQUIRED`. |
| `SAFETY_CPU_BUSY` | The processor is busy with other programs. It clears by itself, or the owner authorizes jobs under this condition: see the hint of `AUTHORIZATION_REQUIRED`. | O processador está ocupado com outros programas. Passa sozinho, ou o dono autoriza tarefas nessa condição: veja a dica de `AUTHORIZATION_REQUIRED`. |
| `SAFETY_HOST_UNKNOWN` | Windows is not telling the worker about the PC. Run `shutup-and-run wsl stop` then `shutup-and-run wsl start`; if it stays, update with the install line. | O Windows não está informando o worker sobre o PC. Rode `shutup-and-run wsl stop` e depois `shutup-and-run wsl start`; se continuar, atualize com a linha de instalação. |
| `WORKER_BUSY` | Every slot is in use. Wait for the running job (`shutup-and-run jobs`) and try again. | Todos os slots estão em uso. Aguarde a tarefa em execução (`shutup-and-run jobs`) e tente de novo. |
| `ROOM_FOR_MORE_SLOTS` | If the jobs to come are like these, set the max_job_memory and max_slots the line names with `shutup-and-run config set KEY VALUE`, then `shutup-and-run config apply`. | Se as próximas tarefas forem como estas, ajuste max_job_memory e max_slots com os valores da linha usando `shutup-and-run config set CHAVE VALOR`, depois `shutup-and-run config apply`. |
| `MEMORY_TO_FREE` | Ask the person at the PC to close what they do not need now (the local page lists what holds the most), or wait: jobs start again by themselves. | Peça a quem usa o PC para fechar o que não precisa agora (a página local lista o que mais ocupa), ou espere: as tarefas voltam sozinhas. |
| `MEMORY_HELD_BY_PROGRAMS` | Tell the person at the PC which of these they could close, in their words; never close one for them. | Diga a quem usa o PC quais destes dá para fechar, com as palavras dela; nunca feche um por ela. |
| `MEMORY_SHORT_HOURS` | To lend only when memory is free, if the owner agrees: `shutup-and-run config set lend_hours "{}" --apply`. | Para emprestar só quando há memória livre, se o dono concordar: `shutup-and-run config set lend_hours "{}" --apply`. |
| `CLOCK_SKEW` | The service sets the WSL2 clock from Windows' by itself every 10 s and could not: restart it with `shutup-and-run wsl stop` and `shutup-and-run wsl start`, and send the supervisor log if it stays. | O serviço acerta sozinho o relógio do WSL2 pelo do Windows a cada 10 s e não conseguiu: reinicie com `shutup-and-run wsl stop` e `shutup-and-run wsl start`, e mande o log do supervisor se continuar. |
| `WSL_DISK_TO_COMPACT` | With no job running and the PC's owner agreeing, follow "Give the WSL2 disk's space back" in docs/WINDOWS.md: WSL is stopped and an administrator compacts the disk file with diskpart. | Sem tarefa rodando e com o dono do PC de acordo, siga "Give the WSL2 disk's space back" no docs/WINDOWS.md: o WSL é parado e um administrador compacta o arquivo do disco com o diskpart. |
| `WSL_MEMORY_NOT_RECLAIMED` | Run `shutup-and-run wsl limits --apply`: it adds autoMemoryReclaim=dropcache, keeps every other line and a .wslconfig.bak, and takes effect when WSL restarts. | Rode `shutup-and-run wsl limits --apply`: ele acrescenta autoMemoryReclaim=dropcache, mantém as outras linhas e um .wslconfig.bak, e vale quando o WSL reiniciar. |
| `SLOTS_EXCEED_MEMORY` | Run `shutup-and-run config set max_slots {} --apply`, or lower `max_job_memory`. | Rode `shutup-and-run config set max_slots {} --apply`, ou reduza `max_job_memory`. |
| `POLL_URL_UNSET` | Run `shutup-and-run config set poll_url URL --apply` with the URL the service gave you. | Rode `shutup-and-run config set poll_url URL --apply` com a URL que o serviço informou. |
| `POLL_UNREACHABLE` | Check the internet and the address: `shutup-and-run config get poll_url --reveal`. On Windows, a URL on this PC or LAN is blocked by the firewall on a Public network (docs/FOR-AGENTS.md). | Confira a internet e o endereço: `shutup-and-run config get poll_url --reveal`. No Windows, uma URL neste PC ou na rede local é bloqueada pelo firewall em rede Pública (docs/FOR-AGENTS.md). |
| `POLL_NOT_APPROVED` | Nothing to do on this machine: ask the service to approve worker {}. It keeps asking by itself. | Nada a fazer nesta máquina: peça ao serviço para aprovar o worker {}. Ele continua tentando sozinho. |
| `POLL_HTTP_ERROR` | The poll URL answered with an error. Check the path and the query: `shutup-and-run config get poll_url --reveal`; fix with `shutup-and-run config set poll_url URL --apply`. | A poll URL respondeu com erro. Confira o caminho e os parâmetros: `shutup-and-run config get poll_url --reveal`; corrija com `shutup-and-run config set poll_url URL --apply`. |
| `POLL_RATE_LIMITED` | The poll URL asked this machine to slow down (429). Wait: the worker backs off by itself. | A poll URL pediu para esta máquina ir mais devagar (429). Aguarde: o worker reduz o ritmo sozinho. |
| `POLL_ERROR` | Check the URL and its query with `shutup-and-run config get poll_url --reveal`, then `shutup-and-run status`. | Confira a URL e seus parâmetros com `shutup-and-run config get poll_url --reveal` e depois `shutup-and-run status`. |
| `MANAGED_INVALID` | Nothing was saved. Fix what the error names; `shutup-and-run managed --help` lists the fields and examples/managed/ has a file to copy. | Nada foi salvo. Corrija o que o erro aponta; `shutup-and-run managed --help` lista os campos e examples/managed/ tem um arquivo para copiar. |
| `MANAGED_NOT_FOUND` | Run `shutup-and-run managed list` and use one of the names it prints. | Rode `shutup-and-run managed list` e use um dos nomes que ele imprime. |
| `MANAGED_BUSY` | Wait for the running containers (`shutup-and-run jobs`), or stop the managed ones with `shutup-and-run managed stop NAME`, then change the mode again. | Aguarde os containers em execução (`shutup-and-run jobs`), ou pare os gerenciados com `shutup-and-run managed stop NOME`, e mude o modo de novo. |
| `MANAGED_MODE` | This machine runs its own managed containers and takes no job. Send the job to another machine, or run `shutup-and-run managed mode off` on this one while it is idle. | Esta máquina roda seus próprios containers gerenciados e não aceita tarefas. Envie a tarefa para outra máquina, ou rode `shutup-and-run managed mode off` nesta enquanto estiver ociosa. |
| `MANAGED_MODE_OFF` | Run `shutup-and-run managed mode on` while nothing is running; the worker then stops taking jobs from outside. | Rode `shutup-and-run managed mode on` quando nada estiver rodando; o worker então para de pegar tarefas de fora. |
| `MANAGED_DISABLED` | Run `shutup-and-run managed enable NAME`, then start it again. | Rode `shutup-and-run managed enable NOME` e inicie de novo. |
| `MANAGED_RUNNING` | Run `shutup-and-run managed stop NAME`, wait a few seconds, then remove it again. | Rode `shutup-and-run managed stop NOME`, aguarde alguns segundos e remova de novo. |
| `MANAGED_FAILED` | Run the command as root (Linux: `sudo`), with the same config as the service; then `shutup-and-run doctor`. | Rode o comando como root (Linux: `sudo`), com a mesma config do serviço; depois `shutup-and-run doctor`. |
| `JOB_INVALID_JSON` | Fix the JSON of the job file and run `shutup-and-run job validate FILE` again. | Corrija o JSON do arquivo da tarefa e rode `shutup-and-run job validate ARQUIVO` de novo. |
| `JOB_INVALID_FIELD` | Fix the field named in the error; docs/INTEGRATION-CONTRACT.md lists the fields of a job. | Corrija o campo citado no erro; docs/INTEGRATION-CONTRACT.md lista os campos de uma tarefa. |
| `JOB_IMAGE_NOT_ALLOWED` | Add the image: `shutup-and-run config set allowlist IMAGE --apply`, or validate as a polled task with `--poll`. | Adicione a imagem: `shutup-and-run config set allowlist IMAGEM --apply`, ou valide como tarefa da poll URL com `--poll`. |
| `JOB_NOT_FOUND` | Run `shutup-and-run jobs` to see the ids of this machine's jobs. | Rode `shutup-and-run jobs` para ver os ids dos jobs desta máquina. |
| `JOB_ID_AMBIGUOUS` | Give more characters of the id; `shutup-and-run jobs` lists them. | Informe mais caracteres do id; `shutup-and-run jobs` lista todos. |
| `JOB_LOG_MISSING` | Run `shutup-and-run jobs` to see how that job ended: a job that never started has no log, and a log is kept for 7 days. | Rode `shutup-and-run jobs` para ver como esse job terminou: um job que não iniciou não tem log, e um log fica guardado por 7 dias. |
| `LAST_JOB_FAILED` | Read what it printed: `shutup-and-run jobs logs {}`. | Leia o que ele imprimiu: `shutup-and-run jobs logs {}`. |
| `LAST_JOB_FAILED_NO_LOG` | Run `shutup-and-run jobs` to see why it ended; it left no output to read. | Rode `shutup-and-run jobs` para ver por que ele terminou; ele não deixou saída para ler. |
| `DEMO_IMAGE_NOT_ALLOWED` | Name an allowed image with `--image`, or allow this one: `shutup-and-run config set allowlist {} --apply`. | Indique uma imagem permitida com `--image`, ou permita esta: `shutup-and-run config set allowlist {} --apply`. |
| `DEMO_JOB_INVALID` | Run `shutup-and-run config check` and fix what it reports. | Rode `shutup-and-run config check` e corrija o que ele apontar. |
| `JOB_FAILED` | Run `shutup-and-run config check` and fix what it reports. | Rode `shutup-and-run config check` e corrija o que ele apontar. |
| `ADMISSION_FAILED` | The machine is short of memory or disk, or paused. Run `shutup-and-run status` and free what it names. | A máquina está com pouca memória ou disco, ou pausada. Rode `shutup-and-run status` e libere o que ele citar. |
| `DEMO_TIMEOUT` | The demo container did not finish in time. Run `docker rm -f shutup-and-run-{}`, check the internet, then `shutup-and-run doctor`. | O container da demo não terminou a tempo. Rode `docker rm -f shutup-and-run-{}`, confira a internet e depois `shutup-and-run doctor`. |
| `CONTAINER_FAILED` | The image could not be downloaded or run. Check the internet of this machine (a restricted network may block the registry), then run the command again. | A imagem não pôde ser baixada ou executada. Confira a internet desta máquina (uma rede restrita pode bloquear o registro) e rode o comando de novo. |
| `DEMO_IMAGE_UNAVAILABLE` | The image has to be downloaded once. Check the internet, or run `docker pull {}`, then the demo again. | A imagem precisa ser baixada uma vez. Confira a internet, ou rode `docker pull {}`, e depois a demo de novo. |
| `IMAGE_FAILED` | The image could not be downloaded. Check the internet and run the demo again. | A imagem não pôde ser baixada. Confira a internet e rode a demo de novo. |
| `DEMO_JOB_FAILED` | The demo container exited with an error; its last lines are in `logs` (`--json`). Run `shutup-and-run doctor`, then the demo again. | O container da demo terminou com erro; as últimas linhas estão em `logs` (`--json`). Rode `shutup-and-run doctor` e depois a demo de novo. |
| `LIMITS_NOT_APPLIED` | Docker did not apply the limits. Update Docker, or on Windows run `shutup-and-run wsl reset`. | O Docker não aplicou os limites. Atualize o Docker ou, no Windows, rode `shutup-and-run wsl reset`. |
| `LIMITS_NOT_READ` | The limits could not be read back. Run the demo again; if it repeats, `shutup-and-run doctor --report`. | Não foi possível ler os limites. Rode a demo de novo; se repetir, `shutup-and-run doctor --report`. |
| `LIMITS_FAILED` | Docker did not apply the limits. Update Docker, or on Windows run `shutup-and-run wsl reset`. | O Docker não aplicou os limites. Atualize o Docker ou, no Windows, rode `shutup-and-run wsl reset`. |
| `DEMO_CLEANUP_FAILED` | A demo container was left behind. Run `docker rm -f shutup-and-run-{}`. | Um container da demo ficou para trás. Rode `docker rm -f shutup-and-run-{}`. |
| `CLEANUP_FAILED` | A demo container was left behind. Remove it: `docker rm -f` the container named in the detail. | Um container da demo ficou para trás. Remova: `docker rm -f` no container citado no detalhe. |
| `DEMO_WORKSPACE` | The demo could not create its temporary folder. Run it as root (Linux: `sudo`) and check free disk. | A demo não conseguiu criar sua pasta temporária. Rode como root (Linux: `sudo`) e confira o disco livre. |
| `WORKSPACE_FAILED` | The demo could not create its temporary folder. Run it as root (Linux: `sudo`) and check free disk. | A demo não conseguiu criar sua pasta temporária. Rode como root (Linux: `sudo`) e confira o disco livre. |
| `SIGNATURE_INVALID` | The signed PDF did not verify. Run `shutup-and-run certificate status` and the demo again. | O PDF assinado não foi validado. Rode `shutup-and-run certificate status` e a demo de novo. |
| `SIGNATURE_FAILED` | The signed PDF did not verify. Run `shutup-and-run certificate status` and the demo again. | O PDF assinado não foi validado. Rode `shutup-and-run certificate status` e a demo de novo. |
| `OUTPUT_NOT_WRITTEN` | Choose a folder you can write to with `--out FILE` and run the demo again. | Escolha uma pasta onde você pode gravar com `--out ARQUIVO` e rode a demo de novo. |
| `OUTPUT_FAILED` | Choose a folder you can write to with `--out FILE` and run the demo again. | Escolha uma pasta onde você pode gravar com `--out ARQUIVO` e rode a demo de novo. |
| `CONSENT_REQUIRED` | Read the risk in the error. To accept it, repeat the command with `--accept-risk`. | Leia o risco no erro. Para aceitar, repita o comando com `--accept-risk`. |
| `CERTIFICATE_MISSING` | The local certificate is optional. To use one: `shutup-and-run certificate set --file FILE.pfx --accept-risk`; to stop asking for it: `shutup-and-run certificate disable`. | O certificado local é opcional. Para usar um: `shutup-and-run certificate set --file ARQUIVO.pfx --accept-risk`; para não usar: `shutup-and-run certificate disable`. |
| `CERTIFICATE_INVALID` | The file is not a readable .pfx/.p12. Export the certificate again with its private key and repeat `shutup-and-run certificate set`. | O arquivo não é um .pfx/.p12 legível. Exporte o certificado de novo com a chave privada e repita `shutup-and-run certificate set`. |
| `CERTIFICATE_PASSWORD_WRONG` | Wrong certificate password. Repeat `shutup-and-run certificate set` with the right one. | Senha do certificado errada. Repita `shutup-and-run certificate set` com a senha certa. |
| `CERTIFICATE_NO_PRIVATE_KEY` | Export the certificate again including the private key (.pfx) and repeat `shutup-and-run certificate set`. | Exporte o certificado de novo incluindo a chave privada (.pfx) e repita `shutup-and-run certificate set`. |
| `CERTIFICATE_EXPIRED` | The certificate expired. Get a new one and run `shutup-and-run certificate set` with it, or `shutup-and-run certificate disable`. | O certificado venceu. Obtenha um novo e rode `shutup-and-run certificate set` com ele, ou `shutup-and-run certificate disable`. |
| `CERTIFICATE_FAILED` | Run `shutup-and-run certificate status`; replace the certificate with `shutup-and-run certificate set`, or `shutup-and-run certificate disable`. | Rode `shutup-and-run certificate status`; troque o certificado com `shutup-and-run certificate set`, ou `shutup-and-run certificate disable`. |
| `CERTIFICATE_WARNING` | Run `shutup-and-run certificate status`; replace the certificate with `shutup-and-run certificate set`, or `shutup-and-run certificate disable`. | Rode `shutup-and-run certificate status`; troque o certificado com `shutup-and-run certificate set`, ou `shutup-and-run certificate disable`. |
| `CERTIFICATE_MOUNT_WRITABLE` | The container could write to the certificate folder. Run `shutup-and-run doctor --report` and open the issue link. | O container conseguiu gravar na pasta do certificado. Rode `shutup-and-run doctor --report` e abra o link de issue. |
| `MOUNT_FAILED` | The certificate was not visible in the container. Run `shutup-and-run certificate status` and the demo again. | O certificado não apareceu no container. Rode `shutup-and-run certificate status` e a demo de novo. |
| `OPENSSL_MISSING` | Install openssl (Linux: `sudo apt-get install -y openssl`); on Windows run `shutup-and-run wsl reset`. | Instale o openssl (Linux: `sudo apt-get install -y openssl`); no Windows rode `shutup-and-run wsl reset`. |
| `DEMO_SIGN_FAILED` | The signing demo needs an image with python3 and openssl: leave `--image` out to use the default. The container's last lines are in `logs` (`--json`). | A demo de assinatura precisa de uma imagem com python3 e openssl: não passe `--image` para usar a padrão. As últimas linhas do container estão em `logs` (`--json`). |
| `WSL_UNAVAILABLE` | In an administrator PowerShell run `wsl --install --no-distribution`, restart the PC, then `shutup-and-run wsl install`. | Em um PowerShell de administrador rode `wsl --install --no-distribution`, reinicie o PC e depois `shutup-and-run wsl install`. |
| `WSL_FAILED` | In an administrator PowerShell run `wsl --update`, restart the PC, then `shutup-and-run doctor`. | Em um PowerShell de administrador rode `wsl --update`, reinicie o PC e depois `shutup-and-run doctor`. |
| `DISTRIBUTION_MISSING` | Run `shutup-and-run wsl install`. | Rode `shutup-and-run wsl install`. |
| `DISTRIBUTION_FAILED` | Run `shutup-and-run wsl reset` to rebuild the distribution; the config is kept. | Rode `shutup-and-run wsl reset` para reconstruir a distribuição; a config é mantida. |
| `BINARY_FAILED` | Run `shutup-and-run wsl install` again: it puts the right binary in the distribution. | Rode `shutup-and-run wsl install` de novo: ele coloca o binário certo na distribuição. |
| `TASK_NOT_REGISTERED` | Run `shutup-and-run wsl task register` so the worker starts at logon. | Rode `shutup-and-run wsl task register` para o worker iniciar no logon. |
| `VERSION_MISMATCH` | Run `shutup-and-run wsl update` to put the Linux binary of this .exe's version in the distribution. | Rode `shutup-and-run wsl update` para colocar na distribuição o binário Linux da mesma versão deste .exe. |
| `UPDATE_AVAILABLE` | In PowerShell, as the user who installed, run the install line again (`irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 \| iex`): it updates the .exe and the distribution. | No PowerShell, como o usuário que instalou, rode a linha de instalação de novo (`irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 \| iex`): ela atualiza o .exe e a distribuição. |
| `TASK_WARNING` | Run `shutup-and-run wsl task register` so the worker starts at logon. | Rode `shutup-and-run wsl task register` para o worker iniciar no logon. |
| `WSL_LIMITS_WARNING` | Run `shutup-and-run wsl limits` to see what WSL2 may take; `shutup-and-run wsl limits --apply` sets the budget. | Rode `shutup-and-run wsl limits` para ver quanto o WSL2 pode usar; `shutup-and-run wsl limits --apply` define o limite. |
| `WSL_LIMITS_ABOVE_BUDGET` | Run `shutup-and-run wsl limits --apply` (it keeps every other line of .wslconfig), then restart WSL yourself: `shutup-and-run wsl stop`, `wsl --shutdown`, `shutup-and-run wsl start`. | Rode `shutup-and-run wsl limits --apply` (mantém as outras linhas do .wslconfig) e reinicie o WSL você mesmo: `shutup-and-run wsl stop`, `wsl --shutdown`, `shutup-and-run wsl start`. |
| `WSL_LIMITS_NOT_IN_EFFECT` | The limits apply when WSL restarts: `shutup-and-run wsl stop`, `wsl --shutdown` (it stops every WSL distribution of this user), `shutup-and-run wsl start`. | Os limites valem quando o WSL reinicia: `shutup-and-run wsl stop`, `wsl --shutdown` (para todas as distribuições WSL deste usuário), `shutup-and-run wsl start`. |
| `INTERNET_NOT_RUNNING` | Run `shutup-and-run internet on --relay HOST:PORT` again with the same relay: it starts the process and its logon task (Linux: its systemd unit). | Rode `shutup-and-run internet on --relay HOST:PORTA` de novo com o mesmo relay: ele inicia o processo e a tarefa de logon (Linux: a unidade do systemd). |
| `INTERNET_RELAY_UNREACHABLE` | Check that this PC reaches {} (`Test-NetConnection HOST -Port PORT` on Windows, `nc -vz HOST PORT` on Linux) and that the relay runs; it reconnects by itself. | Confira se este PC alcança {} (`Test-NetConnection HOST -Port PORTA` no Windows, `nc -vz HOST PORTA` no Linux) e se o relay está no ar; ele reconecta sozinho. |
| `INTERNET_REFUSED` | The relay says why on this line. A token: `shutup-and-run internet on --relay HOST:PORT --token TOKEN` with the operator's. An id in use: the same with `--id NEW-NAME` or a new `--login`. | O relay diz o motivo nesta linha. Token: `shutup-and-run internet on --relay HOST:PORTA --token TOKEN` com o do operador. Id em uso: o mesmo com `--id NOVO-NOME` ou um novo `--login`. |
| `INTERNET_WAITING` | New connections resume by themselves when the reason on this line ends. `shutup-and-run internet resume` (or `resume`, for the worker's) ends a pause; `internet on ... --metered` accepts a metered network, `--max-day` sets the daily limit. | Novas conexões voltam sozinhas quando o motivo desta linha acabar. `shutup-and-run internet resume` (ou `resume`, a do worker) encerra uma pausa; `internet on ... --metered` aceita rede limitada, `--max-day` define o limite diário. |
| any other | Run `shutup-and-run doctor --report` and open the issue link it prints. | Rode `shutup-and-run doctor --report` e abra o link de issue que ele imprime. |

## A task from the operator: run it here, lend the internet, or both

A person asks you, their agent, to take part in the operator's work: run containers on this PC, lend its internet connection, or both. The two live side by side; neither needs the other.

1. **Ask before turning anything on.** Read this to the person, in their language, and go on only on a clear yes:
   > "This lets the operator use this PC: its internet connection, so their programs reach sites as if from your home, and, if it can, part of its processor and memory for their containers. Connections to your own network are refused. It waits while you are on battery, presenting, on a metered network, or outside the hours you set, and you can pause or stop it at any time on http://127.0.0.1:8788/ or with `shutup-and-run internet off`."
2. **What can it lend?** `shutup-and-run fit --json` (no install needed: run it once the `.exe` or the binary is there). `"lend": "both"` or `"internet"`; the `containers` line says why not, with its hint. No WSL2 means an administrator and a restart: lend the internet now and offer the containers for when they can restart.
3. **Internet, one command.** The operator gives the relay, its token and, best, the login it chose for this PC:
   ```bash
   shutup-and-run internet on --relay HOST:PORT --token TOKEN --login ID:PASSWORD --wait 60 --json
   ```
   Or, when the operator's poll URL answers the discovery ([contract](INTEGRATION-CONTRACT.md#internet-discovery)), only the URL:
   ```bash
   SHUTUP_AND_RUN_POLL_TOKEN=TOKEN shutup-and-run internet on --discover POLL_URL --wait 60 --json
   ```
   It returns once connected (`"connected": true`, exit 0), or exits 1 with `code` and `hint`: `INTERNET_REFUSED` (the relay said why), `INTERNET_WAITING` (a battery, a metered network, the hours: it starts by itself later), `INTERNET_RELAY_UNREACHABLE`. With `--login`, nothing needs to go back to the operator: the backend already knows `http://ID:PASSWORD@RELAY:8080` and checks the PC with `GET /status` on the relay. Without it, `internet credentials --json` gives the address to send to the operator, and nowhere else.
4. **Containers too**, when `fit` said `both`: Windows, `shutup-and-run wsl install --poll-url URL --max-slots 1`; Linux, the install line. Or [managed containers](#managed-containers-work-with-no-poll-url) when there is no poll URL.
5. **Check, one read.** `shutup-and-run status --json`: the worker, and an `internet` section when it is on. On a PC that only lends the internet, `"worker": "not installed"` and the exit code is the internet's.

On Windows the install line does steps 3 and 4: `$env:SHUTUP_AND_RUN_RELAY = "HOST:PORT"`, `$env:SHUTUP_AND_RUN_RELAY_TOKEN`, `$env:SHUTUP_AND_RUN_INTERNET_LOGIN = "ID:PASSWORD"`, and `$env:SHUTUP_AND_RUN_POLL_URL` for the worker too, then `irm .../install.ps1 | iex`. With discovery, `$env:SHUTUP_AND_RUN_POLL_URL` and `$env:SHUTUP_AND_RUN_LEND_INTERNET = "1"` are enough (and `$env:SHUTUP_AND_RUN_POLL_TOKEN` when the operator gave one). It installs the worker only where `fit` says containers; elsewhere it lends the internet and prints what the worker needs. On Linux the same variables go to `install.sh`; `SHUTUP_AND_RUN_INTERNET_ONLY=1` skips Docker and the worker.

The person can always stop it: the page's buttons (pause 1 hour, stop sharing), `shutup-and-run internet pause --for 2h`, `internet resume`, `internet off`. The worker's `pause` holds the internet too.

## Internet sharing: more

`--hours "mon-fri 19-07"` limits when; `--max-day 5g` limits the data a day; `--metered` lets it use a metered network, only if the PC's owner agreed. For a task that must go out through this PC, `shutup-and-run internet credentials --env` prints the `HTTPS_PROXY` and `HTTP_PROXY` lines for its `env` (docs/INTERNET.md, "A task that goes out through a client's connection"); the code for curl, requests, Selenium, Playwright, Puppeteer, Node and n8n is in its [examples](INTERNET.md#examples-curl-requests-selenium-playwright-puppeteer-node-n8n). `internet status --json` is this mode alone. `internet off --forget` undoes all of it, the id and password included. Turning it on is the owner's consent: do it only when the owner asked. docs/INTERNET.md has the risks to tell them.

## Managed containers (work with no poll URL)

When the owner wants a container to run on this machine by itself (every N seconds, or always), not from a queue:

```bash
shutup-and-run managed apply definition.json --json   # checked like a job; exit 3 and MANAGED_INVALID say what is wrong
printf '%s' "$VALUE" | shutup-and-run managed secret NAME KEY --json   # for each key of secret_env that is null in the file
shutup-and-run managed mode on --json                  # only while nothing runs; MANAGED_BUSY otherwise
shutup-and-run managed list --json
```

- A definition: `name`, `image` (in the allowlist), `policy` (`once`, `interval`, `always`), and optionally `command`, `env`, `secret_env`, `cpu`, `memory`, `shm`, `timeout_secs`, `interval_secs` (for `interval`, 60 or more), `enabled`. `examples/managed/rpa-selenium.json` is one to copy. Unknown fields are refused.
- `mode on` stops the worker from polling and from accepting `POST /v1/jobs` (`409` `MANAGED_MODE`). Say so to the owner before turning it on. `mode off` gives that back. Neither interrupts anything: stop the running containers first (`managed stop NAME`), or wait.
- In `list --json` each container has a `state`: `running`, `starting`, `scheduled`, `backoff` (an `always` container that keeps ending early; `failures` counts them), `waiting` (`note` says what the machine refused; it is retried), `needs_secret`, `stopped` (by a person; only `managed start` lifts it), `idle`, `disabled`, `off` (managed mode is off).
- `start` and `stop` are requests the running service acts on within 10 seconds. Read `list` again; do not repeat them in a loop.
- Never put a secret's value in a file, a command line or your answer. `managed secret` reads it from stdin, and nothing prints it back.

## Certificate passthrough (only when the owner asks)

Off by default, and it stays off unless the **owner of the machine** asks for it. Turning it on gives every container the owner's certificate and its password, and a container can copy the private key. Do not enable it on your own judgment, and do not pass `--accept-risk` unless the person told you they accept that. Say it to them in those words first.

```
SHUTUP_AND_RUN_CERT_PASSWORD='...' shutup-and-run certificate set --file "PATH\cert.pfx" --accept-risk --apply --json
shutup-and-run certificate status --json
shutup-and-run certificate validate --json     # exit 3 and a CERTIFICATE_* code when unusable
shutup-and-run demo sign --configured --json   # a container signs a made-up PDF with it
shutup-and-run certificate disable --forget --apply
```

The password goes in the environment variable or on stdin (`--password-stdin`), never in an argument, and never into a report, an issue or a chat. On Windows `--file` is a Windows path; spaces and accents are fine. A container reads the certificate at `/run/shutup-and-run/certificate/client.pfx` and the password in the file `/run/shutup-and-run/secrets/certificate-password`. Details and limits: [WINDOWS.md](WINDOWS.md#certificate-passthrough).

## Protect the PC: the shared-PC limits

The machine is someone's everyday computer. Their work comes first. A Windows worker has the `shared-pc` profile without being told; a Linux desktop gets it with one command:

```
shutup-and-run config set resource_profile shared-pc --apply
```

| Key | shared-pc default | Meaning |
| --- | --- | --- |
| `max_slots` | 1 (set by the install) | Jobs at once. |
| `max_job_cpus` | `1` | The most CPU one job gets, and what a job that names none gets. |
| `max_job_memory` | `2g` | The same for memory. No swap beyond it. |
| `min_free_memory` | `2g` | No new job starts with less memory available. `0`: not checked. |
| `min_free_disk` | `5g` | The same for the disk Docker writes to. |
| `min_idle_secs` | `0` | Windows only: a new job waits until the PC was not used for this many seconds. `0`: off. A running job is never stopped for it. |
| `lend_on_metered` | `false` | Windows only: `false` waits on a metered network (`METERED_NETWORK`); `true` only when the person at the PC accepts that jobs spend its data. |

Where the service sees under 4 GiB of memory, the two memory defaults are half of what it sees. Change one figure with `config set KEY VALUE --apply`; go back to the default with `config unset KEY --apply`. `resource_profile none` removes the defaults, and the four keys still work one by one.

Inspect what is in force, and whether the worker is waiting for memory or disk:

```
shutup-and-run config show --json   # "effective": the limits and the poll interval that run, defaults filled in
shutup-and-run status --json     # "limits": {...}, "waiting_for": null or "low on memory: ...", "safety": {...}
shutup-and-run doctor --json     # the "resources" check; on Windows also "wsl-limits"
```

On Windows the whole WSL2 VM has its own limit, in the user's `%UserProfile%\.wslconfig`. That file covers every WSL distribution of the user, so check before changing it:

```powershell
shutup-and-run wsl limits --json            # exit 1: the file lets WSL2 take more than the budget
shutup-and-run wsl limits --apply           # write memory= and processors=, keep every other line
shutup-and-run wsl limits --memory 4GB --processors 2 --apply
```

`--apply` saves the old file as `.wslconfig.bak`. The new limits take effect when WSL restarts. **Do not run `wsl --shutdown` on your own**: it stops every WSL distribution, the user's included. Tell the person, or wait for the next restart of the PC. If the PC has Docker Desktop or another distribution, ask before `--apply`.

None of this guarantees a responsive PC. See [WINDOWS.md](WINDOWS.md#sharing-the-pc).

## Safety of the owner's computer: what you may and may not do

Under the `shared-pc` profile the worker asks, before every job, whether starting it is safe for the person who owns the computer. It reads memory, disk and processor load where the containers run, and on Windows also the PC's own memory, processor use and disk. The answer is one of three:

| `safety.level` | `safety.admission` | What happens |
| --- | --- | --- |
| `healthy` | `open` | Jobs start. |
| `warning` | `authorization_required` | No job starts until the owner of the computer says yes to the risks listed. |
| `warning` | `open` | The owner said yes to those risks; jobs start. |
| `blocked` | `blocked` | No job starts. It clears by itself. No authorization lifts it. |

```
shutup-and-run safety --json     # {"ok":true,"safety":{"level","admission","reasons":[{"code","level","detail"}],"needs_authorization":[...],"authorization":{...}|null,"incidents":[...]}}
shutup-and-run status --json     # the same object under "safety", and "waiting_for" says it in one line
shutup-and-run doctor --json     # the "safety" check: ok, or warn with the code below
shutup-and-run safety revoke     # withdraw the owner's authorization (always allowed)
```

**You do not authorize on your own.** `AUTHORIZATION_REQUIRED` is a decision that belongs to the person whose computer it is. `shutup-and-run safety authorize` asks that person, at a terminal, to type a word after reading the risks; with `--json`, or without a terminal (which is every call through `shutup-and-run.exe` on Windows), it exits 1 with `AUTHORIZATION_REQUIRED` and changes nothing. The one exception is the consent the owner already gave in the contract: when the operator who deploys the machine tells you so, install with `shutup-and-run wsl install ... --owner-consent`, or run `shutup-and-run safety authorize --owner-consent` on a machine already installed. It records that consent with the channel `contract` and never lifts a block. Without that word from the operator, do not use it; there is no config key, environment variable or API field that says yes, and none should be looked for. Do not drive the local page, pipe the word in through a pseudo-terminal, or write to the history file: tell the person what the worker found, in their language, and where to decide: `http://127.0.0.1:8787/` on that computer (the desktop shortcut), "Safety of this computer". If they say no, leave the worker waiting or run `shutup-and-run pause`.

The warnings (`reasons[].level` `warning`): `SAFETY_SMALL_MACHINE` (under 8 GiB of memory, or 2 processors or fewer), `SAFETY_MEMORY_LOW` (under 20% of memory available), `SAFETY_CPU_BUSY` (the PC's processors 75% busy or more, or a load of 1 per processor where the containers run), `SAFETY_HOST_UNKNOWN` (Windows is not reporting the PC to the worker). The owner's yes covers the warnings shown at that moment; a new one asks again, and so does a new version of the notice (`notice_version`).

The blocks (`level` `blocked`): `SAFETY_MEMORY_CRITICAL` (under 10% of memory, or under 1 GiB on the PC, 256 MiB inside WSL2), `SAFETY_DISK_CRITICAL` (under 2 GiB free), `SAFETY_CPU_CRITICAL` (processors 95% busy, or a load of 2 per processor), `SAFETY_TELEMETRY_MISSING` (a reading could not be taken), `SAFETY_OOM_BREAKER` (three jobs killed for memory in 30 minutes: 15 minutes without new jobs). Each clears a little above the line it tripped at, so it does not flap. A running job is never stopped for any of them. Nothing in the config changes these lines.

`incidents` are the last few things recorded for the owner: `oom` (a job reached its memory limit; `jobs --json` has `"oom_killed": true` for it), `cpu_throttled` (a job was held at its CPU limit for most of its time), `blocked`, `recovered`, `authorization_granted`, `authorization_revoked`. They stay on the machine.

## Check that it works

```
shutup-and-run status --json     # state: idle | active | draining | failed | stopped; last_poll_at, last_poll_status
shutup-and-run doctor --json     # worker_ready, poll_endpoint_ok, and checks[], each with status ok|warn|fail, detail, hint
shutup-and-run jobs --json       # the last 20 jobs and how each ended
shutup-and-run jobs --summary --json  # what they used, and a max_job_memory and max_slots that would fit more
shutup-and-run status --json     # "memory": how much to free, where, and how long jobs waited (never program names)
```

"Is it asking for work?" is `last_poll_at` and `last_poll_status` in `status --json`: when the last request went to the poll URL, and how it ended: `task`, `nothing`, `task_rejected` (a task came and this machine refused it), `not_approved` or `error`. Both are null while nothing was sent: no poll URL, or the worker is full, paused or short of memory or disk.

`jobs` answers "did my petition run?". Newest first, `--limit N` up to 200:

```json
{"ok": true, "limit": 20, "jobs": [
  {"id": "9f2c...", "image": "registry.example/protocol:1", "started_at": "2026-11-14T12:00:00Z",
   "ended_at": "2026-11-14T12:00:42Z", "duration_secs": 42, "exit_code": 0, "status": "ok", "failure": null, "oom_killed": false,
   "log": {"path": "/var/lib/shutup-and-run/logs/9f2c....log", "bytes": 1834, "command": "shutup-and-run jobs logs 9f2c..."}}
]}
```

`status` is `running`, `ok` (exit 0), `failed` (another exit code, or the container never started), `timeout` (it ran past its time limit), `killed` (exit 137: stopped from outside; `oom_killed` is true when Docker confirmed it was for memory) or `interrupted` (the PC, WSL or Docker stopped under it, or the worker was not running when it ended; it is not run again, the service that sent it decides). `failure` says why in one sentence, and is null for `ok` and `running`. The history keeps the last 500 finished jobs at most, for `history_days`. It never holds or prints a job's env, command or payload. The result of a job is not here either: the container sends it to the service itself. `log` is null when the job has no log: it still runs, it never started a container, or its log is older than 7 days.

### What a job printed: `jobs logs`

Every job that ran a container leaves its output on the machine: the last 256 KiB of stdout and stderr together, in `logs/<job id>.log` next to the history (`/var/lib/shutup-and-run/logs/` on Linux and inside the WSL distribution on Windows). It is written when the job ends, however it ends (ok, failed, timeout, out of memory, stopped), in push, pull and managed mode alike. Files older than 7 days are deleted. Nothing is masked: what the job printed is there as it printed it.

```bash
shutup-and-run jobs logs 9f2c            # the start of an id is enough
shutup-and-run jobs logs --last --tail 50
shutup-and-run jobs logs 9f2c --json     # {"id", "path", "bytes", "written_at", "live", "image", "status", "exit_code", "log"}
```

A job still running has no file yet: `jobs logs` reads what it printed so far from Docker, with `"live": true` and `path` null.

On Windows the same commands run in PowerShell; the `.exe` forwards them into the distribution. An id that matches nothing is `JOB_NOT_FOUND`, one that matches several is `JOB_ID_AMBIGUOUS`, and a job with no log is `JOB_LOG_MISSING`; each exits 1 with its hint. When the last job did not end well, the doctor warns with `LAST_JOB_FAILED` and its hint is the exact `jobs logs` command for that job. The person at the PC has the same files on the local page: `http://127.0.0.1:8787/local/logs`, to open or download.

"Is the machine broken, or is it the server?" is two fields of `doctor --json` (and of `config check --json`), since 0.2.12:

| Field | Value | Meaning |
| --- | --- | --- |
| `worker_ready` | `true` / `false` | No check other than `poll` failed: the config, Docker and the service are fine, and this machine can run a job. A warning (`warn`) does not make it false. |
| `poll_endpoint_ok` | `true` / `false` / `null` | Did the poll URL answer as a poll endpoint? `null` means nobody asked it: see `poll_endpoint`. |
| `poll_endpoint` | `ok`, `error`, `disabled`, `not_checked` | `disabled`: no `poll_url` is set. `not_checked`: the checks stopped before the poll URL (no config, or on Windows no WSL, distribution or Docker). `error`: the `poll` check's `code` says which (`POLL_HTTP_ERROR`, `POLL_UNREACHABLE`, `POLL_NOT_APPROVED`, `POLL_RATE_LIMITED`, `POLL_ERROR`). |

`ok`, the exit code and every check `code` are what they were. A worker that runs while its poll URL answers 404 is `"ok": false`, exit 1, `"worker_ready": true`, `"poll_endpoint_ok": false`, with `POLL_HTTP_ERROR` on the `poll` check: nothing on the machine needs fixing, the URL or the server behind it does. The text output ends with "the worker is ready to run jobs, but the poll URL did not answer as expected".

Healthy is: `status` exits 0, `doctor` exits 0, and `poll_error` is null. A new machine shows `poll` as `warn` with "has not approved this machine" until the owner approves it at the poll URL. That is expected, and nothing on the machine fixes it.

The person at the PC sees the same at `http://127.0.0.1:8787/` (the desktop shortcut).

## Problems already seen

Run `shutup-and-run doctor` first. Every failed line has a `hint:`. The cases below are what those lines mean.

| Symptom | Cause | Fix |
| --- | --- | --- |
| `poll` fails: cannot reach the URL, and the URL is on this PC or this LAN | The Windows firewall blocks WSL to the host when the network profile is Public | PowerShell as administrator: `Get-NetConnectionProfile` shows the profile. Either `Set-NetConnectionProfile -InterfaceAlias "NAME" -NetworkCategory Private`, or allow the port: `New-NetFirewallRule -DisplayName "poll" -Direction Inbound -Action Allow -Protocol TCP -LocalPort PORT -Profile Any`. Use the PC's LAN address in the URL, never `127.0.0.1`. |
| `poll` fails: cannot reach the URL, on the internet | No internet, DNS, or a network that blocks the host | Check `shutup-and-run config get poll_url`. The worker keeps retrying, up to every 5 minutes, and recovers alone when the network does. |
| `poll` warns: not approved (401/403) | The poll URL does not know this machine's token yet | The owner approves the worker by name at the poll URL. Nothing to do here. |
| `bans` fails: this machine's own address is banned | Wrong tokens were sent to the local API (401), three times | `shutup-and-run bans lift 127.0.0.1` |
| `service` fails with "listen" or "in use" | Another program has port 8787 | `shutup-and-run config set listen 127.0.0.1:8788`, then on Windows `shutup-and-run wsl stop` and `shutup-and-run wsl start`. The desktop shortcut follows the port at the next `config apply`. |
| `resources` warns: low on memory | Programs on the PC are using the memory. No new job starts; nothing is lost | It clears by itself. To ask for less: `shutup-and-run config set min_free_memory 1g --apply`. On Windows, `shutup-and-run wsl limits` shows what WSL2 has. |
| `resources` warns: low on disk | Images and containers filled the disk | Windows: `wsl -d shutup-and-run -u root docker image prune -af`. Linux: `docker image prune -af`. Or `shutup-and-run config set min_free_disk 2g --apply`. |
| `resources` warns: N jobs of X each is more than this machine has | `max_slots` times `max_job_memory` does not fit | Run the `config set max_slots` command in its `hint`. |
| `wsl-limits` warns | `.wslconfig` is missing, or lets WSL2 take more than 40% of the RAM or half the processors | `shutup-and-run wsl limits --apply`, then WSL has to restart (see above). "not in effect yet" needs only the restart. |
| Jobs never start, and the log says `poll task rejected` | The task asks for more `cpu` or `memory` than `max_job_cpus` / `max_job_memory` | The owner sends a smaller task, or raise the limit: `shutup-and-run config set max_job_memory 3g --apply`. |
| `paused` warns | Someone paused the worker on the page and forgot | `shutup-and-run resume`. A restart of the PC also resumes it, unless it was paused for a while (`detail` says until when; it resumes by itself then). |
| `service` not running (Windows) | The logon task is not running | `shutup-and-run wsl start`. `task` warns if the task is missing: `shutup-and-run wsl task register`. |
| `config` fails, or `config-in-use` warns | The file was damaged; the service is running on an older copy or on defaults | `shutup-and-run config validate` shows why. `shutup-and-run config reset` writes the factory config (tokens kept), then set the poll URL again. |
| `docker` or `distribution` fails, or anything odd after a power cut | The WSL distribution is damaged | `shutup-and-run wsl reset` deletes and rebuilds it and keeps the config. `--factory` forgets the config too; the machine then needs approval again. |
| `wsl install` printed `note: the install is complete` about the poll URL | The poll URL did not answer during the install. The install itself succeeded (exit 0) | Nothing on the PC. Check the URL with `shutup-and-run config get poll_url`; `shutup-and-run doctor` later. |
| Re-install | | Run `shutup-and-run wsl install` again with no options. It keeps the config. |
| Factory reset | | Config only: `shutup-and-run config reset` (tokens, name and listen address kept; resource limits back to the profile's defaults). The whole Windows worker: `shutup-and-run wsl reset --factory`. |
| `container` fails | The smoke image could not be pulled | A restricted network blocks the registry. Try another network, then the doctor again. |

On Windows the supervisor's log is `shutup-and-run wsl logs --lines 60`.

## Open an issue

```
shutup-and-run doctor --report
```

It prints a diagnostic report with tokens and passwords masked, and a URL that opens a new GitHub issue with that report filled in. You do not need the user's GitHub login: give the URL to the person, or open it in their browser, and they submit it. With `--json` the same are in `report` and `issue_url`.

## What the page can change

The certificate section (Advanced settings, Certificate) does what `shutup-and-run certificate` does, behind the same login and a confirmation box.

`http://127.0.0.1:8787/` opens only on the machine itself. Everyday keys (poll URL, interval, slots, machine name, pause) are edited directly under Settings. The dangerous ones (allowed images, any-image, time limits, both tokens, the admin login) are under Advanced settings and ask for a user and password: `admin` / `admin` until changed with

```
shutup-and-run config set admin_password "NEW" --apply
```

That password keeps a curious person out. It is not security: the machine's user can read the file.

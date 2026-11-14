# For agents

You are an AI agent setting up or repairing a shutup-and-run worker on someone's machine. Everything here is non-interactive. No command asks a question, and you never edit a file by hand.

## Rules of the CLI

- Add `--json` to `config`, `status`, `doctor`, `jobs`, `pause` and `resume`: one JSON object on stdout, `"ok": true|false`, and `"error"`, `"code"` and `"hint"` when false.
- `hint` is the next step: short, one concrete action, with the command when there is one. Run it if you have a shell; read it to the person if you are guiding one. Without `--json` it is the `hint:` line. Its language is the config key `language`: `shutup-and-run config set language pt` for Portuguese, `en` (the default) for English. The [catalogue](#codes-and-hints) lists every code.
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

Each prints `ok`, a top-level `code`, and `checks[]` with `name`, `code`, `status`, `detail`, `fix`, `hint`. Act on `code`, show `detail` to a person, run `fix` (the exact command for this machine) or follow `hint` (the same step in the configured language). None of these changes the config or the job history. `demo` runs `hello-world`; with another image use `--image`, and if the answer is `DEMO_IMAGE_NOT_ALLOWED` its `fix` is the command that allows it. `job validate` is the static half of admission only: a `JOB_VALID` task can still meet a full or paused machine later.

## Diagnostic codes

`doctor --json`, `config check --json` and `demo --json` give every check a `code` that does not change between releases. A check that passed is `NAME_OK`. One that did not, and has no code of its own below, is `NAME_WARNING` or `NAME_FAILED`.

| Code | Meaning | First thing to do |
| --- | --- | --- |
| `CONFIG_INVALID` | The config file does not validate | `shutup-and-run config validate`, then `config set` or `config reset` |
| `CONFIG_FALLBACK_IN_USE` | The file was damaged; the service runs on an older copy or on defaults | `shutup-and-run config reset` |
| `DOCKER_UNAVAILABLE` | Docker did not answer | Windows: `wsl stop`, `wsl start`. Linux: `systemctl start docker` |
| `SERVICE_NOT_RUNNING`, `SERVICE_FAILED`, `SERVICE_PORT_IN_USE` | The service is down, crashed, or its port is taken | The check's `fix` |
| `STORAGE_NOT_WRITABLE` | The service cannot write its history folder | Run as root; fix the folder's owner |
| `LOW_MEMORY`, `LOW_DISK` | No new job starts until it clears | Wait, free disk, or lower `min_free_*` |
| `SLOTS_EXCEED_MEMORY` | `max_slots` times `max_job_memory` does not fit | The `config set max_slots` in `fix` |
| `POLL_INTERVAL_RAISED` | The config asks for a poll interval under 30 s, from an older release. The worker runs, on 30 | `shutup-and-run config set poll_interval_secs 30 --apply` |
| `POLL_URL_UNSET` | No poll URL: the worker is idle | `config set poll_url URL --apply` |
| `POLL_UNREACHABLE` | The poll URL's host did not answer. The network or the server, not the config | Check the network, DNS, the URL |
| `POLL_NOT_APPROVED` | The server has not approved this machine | Nothing here: the owner approves it |
| `POLL_HTTP_ERROR` | The poll URL answered 404, 405 or 5xx: a wrong path or a failing server. `config check` asks it once, with no token and no free slot | `config get poll_url --reveal`, then `config set poll_url URL --apply` |
| `POLL_RATE_LIMITED` | The poll URL answered 429 | Nothing: the worker backs off by itself |
| `POLL_ERROR` | The poll failed some other way | Check the URL and its query |
| `WORKER_PAUSED` | Someone paused the worker | `shutup-and-run resume` |
| `LOCAL_ADDRESS_BANNED`, `ADDRESS_BANNED` | Wrong tokens banned an address | `shutup-and-run bans lift IP` |
| `WSL_UNAVAILABLE`, `DISTRIBUTION_MISSING`, `TASK_NOT_REGISTERED` | Windows: WSL, the distribution or the logon task is missing | `wsl --install --no-distribution`; `shutup-and-run wsl install`; `wsl task register` |
| `WSL_LIMITS_ABOVE_BUDGET`, `WSL_LIMITS_NOT_IN_EFFECT` | `.wslconfig` lets WSL2 take too much, or WSL has not restarted | `shutup-and-run wsl limits --apply`; a restart of WSL |
| `CERTIFICATE_MISSING`, `CERTIFICATE_INVALID`, `CERTIFICATE_PASSWORD_WRONG`, `CERTIFICATE_EXPIRED`, `CERTIFICATE_NO_PRIVATE_KEY` | Passthrough is on and the certificate cannot be used | `shutup-and-run certificate set --file PATH --accept-risk` with a good file, or `certificate disable` |
| `OPENSSL_MISSING` | The openssl program is not installed where the service runs | `apt-get install -y openssl` |
| `DEMO_IMAGE_NOT_ALLOWED`, `DEMO_IMAGE_UNAVAILABLE`, `DEMO_JOB_FAILED`, `DEMO_TIMEOUT`, `WORKER_BUSY`, `LIMITS_NOT_APPLIED`, `DEMO_CLEANUP_FAILED` | `demo`: which step failed | The check's `fix` |
| `SIGNATURE_INVALID`, `CERTIFICATE_MOUNT_WRITABLE` | `demo sign`: the PDF did not verify, or the mount was not read-only | `doctor --report` and open the issue |
| `JOB_VALID`, `JOB_INVALID_JSON`, `JOB_INVALID_FIELD`, `JOB_IMAGE_NOT_ALLOWED` | `job validate` | Fix the task, or the allowlist |

## Codes and hints

Every error and warning code with its `hint`, in both languages, as the CLI prints it (`src/hints.rs`; a test keeps this table complete). A code that is not here gets the last line's hint. A check that passed is `NAME_OK` and has no hint.

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
| `SERVICE_FAILED` | Fix the docker line first, then start the service: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. | Corrija primeiro a linha do docker e inicie o serviço: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. |
| `SERVICE_NOT_RUNNING` | Start the service: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. | Inicie o serviço: Windows `shutup-and-run wsl start`, Linux `sudo systemctl restart shutup-and-run`. |
| `WORKER_PAUSED` | Run `shutup-and-run resume`. | Rode `shutup-and-run resume`. |
| `LOCAL_ADDRESS_BANNED` | Run `shutup-and-run bans lift 127.0.0.1`, then check the token of whoever calls this machine. | Rode `shutup-and-run bans lift 127.0.0.1` e confira o token de quem chama esta máquina. |
| `ADDRESS_BANNED` | Only if one of the addresses is yours: `shutup-and-run bans lift IP`. | Só se um dos endereços for seu: `shutup-and-run bans lift IP`. |
| `RESOURCES_FAILED` | Free memory or disk on this machine, or lower the need: `shutup-and-run config set max_slots 1 --apply`. | Libere memória ou disco nesta máquina, ou reduza a exigência: `shutup-and-run config set max_slots 1 --apply`. |
| `RESOURCES_WARNING` | The worker waits until memory and disk are free again. Close heavy programs or free disk space. | O worker espera até haver memória e disco livres. Feche programas pesados ou libere espaço em disco. |
| `LOW_MEMORY` | Close heavy programs on this machine, then try again; or lower the floor: `shutup-and-run config set min_free_memory 1g --apply`. | Feche programas pesados nesta máquina e tente de novo; ou reduza o mínimo: `shutup-and-run config set min_free_memory 1g --apply`. |
| `LOW_DISK` | Free disk space: `docker image prune -af` (on Windows inside the distribution: `wsl -d shutup-and-run -u root docker image prune -af`). | Libere espaço em disco: `docker image prune -af` (no Windows, dentro da distribuição: `wsl -d shutup-and-run -u root docker image prune -af`). |
| `WORKER_BUSY` | Every slot is in use. Wait for the running job (`shutup-and-run jobs`) and try again. | Todos os slots estão em uso. Aguarde a tarefa em execução (`shutup-and-run jobs`) e tente de novo. |
| `SLOTS_EXCEED_MEMORY` | Run `shutup-and-run config set max_slots 1 --apply`, or lower `max_job_memory`. | Rode `shutup-and-run config set max_slots 1 --apply`, ou reduza `max_job_memory`. |
| `POLL_URL_UNSET` | Run `shutup-and-run config set poll_url URL --apply` with the URL the service gave you. | Rode `shutup-and-run config set poll_url URL --apply` com a URL que o serviço informou. |
| `POLL_UNREACHABLE` | Check the internet and the address: `shutup-and-run config get poll_url --reveal`. Then `shutup-and-run doctor` again. | Confira a internet e o endereço: `shutup-and-run config get poll_url --reveal`. Depois rode `shutup-and-run doctor` de novo. |
| `POLL_NOT_APPROVED` | Nothing to do on this machine: ask the service to approve this worker. It keeps asking by itself. | Nada a fazer nesta máquina: peça ao serviço para aprovar este worker. Ele continua tentando sozinho. |
| `POLL_HTTP_ERROR` | The poll URL answered with an error. Check the path and the query: `shutup-and-run config get poll_url --reveal`; fix with `shutup-and-run config set poll_url URL --apply`. | A poll URL respondeu com erro. Confira o caminho e os parâmetros: `shutup-and-run config get poll_url --reveal`; corrija com `shutup-and-run config set poll_url URL --apply`. |
| `POLL_RATE_LIMITED` | The poll URL asked this machine to slow down (429). Wait: the worker backs off by itself. | A poll URL pediu para esta máquina ir mais devagar (429). Aguarde: o worker reduz o ritmo sozinho. |
| `POLL_ERROR` | Check the URL and its query with `shutup-and-run config get poll_url --reveal`, then `shutup-and-run status`. | Confira a URL e seus parâmetros com `shutup-and-run config get poll_url --reveal` e depois `shutup-and-run status`. |
| `JOB_INVALID_JSON` | Fix the JSON of the job file and run `shutup-and-run job validate FILE` again. | Corrija o JSON do arquivo da tarefa e rode `shutup-and-run job validate ARQUIVO` de novo. |
| `JOB_INVALID_FIELD` | Fix the field named in the error; docs/INTEGRATION-CONTRACT.md lists the fields of a job. | Corrija o campo citado no erro; docs/INTEGRATION-CONTRACT.md lista os campos de uma tarefa. |
| `JOB_IMAGE_NOT_ALLOWED` | Add the image: `shutup-and-run config set allowlist IMAGE --apply`, or validate as a polled task with `--poll`. | Adicione a imagem: `shutup-and-run config set allowlist IMAGEM --apply`, ou valide como tarefa da poll URL com `--poll`. |
| `DEMO_IMAGE_NOT_ALLOWED` | Add the demo image: `shutup-and-run config set allowlist hello-world --apply`. | Adicione a imagem da demo: `shutup-and-run config set allowlist hello-world --apply`. |
| `DEMO_JOB_INVALID` | Run `shutup-and-run config check` and fix what it reports. | Rode `shutup-and-run config check` e corrija o que ele apontar. |
| `JOB_FAILED` | Run `shutup-and-run config check` and fix what it reports. | Rode `shutup-and-run config check` e corrija o que ele apontar. |
| `ADMISSION_FAILED` | The machine is short of memory or disk, or paused. Run `shutup-and-run status` and free what it names. | A máquina está com pouca memória ou disco, ou pausada. Rode `shutup-and-run status` e libere o que ele citar. |
| `DEMO_TIMEOUT` | The demo container did not finish in time. Check the internet (the image is downloaded) and run the demo again. | O container da demo não terminou a tempo. Confira a internet (a imagem é baixada) e rode a demo de novo. |
| `CONTAINER_FAILED` | Run `shutup-and-run doctor`; if Docker is fine, run the demo again. | Rode `shutup-and-run doctor`; se o Docker estiver bem, rode a demo de novo. |
| `DEMO_IMAGE_UNAVAILABLE` | The image could not be downloaded. Check the internet and run the demo again. | A imagem não pôde ser baixada. Confira a internet e rode a demo de novo. |
| `IMAGE_FAILED` | The image could not be downloaded. Check the internet and run the demo again. | A imagem não pôde ser baixada. Confira a internet e rode a demo de novo. |
| `DEMO_JOB_FAILED` | The demo container exited with an error. Run `shutup-and-run doctor`, then the demo again. | O container da demo terminou com erro. Rode `shutup-and-run doctor` e depois a demo de novo. |
| `LIMITS_NOT_APPLIED` | Docker did not apply the limits. Update Docker, or on Windows run `shutup-and-run wsl reset`. | O Docker não aplicou os limites. Atualize o Docker ou, no Windows, rode `shutup-and-run wsl reset`. |
| `LIMITS_NOT_READ` | The limits could not be read back. Run the demo again; if it repeats, `shutup-and-run doctor --report`. | Não foi possível ler os limites. Rode a demo de novo; se repetir, `shutup-and-run doctor --report`. |
| `LIMITS_FAILED` | Docker did not apply the limits. Update Docker, or on Windows run `shutup-and-run wsl reset`. | O Docker não aplicou os limites. Atualize o Docker ou, no Windows, rode `shutup-and-run wsl reset`. |
| `DEMO_CLEANUP_FAILED` | A demo container was left behind. Remove it: `docker rm -f` the container named in the detail. | Um container da demo ficou para trás. Remova: `docker rm -f` no container citado no detalhe. |
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
| `WSL_UNAVAILABLE` | In an administrator PowerShell run `wsl --install --no-distribution`, restart the PC, then `shutup-and-run wsl install`. | Em um PowerShell de administrador rode `wsl --install --no-distribution`, reinicie o PC e depois `shutup-and-run wsl install`. |
| `WSL_FAILED` | In an administrator PowerShell run `wsl --update`, restart the PC, then `shutup-and-run doctor`. | Em um PowerShell de administrador rode `wsl --update`, reinicie o PC e depois `shutup-and-run doctor`. |
| `DISTRIBUTION_MISSING` | Run `shutup-and-run wsl install`. | Rode `shutup-and-run wsl install`. |
| `DISTRIBUTION_FAILED` | Run `shutup-and-run wsl reset` to rebuild the distribution; the config is kept. | Rode `shutup-and-run wsl reset` para reconstruir a distribuição; a config é mantida. |
| `BINARY_FAILED` | Run `shutup-and-run wsl install` again: it puts the right binary in the distribution. | Rode `shutup-and-run wsl install` de novo: ele coloca o binário certo na distribuição. |
| `TASK_NOT_REGISTERED` | Run `shutup-and-run wsl task register` so the worker starts at logon. | Rode `shutup-and-run wsl task register` para o worker iniciar no logon. |
| `TASK_WARNING` | Run `shutup-and-run wsl task register` so the worker starts at logon. | Rode `shutup-and-run wsl task register` para o worker iniciar no logon. |
| `WSL_LIMITS_WARNING` | Run `shutup-and-run wsl limits` to see what WSL2 may take; `shutup-and-run wsl limits --apply` sets the budget. | Rode `shutup-and-run wsl limits` para ver quanto o WSL2 pode usar; `shutup-and-run wsl limits --apply` define o limite. |
| `WSL_LIMITS_ABOVE_BUDGET` | Run `shutup-and-run wsl limits --apply`, then `wsl --shutdown` and `shutup-and-run wsl start`. | Rode `shutup-and-run wsl limits --apply`, depois `wsl --shutdown` e `shutup-and-run wsl start`. |
| `WSL_LIMITS_NOT_IN_EFFECT` | Restart WSL so the limits apply: `shutup-and-run wsl stop`, `wsl --shutdown`, `shutup-and-run wsl start`. | Reinicie o WSL para os limites valerem: `shutup-and-run wsl stop`, `wsl --shutdown`, `shutup-and-run wsl start`. |
| any other | Run `shutup-and-run doctor --report` and open the issue link it prints. | Rode `shutup-and-run doctor --report` e abra o link de issue que ele imprime. |

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

Where the service sees under 4 GiB of memory, the two memory defaults are half of what it sees. Change one figure with `config set KEY VALUE --apply`; go back to the default with `config unset KEY --apply`. `resource_profile none` removes the defaults, and the four keys still work one by one.

Inspect what is in force, and whether the worker is waiting for memory or disk:

```
shutup-and-run config show --json   # "effective": the limits and the poll interval that run, defaults filled in
shutup-and-run status --json     # "limits": {...}, "waiting_for": null or "low on memory: ..."
shutup-and-run doctor --json     # the "resources" check; on Windows also "wsl-limits"
```

On Windows the whole WSL2 VM has its own limit, in the user's `%UserProfile%\.wslconfig`. That file covers every WSL distribution of the user, so check before changing it:

```powershell
shutup-and-run wsl limits --json            # exit 1: the file lets WSL2 take more than the budget
shutup-and-run wsl limits --apply           # write memory= and processors=, keep every other line
shutup-and-run wsl limits --memory 4GB --processors 2 --apply
```

`--apply` saves the old file as `.wslconfig.bak`. The new limits take effect when WSL restarts. **Do not run `wsl --shutdown` on your own**: it stops every WSL distribution, the user's included. Tell the person, or wait for the next restart of the PC. If the PC has Docker Desktop or another distribution, ask before `--apply`.

None of this guarantees a responsive PC. The memory check reads the VM, not Windows, and only when a job starts. See [WINDOWS.md](WINDOWS.md#sharing-the-pc).

## Check that it works

```
shutup-and-run status --json     # state: idle | active | draining | failed | stopped
shutup-and-run doctor --json     # checks[], each with status ok|warn|fail, detail, fix, hint
shutup-and-run jobs --json       # the last 20 jobs and how each ended
```

`jobs` answers "did my petition run?". Newest first, `--limit N` up to 200:

```json
{"ok": true, "limit": 20, "jobs": [
  {"id": "9f2c...", "image": "registry.example/protocol:1", "started_at": "2026-11-14T12:00:00Z",
   "ended_at": "2026-11-14T12:00:42Z", "duration_secs": 42, "exit_code": 0, "status": "ok", "failure": null}
]}
```

`status` is `running`, `ok` (exit 0), `failed` (another exit code, or the container never started), `timeout` (it ran past its time limit) or `killed` (exit 137: out of memory, or stopped). `failure` says why in one sentence, and is null for `ok` and `running`. The history keeps the last 500 finished jobs at most, for `history_days`. It never holds or prints a job's env, command, payload or logs. The result of a job is not here either: the container sends it to the service itself.

Healthy is: `status` exits 0, `doctor` exits 0, and `poll_error` is null. A new machine shows `poll` as `warn` with "has not approved this machine" until the owner approves it at the poll URL. That is expected, and nothing on the machine fixes it.

The person at the PC sees the same at `http://127.0.0.1:8787/` (the desktop shortcut).

## Problems already seen

Run `shutup-and-run doctor` first. Every failed line has a `fix:`. The cases below are what those lines mean.

| Symptom | Cause | Fix |
| --- | --- | --- |
| `poll` fails: cannot reach the URL, and the URL is on this PC or this LAN | The Windows firewall blocks WSL to the host when the network profile is Public | PowerShell as administrator: `Get-NetConnectionProfile` shows the profile. Either `Set-NetConnectionProfile -InterfaceAlias "NAME" -NetworkCategory Private`, or allow the port: `New-NetFirewallRule -DisplayName "poll" -Direction Inbound -Action Allow -Protocol TCP -LocalPort PORT -Profile Any`. Use the PC's LAN address in the URL, never `127.0.0.1`. |
| `poll` fails: cannot reach the URL, on the internet | No internet, DNS, or a network that blocks the host | Check `shutup-and-run config get poll_url`. The worker keeps retrying, up to every 5 minutes, and recovers alone when the network does. |
| `poll` warns: not approved (401/403) | The poll URL does not know this machine's token yet | The owner approves the worker by name at the poll URL. Nothing to do here. |
| `bans` fails: this machine's own address is banned | Wrong tokens were sent to the local API (401), three times | `shutup-and-run bans lift 127.0.0.1` |
| `service` fails with "listen" or "in use" | Another program has port 8787 | `shutup-and-run config set listen 127.0.0.1:8788`, then on Windows `shutup-and-run wsl stop` and `shutup-and-run wsl start`. The desktop shortcut follows the port at the next `config apply`. |
| `resources` warns: low on memory | Programs on the PC are using the memory. No new job starts; nothing is lost | It clears by itself. To ask for less: `shutup-and-run config set min_free_memory 1g --apply`. On Windows, `shutup-and-run wsl limits` shows what WSL2 has. |
| `resources` warns: low on disk | Images and containers filled the disk | Windows: `wsl -d shutup-and-run -u root docker image prune -af`. Linux: `docker image prune -af`. Or `shutup-and-run config set min_free_disk 2g --apply`. |
| `resources` warns: N jobs of X each is more than this machine has | `max_slots` times `max_job_memory` does not fit | Run the `config set max_slots` command in its `fix`. |
| `wsl-limits` warns | `.wslconfig` is missing, or lets WSL2 take more than 40% of the RAM or half the processors | `shutup-and-run wsl limits --apply`, then WSL has to restart (see above). "not in effect yet" needs only the restart. |
| Jobs never start, and the log says `poll task rejected` | The task asks for more `cpu` or `memory` than `max_job_cpus` / `max_job_memory` | The owner sends a smaller task, or raise the limit: `shutup-and-run config set max_job_memory 3g --apply`. |
| `paused` warns | Someone paused the worker on the page and forgot | `shutup-and-run resume`. A restart of the PC also resumes it. |
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

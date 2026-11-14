# Windows 11

The Windows worker is the same product in pull mode. A Windows 11 machine, usually one a client lends, asks your orchestrator for work and runs it as a container inside WSL2. Nothing is pushed to it, and no port is opened on it.

Tested target: Windows 11 Pro 25H2 with WSL2. Windows 10 is not a target.

## What is on the machine

```text
 Windows                                   WSL2 distribution "shutup-and-run"
 -------                                   ----------------------------------
 shutup-and-run.exe wsl serve   --------->  /usr/local/bin/shutup-and-run serve   (the Linux musl binary)
   scheduled task at logon                    polls your URL, listens on 127.0.0.1:8787 only
   restarts the service if it exits           Docker Engine (docker.io), started by systemd in it  
   writes %ProgramData%\shutup-and-run\       /etc/shutup-and-run/config.json (600)
     supervisor.log                           containers, images, jobs.sqlite
```

The `.exe` never starts a container itself. It is a supervisor: it checks WSL, imports an Ubuntu 24.04 rootfs as a distribution of its own, installs Docker Engine and the Linux binary from the same release inside it, writes a pull-only config there, registers a logon task, and runs the doctor. Docker Desktop is not needed and not used, so there is no Docker Desktop licence on the client's machine.

The distribution mounts no Windows drive and cannot launch Windows programs (`/etc/wsl.conf`: `automount` and `interop` off). A container sees that distribution, its network, and nothing of `C:`.

## Install

WSL2 must be enabled once, as administrator, then reboot:

```powershell
wsl --install --no-distribution
```

Then, as the user who will stay logged on:

```powershell
$env:SHUTUP_AND_RUN_POLL_URL = "https://orchestrator.example/poll"
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

`install.ps1` downloads the `.exe` of the latest release, checks it against `SHA256SUMS`, puts it in `%ProgramData%\shutup-and-run`, adds that folder to the user's `PATH`, and runs `shutup-and-run wsl install`. That takes a few minutes: about 340 MiB for the rootfs, then `apt-get install docker.io` inside it. It ends with the doctor's table.

The install ends with exit 0 when everything it does on this PC worked, also when the poll URL did not answer at that minute. That is someone else's server: the doctor's `poll` line says so, a `note:` repeats it, and the worker keeps trying. The desktop shortcut is written to the folder Windows reports, accents included (`C:\Users\...\OneDrive\Área de Trabalho`).

Without the one-liner, with an `.exe` you already have:

```powershell
$env:SHUTUP_AND_RUN_POLL_TOKEN = "<token>"
shutup-and-run.exe wsl install --poll-url https://orchestrator.example/poll --max-slots 2 --worker-name DESK-07
```

| Option | |
| --- | --- |
| `--poll-url URL` | Or `SHUTUP_AND_RUN_POLL_URL`. Without it the worker is installed idle, see below. |
| `--max-slots N` | Containers at once. Default 1. |
| `--poll-interval-secs N` | Seconds between polls. Default 30, and not less: a lower number is refused. An upgrade that finds a lower value in the kept config raises it to 30. |
| `--worker-name NAME` | What the poll body calls this machine. Default: the computer name. |
| `--smoke-image IMAGE` | The image the doctor runs. Default `hello-world`. |
| `--wsl-memory 4GB`, `--wsl-processors 2` | The limits of the whole WSL2 VM. Default: 40% of the PC's RAM and half its processors. See [Sharing the PC](#sharing-the-pc). |
| `--rootfs PATH`, `--bin PATH` | Skip the downloads. For an offline machine or a mirror. |
| `--no-task` | Do not register the logon task. |

The poll token, given or made, is written to `/etc/shutup-and-run/config.json` inside the distribution, mode 600, with the rest of the config. It is never on a command line. The API token for the localhost listener is generated and written beside it. A copy of that file is kept in `%ProgramData%\shutup-and-run\config.backup.json`, which is what `wsl reset` and a later install put back. `SHUTUP_AND_RUN_URL` and `SHUTUP_AND_RUN_ROOTFS_URL` point the downloads at a mirror; `SHA256SUMS` is fetched from the same directory either way.

**Enrollment.** With no `SHUTUP_AND_RUN_POLL_TOKEN` set, the install makes a random token for this machine, or keeps the one a previous install left, and starts polling with it. Your poll URL has never seen that token, so it records the machine as pending (token, `worker`, the address of the connection, what the body says) and answers `403` or nothing until you approve it. The worker keeps asking, at most every five minutes while refused. If your orchestrator issues tokens instead, set `SHUTUP_AND_RUN_POLL_TOKEN` before the install and that one is used.

**Without a poll URL** the install does everything except give the machine work: no token is needed, nothing is polled, and the only container that runs is the doctor's smoke test. Use it to check a machine before your orchestrator exists. Run `wsl install --poll-url ...` again, with the token set, to turn polling on.

The task runs `shutup-and-run.exe wsl serve --background` when that user logs on. It may run on battery and has no time limit; the install changes those two task defaults with PowerShell and says so if it could not.

**One instance.** A second `wsl serve` while one is running prints `another shutup-and-run is already running` and exits 1. The service inside the distribution does the same. The service is up while the user is logged on. A machine that nobody logs on to after a reboot runs nothing; that is the trade for not needing administrator rights or a stored password.

## Sharing the PC

The person who owns the PC is working on it. Their work comes first, and three layers keep the worker out of the way. All three are set by the install.

| Layer | Default | Where |
| --- | --- | --- |
| The WSL2 VM | 40% of the PC's RAM, half its logical processors | `%UserProfile%\.wslconfig` |
| Each job | 1 CPU, 2 GiB of memory, no swap beyond it; one job at a time | `config.json`: `max_job_cpus`, `max_job_memory`, `max_slots` |
| Starting a job | not while the VM has under 2 GiB of memory or 5 GiB of disk free | `config.json`: `min_free_memory`, `min_free_disk` |

The figures follow the PC:

| PC | WSL2 VM | One job gets | A job starts with |
| --- | --- | --- | --- |
| 32 GiB, 16 processors | 13107 MB, 8 | 1 CPU, 2 GiB | 2 GiB free |
| 16 GiB, 8 processors | 6553 MB, 4 | 1 CPU, 2 GiB | 2 GiB free |
| 8 GiB, 4 processors | 3276 MB, 2 | 1 CPU, about 1.5 GiB | about 1.5 GiB free |
| 4 GiB, 2 processors | 2048 MB, 1 | 1 CPU, about 1 GiB | about 1 GiB free |

Below 4 GiB in the VM, the two memory figures are half of what the VM has, so a small PC is not asked for a 2 GiB job it cannot hold. A browser job on an 8 GiB PC is tight. Send such a machine lighter work.

**`.wslconfig` belongs to the user and covers every WSL2 distribution**, Docker Desktop and a developer's own Ubuntu included. So the install is careful with it:

- No file, and no other distribution on the PC: the install writes `[wsl2]`, `memory=` and `processors=`.
- No file, but other distributions exist: nothing is written. The install says what it would set and how.
- A file exists: it is never changed by the install. If it lets WSL2 take more than the budget, the install and the doctor say so.

`shutup-and-run wsl limits` shows the PC, the budget and the file. `--apply` writes the two keys under `[wsl2]`, keeps every other line, and saves the old file as `.wslconfig.bak`. `--memory 4GB --processors 2` sets other figures. `--json` is for an agent; exit 1 means the file lets WSL2 take more than the budget.

```powershell
shutup-and-run wsl limits
shutup-and-run wsl limits --apply
```

**A change to `.wslconfig` takes effect when WSL restarts**: at the next restart of the PC, or after `wsl --shutdown`. That command stops every WSL distribution of the user, so this program never runs it. Run it when no job and no other WSL work is open. Until then the doctor's `wsl-limits` line says `not in effect yet`.

The per-job limits and the two free-space checks are in `config.json` and change like any other key. `shutup-and-run config show` prints the values in force under the file, defaults included, and the local page has them under Advanced settings. `resource_profile` is `shared-pc` on every Windows worker, also on one installed before 0.2.6; its config needs no change.

```powershell
shutup-and-run config set max_job_memory 1g --apply
shutup-and-run config set min_free_disk 10g --apply
shutup-and-run status --json     # "limits" and "waiting_for"
```

A job that asks for more `cpu` or `memory` than the machine's limit is refused, and the supervisor log says `poll task rejected`. While memory or disk is short the worker does not ask for work; `status` shows `waiting_for`, and it resumes by itself.

**What this does not do.** It is not a guarantee that the PC stays responsive. The free-memory check sees the VM, not Windows: a PC whose Windows side is out of memory still looks fine from inside. It is taken when a job starts, not while it runs. Disk and network use of a job are not limited. Nothing watches the keyboard or the mouse, and a running job is never stopped to make room. The pause button on the local page is how the person at the PC says "not now".

## Certificate passthrough

Optional and off by default. The owner of the PC can let jobs use a digital certificate that is on the PC: a PKCS#12 file (`.pfx` or `.p12`) and its password.

> When enabled, every container executed by this worker can access the configured certificate and its password. Read-only mounting protects the original file from modification but does not prevent copying, exporting, or using its private key. Enable this feature only if you trust every workload that may run on this machine.

There is no list of images that may use it. That is the decision: every job, or none.

**On the local page.** `http://127.0.0.1:8787/`, Settings, Advanced settings, **Certificate**. Choose the file, type its password, tick the confirmation, **Enable certificate passthrough**. **Validate certificate** checks it, **Test PDF signing** runs a container that signs a made-up PDF with it, and the two **Disable** buttons turn it off, one of them deleting the imported copy. The password is never shown again.

**From a terminal**, with the password in a variable or on stdin, never as an argument:

```powershell
$env:SHUTUP_AND_RUN_CERT_PASSWORD = Read-Host "Certificate password"
shutup-and-run certificate set --file "C:\Users\Ana\Documents\Meu Certificado.pfx" --accept-risk --apply
Remove-Item Env:SHUTUP_AND_RUN_CERT_PASSWORD
shutup-and-run certificate status
shutup-and-run certificate validate
shutup-and-run demo sign --configured
shutup-and-run certificate disable --forget --apply
```

**How it gets from Windows into a container.** The WSL2 distribution mounts no Windows drive, so the service cannot open `C:\...` and a job cannot be handed a Windows path. Instead:

1. The `.exe` (or the browser, on the local page) reads the file you chose and hands its bytes and the password to the service inside the distribution, on stdin or in the form. Nothing goes over the network and nothing is put on a command line.
2. The service checks it with openssl (it opens with that password, it has a private key, it has not expired) and only then keeps a copy in `/etc/shutup-and-run/certificate/`, mode 600, with the password in a file of its own beside it. The original on `C:` is only read, never changed. Its path is not kept, only its file name.
3. When a job starts, the service puts copies for that one job in `/run/shutup-and-run/jobs/<id>/` (memory, not disk) and Docker mounts them read-only into the container:

| In every container | |
| --- | --- |
| `/run/shutup-and-run/certificate/client.pfx` | The certificate. The same path whatever the file was called. Also in `SHUTUP_AND_RUN_CERT_PATH`. |
| `/run/shutup-and-run/secrets/certificate-password` | The password, the only content of the file. Also in `SHUTUP_AND_RUN_CERT_PASSWORD_FILE`. |

4. When the job ends, by itself, at its timeout or because it failed to start, its copies are deleted. Copies of jobs that ended while the service was down are deleted when it starts. A restart of WSL empties `/run` anyway.

In Python, inside a job:

```python
import os
pfx = open(os.environ["SHUTUP_AND_RUN_CERT_PATH"], "rb").read()
password = open(os.environ["SHUTUP_AND_RUN_CERT_PASSWORD_FILE"]).read()
```

A task cannot ask for the certificate, move it, or mount anything: there is no such field, and its own values for those two variables are dropped. It is given to every job or to none. Enabling, disabling or replacing the certificate restarts the service and leaves running containers alone: they keep what they started with, and the next job gets the new state.

**What you are accepting.**

- Any job can copy the certificate and the password and use them elsewhere. Read-only stops a job from changing the file, nothing more.
- The imported copy and its password are plain files inside the distribution, readable by root there, and the PC's user is root there. Windows' own credential store is not used: the service that needs the password at every job start runs inside WSL, where that store does not reach.
- The copy is included in `%ProgramData%\shutup-and-run\vhd`, the distribution's disk. `certificate disable --forget` deletes the files; it does not erase their blocks from that disk. `shutup-and-run wsl uninstall` deletes the whole disk.
- `wsl reset` rebuilds the distribution and the certificate is gone with it: the install turns passthrough off and says so, and you import it again. It is not in the Windows-side config backup, on purpose.
- The password is not in `config.json`, in `status.json`, in the supervisor log, in the job history, in a callback, in `doctor --report`, or in the poll request. The orchestrator never receives it and cannot turn the feature on.
- A valid certificate here is one openssl can open and use. Whether a court's system accepts it is decided there. The signing test makes a basic PDF signature (`adbe.pkcs7.detached`); it is not PAdES and is not checked against ICP-Brasil.

## Check it

```powershell
shutup-and-run wsl doctor
```

```text
ok   wsl                    Default Version: 2
ok   distribution           shutup-and-run (Running)
ok   docker                 engine 27.5.1
ok   container              hello-world ran: Hello from Docker!
ok   binary                 /usr/local/bin/shutup-and-run
ok   config                 /etc/shutup-and-run/config.json
ok   service                answers on 127.0.0.1:8787: {"status":"ok","draining":false,...}
ok   resources              profile shared-pc; per job: cpu 1, memory 2.0 GiB; a new job needs free memory 2.0 GiB, free disk 5.0 GiB; free now: memory 5.1 GiB, disk 230.4 GiB
ok   task                   shutup-and-run is registered
ok   wsl-limits             this PC: 15.8 GiB, 8 processors; budget for WSL2: 6553MB, 4 processors; the VM now has 6.2 GiB, 4 processors
the worker is ready
```

The doctor stops at the first hard failure and exits 1. `service`, `task`, `resources` and `wsl-limits` are soft: they say the service is not running yet, the task was skipped, the machine is short of memory or disk, or `.wslconfig` lets WSL2 take more than the budget, and do not fail the doctor. `wsl install` is stricter: when it registered the task, the service must answer or the install fails.

After the Windows-side checks, the doctor runs `shutup-and-run doctor` inside the distribution: the config file, the service (from its status file), a forgotten pause, bans, and whether the poll URL's host can be reached. It sends no token anywhere. Every failed line is followed by a `fix:` line, `--json` prints the same as one object, and `--report` adds a report without secrets and a link that opens a GitHub issue.

From a Linux shell inside the distribution, the API is the same as on a server:

```powershell
wsl -d shutup-and-run -u root -- sh -c 'curl -s -H "Authorization: Bearer $(shutup-and-run config get token --reveal)" http://127.0.0.1:8787/health'
```

## Day to day

| | |
| --- | --- |
| `shutup-and-run wsl logs` | The end of `%ProgramData%\shutup-and-run\supervisor.log`: the supervisor's lines and the service's own log. `--lines 500` for more. The file rotates once at 10 MiB. |
| `shutup-and-run wsl stop` | Ends the task, stops the service, gives each running container SIGTERM and 10 seconds, then terminates the distribution. On a Linux server, stopping the service leaves containers alone; here the VM goes away with the service. |
| `shutup-and-run wsl start` | Starts the task again. |
| `shutup-and-run wsl uninstall` | Deletes the task and the distribution, with its images and job history. `--purge` also removes `%ProgramData%\shutup-and-run`. `%UserProfile%\.wslconfig` is left as it is: delete it, or its `memory=` and `processors=` lines, to give WSL2 back its defaults. |
| `shutup-and-run wsl limits` | How much of the PC WSL2 may take. See [Sharing the PC](#sharing-the-pc). |
| `wsl -d shutup-and-run -u root` | A root shell in the distribution. `docker ps`. |

To change the config, use the CLI or the local page. Nothing is edited by hand, and nothing needs a re-install:

```powershell
shutup-and-run config show
shutup-and-run config set poll_url "https://queue.example/poll?user=ana" --apply
shutup-and-run status
shutup-and-run doctor
```

`config`, `pause`, `resume` and `bans` run inside the distribution; the `.exe` forwards them. The page is `http://127.0.0.1:8787/`, and the install puts a `shutup-and-run` shortcut to it on the desktop. [FOR-AGENTS.md](FOR-AGENTS.md) lists the known problems and their fixes.

When the machine itself broke (a power cut, a damaged distribution), `shutup-and-run wsl reset` deletes the distribution and installs it again, keeping the config and the machine's poll token. `--factory` forgets those too.

To upgrade, run `install.ps1` or `wsl install` again. It replaces the `.exe` and the Linux binary and keeps the distribution, its job history and the whole config, the poll token included. Only the options you pass change. An install from 0.2.4 or earlier has its `config.toml` and env file carried over into `config.json`. A worker that was paused resumes.

A re-install replaces the running worker, and checks that it did:

- It ends the logon task and waits for the supervisor to be gone. One that is still there, a `wsl serve` left open in a terminal for instance, is ended with `taskkill`. If that does not work the install stops before it changes anything and says so.
- It stops the service inside the distribution and waits until that process is gone before it writes the new config and env.
- It makes a new API token for the localhost listener, and after starting the task it waits up to a minute for the service to answer that token. Only a service started on the new files can. If none does, the install ends with an error and not with `done`.
- It lifts a ban on `127.0.0.1` that an install before this fix may have left.

Jobs that are running when you re-install are ended: the distribution is restarted. Their callbacks are sent when the service comes back, with a non-zero or missing `exit_code`. Pause the worker from its status page and wait for it to be idle first, if that matters.

## What the poll URL gets

Each poll is a `POST` with the token and this body:

```json
{"worker": "DESK-07", "os": "windows", "arch": "x86_64", "version": "0.2.8", "max_slots": 1, "free_slots": 1, "cpus": 4, "memory_bytes": 4110753792}
```

`cpus` and `memory_bytes` are what the WSL2 VM has, which is what the machine lends. The body has no address, because the worker only knows the VM's private one and could claim any other. The server takes the public address from the connection.

**The server decides.** Check the token before answering with anything. An unknown token is a pending machine, not a caller: record it, answer `403` (or `204`), and never put a task, an image name or a secret in that answer. Store a hash of the token, not the token. To retire a machine, stop serving its token.

The answer is a task (the same JSON as `POST /v1/jobs`), `{"task": {...}}`, nothing, or `{"manifest_url": "https://..."}`. A manifest is fetched once with a `GET` and must be a task. The full contract, with schemas, is [INTEGRATION-CONTRACT.md](INTEGRATION-CONTRACT.md), and a poll URL built in n8n is [examples/n8n](../examples/n8n/README.md).

**There is no allowlist on a Windows worker.** `wsl install` writes `poll_any_image = true` and an empty allowlist: the image is whatever the manifest names. The orchestrator is the one source of work for these machines, and it is yours, so the list would only repeat what it already decides. A pushed job is still refused on this config, because the listener is on localhost and the empty allowlist matches nothing. The image name is still validated. The machine's owner is trusting your orchestrator, and that is written down in [CONVENTIONS.md](CONVENTIONS.md).

## For the person at the PC

**No window.** The worker runs with no console or terminal window, at logon and after `wsl start`. To check, with no terminal of your own open on the worker:

```powershell
Get-Process wsl, shutup-and-run, conhost, WindowsTerminal, OpenConsole -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Format-Table Name, Id, MainWindowTitle
```

The terminal you type that in is listed; nothing titled `wsl.exe` or `shutup-and-run` should be.

**Status page.** `http://127.0.0.1:8787/` in a browser on the PC shows whether the worker is running, how many jobs are running, whether the PC can be turned off, the last job, and the version. One button pauses or resumes taking new work. It opens only on that PC and shows no token. Ask for a screenshot of it when someone needs support.

**Status file.** The same facts are in `C:\ProgramData\shutup-and-run\status.json`, written by the supervisor, so a script can read it without HTTP and without an administrator. `state` is `idle`, `active`, `draining`, `failed`, or `stopped`. If `updated_at` is more than 60 seconds ago, the worker is dead. After `wsl stop` the state is `stopped`.

```powershell
(Get-Content C:\ProgramData\shutup-and-run\status.json -Raw | ConvertFrom-Json).state
```

`scripts/shutup-and-run.bat` is a desktop shortcut for the machine's user. Double-clicked, it starts the worker if it is stopped and answers two questions in two lines: is it running, and can the PC be turned off. With a job running it says not to, and `P` stops the worker with the 10 second grace first. It reads nothing but the process list and `docker ps` inside the distribution.

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/shutup-and-run.bat -OutFile "$env:USERPROFILE\Desktop\shutup-and-run.bat"
```

A full hand test of a Windows PC and a Linux server is in [TESTING.md](TESTING.md).

## Shutdown, restart, sign-out

Windows ends the WSL2 VM when the user signs out or the machine shuts down. The supervisor hears that and stops the service, then gives each running container SIGTERM and **three seconds** before it is killed; Windows allows about five in all. `wsl stop` gives ten. A job that needs longer is cut. Nothing is left behind: a container is never restarted, and when the service next starts it closes the job's row, removes the container, and sends the callback. Such a job has `reason` `exited` with a non-zero `exit_code` (usually 137 or 255), or no `exit_code` if it was still pulling its image. The orchestrator offers the work again under a new `job_key`. Tasks should be safe to run twice.

## The machine is not yours

Treat a Windows worker as a computer you do not control. Its user is administrator of the VM: they can read the poll token, every task's `env` (`docker inspect`), the image layers, and what the container writes. So:

- One token per machine, and the server knows which machine it is.
- A task sent there carries only secrets you can afford that client to see: short-lived, scoped to that one task.
- What comes back (callback, results, exit code) is a claim by that machine. Check it where it matters.
- Keep work that needs real secrets on your own Linux machines. The queue behind the poll URL makes that choice, using what the poll body says.

## When the .exe cannot run

A machine without virtualization cannot start WSL2, so `shutup-and-run.exe` cannot run. [`scripts/windows/sar-launcher.bat`](../scripts/windows/sar-launcher.bat) is the fallback: a `cmd` script you edit and start yourself. It is not the product. It does not poll, it does not take pushes, and what it runs is a Python process, not a container. The allowlist, slots, timeouts, and callbacks of the binary do not apply.

It downloads `MAIN_SCRIPT` and the names in `HELPERS`, then runs the script with `py -3`, or with `python` if `py` is absent. Each URL is `BASE_URL`, a slash, the file name, then `URL_SUFFIX`. The sample config fetches `https://www.domain.example/files/smoke_test.py/0.0/0`. A file newer than its TTL stays as it is. A failed download is retried, then the previous local copy is used. If the main script is missing even locally, the launcher exits 1. A helper that fails is a warning. The script sees `RUN_MODE` and `BOT_TASK` in the environment. Its exit code is the launcher's exit code.

| Variable | |
| --- | --- |
| `RUN_MODE`, `BOT_TASK` | Passed through to the Python process. `BOT_TASK` is also the lock file name. |
| `BASE_DIR` | Directory for the scripts and the lock. Created if missing. |
| `BASE_URL` | No trailing slash. HTTPS, on a host you control. |
| `URL_SUFFIX` | Appended after the file name. Include the leading slash when the suffix is a path. |
| `MAIN_SCRIPT` | `.py` or `.pyc`. |
| `HELPERS`, `HELPER_EXT` | Space-separated helper names, same URL pattern. Extension `py` or `pyc`. |
| `TTL_MAIN`, `TTL_HELPER` | Minutes to keep a cached file before downloading it again. |
| `LOCK_TTL` | Minutes. A newer lock means the task is already running (exit 2). An older lock is stale and is taken. A killed window leaves the lock until then. The file does not store a process id. |
| `RETRIES`, `TIMEOUT_SEC` | Download attempts, and the timeout of each attempt. The wait between tries is 2 seconds times the attempt number. |

The lock is deleted when the launcher exits after taking it: Python missing, main script missing, or the script finished. It is not deleted when another run still holds it. The check is the file's last write time, so a stale lock is detectable without reading the file.

**Security.** The file downloads those URLs and runs the bytes. There is no checksum and no allowlist. Anyone who can change the files at that URL, or redirect it, runs code on the PC. Use HTTPS and a host you control. Do not point it at a URL you do not administer.

Prefer the `.exe` whenever WSL2 works.

## Limits

- Resource limits (`cpu`, `memory`, `shm`) and the free-memory check apply inside the WSL2 VM. The VM itself takes up to half the machine's RAM unless `.wslconfig` says otherwise. See [Sharing the PC](#sharing-the-pc) for what is and is not protected.
- The service runs only while the installing user is logged on. Lock the screen, do not sign out.
- The `.exe` is not code signed. SmartScreen shows "Windows protected your PC" the first time; "More info", "Run anyway". The hash check in `install.ps1` is the integrity check.
- `wsl --install --no-distribution` and the reboot need an administrator once. Nothing else does.
- There is no script mode in the binary. A job of the service is a container. The `.bat` in [When the .exe cannot run](#when-the-exe-cannot-run) is a fallback for a PC that cannot run the `.exe`, and it is not that mode. [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) keeps the open questions.
- Docker inside the distribution is started by systemd, which needs WSL 0.67.6 or newer. `wsl --update` brings an older one up to date.
- Docker Desktop may be installed on the same PC. This worker has its own distribution, `shutup-and-run`, and always names it; it does not use or change `docker-desktop`, the default distribution, or Docker Desktop's engine. The one shared file is `%UserProfile%\.wslconfig`, which caps every WSL2 VM. With Docker Desktop on the PC the install does not write it; `shutup-and-run wsl limits --apply` does, when the owner agrees.
- A Windows machine without virtualization (some VMs, some corporate images) cannot run WSL2 and cannot run the `.exe`. That section is the other option.

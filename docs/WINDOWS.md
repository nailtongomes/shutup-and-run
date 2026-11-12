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
   writes %ProgramData%\shutup-and-run\       /etc/shutup-and-run/config.toml, env (600)
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

Without the one-liner, with an `.exe` you already have:

```powershell
$env:SHUTUP_AND_RUN_POLL_TOKEN = "<token>"
shutup-and-run.exe wsl install --poll-url https://orchestrator.example/poll --max-slots 2 --worker-name DESK-07
```

| Option | |
| --- | --- |
| `--poll-url URL` | Or `SHUTUP_AND_RUN_POLL_URL`. Without it the worker is installed idle, see below. |
| `--max-slots N` | Containers at once. Default 1. |
| `--poll-interval-secs N` | Default 15. |
| `--worker-name NAME` | What the poll body calls this machine. Default: the computer name. |
| `--smoke-image IMAGE` | The image the doctor runs. Default `hello-world`. |
| `--wsl-memory 4GB`, `--wsl-processors 2` | Written to `%UserProfile%\.wslconfig` when that file does not exist. It caps the whole WSL2 VM, which is the right place to cap what the lent machine gives up. |
| `--rootfs PATH`, `--bin PATH` | Skip the downloads. For an offline machine or a mirror. |
| `--no-task` | Do not register the logon task. |

The poll token, given or made, is written only to `/etc/shutup-and-run/env` inside the distribution, mode 600. It is never on a command line. The API token for the localhost listener is generated and written beside it. `SHUTUP_AND_RUN_URL` and `SHUTUP_AND_RUN_ROOTFS_URL` point the downloads at a mirror; `SHA256SUMS` is fetched from the same directory either way.

**Enrollment.** With no `SHUTUP_AND_RUN_POLL_TOKEN` set, the install makes a random token for this machine, or keeps the one a previous install left, and starts polling with it. Your poll URL has never seen that token, so it records the machine as pending (token, `worker`, the address of the connection, what the body says) and answers `403` or nothing until you approve it. The worker keeps asking, at most every five minutes while refused. If your orchestrator issues tokens instead, set `SHUTUP_AND_RUN_POLL_TOKEN` before the install and that one is used.

**Without a poll URL** the install does everything except give the machine work: no token is needed, nothing is polled, and the only container that runs is the doctor's smoke test. Use it to check a machine before your orchestrator exists. Run `wsl install --poll-url ...` again, with the token set, to turn polling on.

The task runs `shutup-and-run.exe wsl serve --background` when that user logs on. It may run on battery and has no time limit; the install changes those two task defaults with PowerShell and says so if it could not.

**One instance.** A second `wsl serve` while one is running prints `another shutup-and-run is already running` and exits 1. The service inside the distribution does the same. The service is up while the user is logged on. A machine that nobody logs on to after a reboot runs nothing; that is the trade for not needing administrator rights or a stored password.

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
ok   config                 /etc/shutup-and-run/config.toml
ok   service                answers on 127.0.0.1:8787: {"status":"ok","draining":false,...}
ok   task                   shutup-and-run is registered
the Windows worker is ready
```

The doctor stops at the first hard failure and exits 1. `service` and `task` are soft: they say the service is not running yet or the task was skipped, and do not fail the doctor. `wsl install` is stricter: when it registered the task, the service must answer or the install fails.

If the `service` line says the service `refuses the token`, it was started before the config was last written: `shutup-and-run wsl stop`, then `shutup-and-run wsl start`. The doctor asked once and stopped, because three wrong tokens ban an address for 48 hours. If it says this machine's own address `is banned`, run `wsl install` again with the same options, or lift it by hand: `wsl -d shutup-and-run -u root -e /usr/local/bin/shutup-and-run bans lift 127.0.0.1`. The `container` line is a real `docker run --rm` of the smoke image inside the distribution; pass `--smoke-image ghcr.io/you/worker` to run the image you will use. The container is labeled `shutup-and-run.doctor=1`, not `shutup-and-run=1`, so it never takes a slot.

From a Linux shell inside the distribution, the API is the same as on a server:

```powershell
wsl -d shutup-and-run -u root -- sh -c '. /etc/shutup-and-run/env; curl -s -H "Authorization: Bearer $SHUTUP_AND_RUN_TOKEN" http://127.0.0.1:8787/health'
```

## Day to day

| | |
| --- | --- |
| `shutup-and-run wsl logs` | The end of `%ProgramData%\shutup-and-run\supervisor.log`: the supervisor's lines and the service's own log. `--lines 500` for more. The file rotates once at 10 MiB. |
| `shutup-and-run wsl stop` | Ends the task, stops the service, gives each running container SIGTERM and 10 seconds, then terminates the distribution. On a Linux server, stopping the service leaves containers alone; here the VM goes away with the service. |
| `shutup-and-run wsl start` | Starts the task again. |
| `shutup-and-run wsl uninstall` | Deletes the task and the distribution, with its images and job history. `--purge` also removes `%ProgramData%\shutup-and-run`. |
| `wsl -d shutup-and-run -u root` | A root shell in the distribution. `docker ps`, `cat /etc/shutup-and-run/config.toml`. |

To change the config, edit it inside the distribution and restart:

```powershell
wsl -d shutup-and-run -u root -- nano /etc/shutup-and-run/config.toml
shutup-and-run wsl stop
shutup-and-run wsl start
```

To upgrade, or to change an option, run `install.ps1` or `wsl install` again. It replaces the `.exe` and the Linux binary, keeps the distribution, its job history and the machine's poll token, and rewrites the config from the options you pass. Pass all of them again: an option left out goes back to its default.

A re-install replaces the running worker, and checks that it did:

- It ends the logon task and waits for the supervisor to be gone. One that is still there, a `wsl serve` left open in a terminal for instance, is ended with `taskkill`. If that does not work the install stops before it changes anything and says so.
- It stops the service inside the distribution and waits until that process is gone before it writes the new config and env.
- It makes a new API token for the localhost listener, and after starting the task it waits up to a minute for the service to answer that token. Only a service started on the new files can. If none does, the install ends with an error and not with `done`.
- It lifts a ban on `127.0.0.1` that an install before this fix may have left.

Jobs that are running when you re-install are ended: the distribution is restarted. Their callbacks are sent when the service comes back, with a non-zero or missing `exit_code`. Pause the worker from its status page and wait for it to be idle first, if that matters.

## What the poll URL gets

Each poll is a `POST` with the token and this body:

```json
{"worker": "DESK-07", "os": "windows", "arch": "x86_64", "version": "0.2.0", "max_slots": 1, "free_slots": 1, "cpus": 4, "memory_bytes": 4110753792}
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

- Resource limits (`cpu`, `memory`, `shm`) apply inside the WSL2 VM. The VM itself takes up to half the machine's RAM unless `.wslconfig` says otherwise. Use `--wsl-memory` and `--wsl-processors` on a machine someone lends.
- The service runs only while the installing user is logged on. Lock the screen, do not sign out.
- The `.exe` is not code signed. SmartScreen shows "Windows protected your PC" the first time; "More info", "Run anyway". The hash check in `install.ps1` is the integrity check.
- `wsl --install --no-distribution` and the reboot need an administrator once. Nothing else does.
- There is no script mode in the binary. A job of the service is a container. The `.bat` in [When the .exe cannot run](#when-the-exe-cannot-run) is a fallback for a PC that cannot run the `.exe`, and it is not that mode. [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) keeps the open questions.
- Docker inside the distribution is started by systemd, which needs WSL 0.67.6 or newer. `wsl --update` brings an older one up to date.
- Docker Desktop may be installed on the same PC. This worker has its own distribution, `shutup-and-run`, and always names it; it does not use or change `docker-desktop`, the default distribution, or Docker Desktop's engine. The one shared file is `%UserProfile%\.wslconfig`, which caps every WSL2 VM and is only written when it does not exist.
- A Windows machine without virtualization (some VMs, some corporate images) cannot run WSL2 and cannot run the `.exe`. That section is the other option.

# Windows 11

The Windows worker is the same product in pull mode. A Windows 11 machine, usually one a client lends, asks your orchestrator for work and runs it as a container inside WSL2. Nothing is pushed to it, and no port is opened on it.

Tested target: Windows 11 Pro 25H2 with WSL2. Windows 10 is not a target.

## What is on the machine

```text
 Windows                                   WSL2 distribution "shutup-and-run"
 -------                                   ----------------------------------
 shutup-and-run.exe wsl serve   --------->  /usr/local/bin/shutup-and-run serve   (the Linux musl binary)
   scheduled task at logon                    polls your URL, listens on 127.0.0.1:8787 only
   restarts the service if it exits           Docker Engine (docker.io), started with the distribution
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

Then, as the user who will stay logged on, with the token your orchestrator issued for this machine:

```powershell
$env:SHUTUP_AND_RUN_POLL_TOKEN = "<token>"
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

`SHUTUP_AND_RUN_POLL_TOKEN` is read from the environment and written only to `/etc/shutup-and-run/env` inside the distribution, mode 600. It is never on a command line. The API token for the localhost listener is generated and written beside it. `SHUTUP_AND_RUN_URL` and `SHUTUP_AND_RUN_ROOTFS_URL` point the downloads at a mirror; `SHA256SUMS` is fetched from the same directory either way.

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
ok   service                answers on 127.0.0.1:8787: {"ok":true,...}
ok   task                   shutup-and-run is registered
the Windows worker is ready
```

The doctor stops at the first hard failure and exits 1. `service` and `task` are soft: they say the service is not running yet or the task was skipped, and do not fail the doctor. The `container` line is a real `docker run --rm` of the smoke image inside the distribution; pass `--smoke-image ghcr.io/you/worker` to run the image you will use. The container is labeled `shutup-and-run.doctor=1`, not `shutup-and-run=1`, so it never takes a slot.

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

To upgrade, run `install.ps1` again. It replaces the `.exe`, and `wsl install` replaces the Linux binary, keeps the distribution, and rewrites the config from the options you pass.

## What the poll URL gets

Each poll is a `POST` with the token and this body:

```json
{"worker": "DESK-07", "os": "windows", "free_slots": 1}
```

The answer is a task (the same JSON as `POST /v1/jobs`), `{"task": {...}}`, nothing, or `{"manifest_url": "https://..."}`. A manifest is fetched once with a `GET` and must be a task. See [Pull](../README.md#pull).

**There is no allowlist on a Windows worker.** `wsl install` writes `poll_any_image = true` and an empty allowlist: the image is whatever the manifest names. The orchestrator is the one source of work for these machines, and it is yours, so the list would only repeat what it already decides. A pushed job is still refused on this config, because the listener is on localhost and the empty allowlist matches nothing. The image name is still validated. The machine's owner is trusting your orchestrator, and that is written down in [CONVENTIONS.md](CONVENTIONS.md).

## Shutdown, restart, sign-out

Windows ends the WSL2 VM when the user signs out or the machine shuts down. **A running container is killed without a warning then**; only `wsl stop` gives it time. Nothing is left behind: a container is never restarted, and when the service next starts it closes the job's row, removes the container, and sends the callback. Such a job has `reason` `exited` with a non-zero `exit_code` (usually 137 or 255), or no `exit_code` if it was still pulling its image. The orchestrator offers the work again under a new `job_key`. Tasks should be safe to run twice.

## Limits

- Resource limits (`cpu`, `memory`, `shm`) apply inside the WSL2 VM. The VM itself takes up to half the machine's RAM unless `.wslconfig` says otherwise. Use `--wsl-memory` and `--wsl-processors` on a machine someone lends.
- The service runs only while the installing user is logged on. Lock the screen, do not sign out.
- The `.exe` is not code signed. SmartScreen shows "Windows protected your PC" the first time; "More info", "Run anyway". The hash check in `install.ps1` is the integrity check.
- `wsl --install --no-distribution` and the reboot need an administrator once. Nothing else does.
- There is no script mode. A job is a container. [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) keeps the idea.
- A Windows machine without virtualization (some VMs, some corporate images) cannot run WSL2 and cannot run this.

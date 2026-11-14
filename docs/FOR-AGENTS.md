# For agents

You are an AI agent setting up or repairing a shutup-and-run worker on someone's machine. Everything here is non-interactive. No command asks a question, and you never edit a file by hand.

## Rules of the CLI

- Add `--json` to `config`, `status`, `doctor`, `pause` and `resume`: one JSON object on stdout, `"ok": true|false`, and `"error"` when false.
- Exit codes: `0` done. `1` it failed, or the doctor found a problem. `2` wrong command line. `3` invalid config or value; nothing was written.
- `shutup-and-run config --help` documents every command. `shutup-and-run config keys` lists every key.
- The config is one file, `config.json` (schema: [schemas/worker-config.schema.json](schemas/worker-config.schema.json)). On Windows it is inside the WSL distribution; the same commands reach it. Change it only with `shutup-and-run config`.
- Secrets (`token`, `poll_token`, `admin_password`) print as `***`. Do not put them in a report or an issue.

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

The poll URL is the setting that changes most. Its `?user=` filter is part of the URL and is not a secret.

```
shutup-and-run config set poll_url "https://QUEUE/poll?user=NAME" --apply --json
shutup-and-run config get poll_url
```

`--apply` restarts the service on the new file and waits until it is back. Running containers are not stopped. Other everyday keys: `max_slots`, `poll_interval_secs`, `worker_name`. `poll_interval_secs` is 30 by default and cannot be set lower: 1 to 29 exits 3 and nothing is written. An invalid value exits 3 with the reason and leaves the file as it was.

## Quick start: prove the machine works

After the install, and before any orchestrator sends work:

```
shutup-and-run version --json                 # which build, platform and runtime you are talking to
shutup-and-run config check --json            # exit 0 ready; 3 the config is invalid; 1 something around it
shutup-and-run demo --json                    # one container under the worker's rules; exit 0 when it passed
shutup-and-run job validate task.json --json  # exit 3 and a code when this machine would refuse the task
```

Each prints `ok`, a top-level `code`, and `checks[]` with `name`, `code`, `status`, `detail`, `fix`. Act on `code`, show `detail` to a person, run `fix`. None of these changes the config or the job history. `demo` runs `hello-world`; with another image use `--image`, and if the answer is `DEMO_IMAGE_NOT_ALLOWED` its `fix` is the command that allows it. `job validate` is the static half of admission only: a `JOB_VALID` task can still meet a full or paused machine later.

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
| `POLL_ERROR` | The poll URL answered with an error | Check the URL and its filter |
| `WORKER_PAUSED` | Someone paused the worker | `shutup-and-run resume` |
| `LOCAL_ADDRESS_BANNED`, `ADDRESS_BANNED` | Wrong tokens banned an address | `shutup-and-run bans lift IP` |
| `WSL_UNAVAILABLE`, `DISTRIBUTION_MISSING`, `TASK_NOT_REGISTERED` | Windows: WSL, the distribution or the logon task is missing | `wsl --install --no-distribution`; `shutup-and-run wsl install`; `wsl task register` |
| `WSL_LIMITS_ABOVE_BUDGET`, `WSL_LIMITS_NOT_IN_EFFECT` | `.wslconfig` lets WSL2 take too much, or WSL has not restarted | `shutup-and-run wsl limits --apply`; a restart of WSL |
| `CERTIFICATE_MISSING`, `CERTIFICATE_INVALID`, `CERTIFICATE_PASSWORD_WRONG`, `CERTIFICATE_EXPIRED`, `CERTIFICATE_NO_PRIVATE_KEY` | Passthrough is on and the certificate cannot be used | `shutup-and-run certificate set --file PATH --accept-risk` with a good file, or `certificate disable` |
| `OPENSSL_MISSING` | The openssl program is not installed where the service runs | `apt-get install -y openssl` |
| `DEMO_IMAGE_NOT_ALLOWED`, `DEMO_IMAGE_UNAVAILABLE`, `DEMO_JOB_FAILED`, `DEMO_TIMEOUT`, `WORKER_BUSY`, `LIMITS_NOT_APPLIED`, `DEMO_CLEANUP_FAILED` | `demo`: which step failed | The check's `fix` |
| `SIGNATURE_INVALID`, `CERTIFICATE_MOUNT_WRITABLE` | `demo sign`: the PDF did not verify, or the mount was not read-only | `doctor --report` and open the issue |
| `JOB_VALID`, `JOB_INVALID_JSON`, `JOB_INVALID_FIELD`, `JOB_IMAGE_NOT_ALLOWED` | `job validate` | Fix the task, or the allowlist |

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
shutup-and-run doctor --json     # checks[], each with status ok|warn|fail, detail, fix
```

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

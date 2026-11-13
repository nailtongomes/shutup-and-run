# shutup-and-run

[Português](README.pt-BR.md)

A small, self-hosted Docker job runner, in Rust, for the spare capacity of computers people already own.

A customer has PCs that sit mostly idle. Their automations (RPA, Selenium, scripts in a container) need somewhere to run. shutup-and-run turns such a PC, Windows 11 or Linux, into a worker: an orchestrator you already use, such as n8n or Kestra, decides what runs and where; this program starts the container on the customer's own machine, limits it, times it out, and reports how it ended.

It is built for a **shared PC**. Someone may be working on the machine while a job runs, and their work comes first: one job at a time, a CPU and a memory limit on every container, and no new job while memory or disk is short.

It is also built to be run by the customer's own **AI agent**. Every command is non-interactive, prints JSON, and has a fixed exit code. The config is one file that is never edited by hand. A doctor names each problem with the command that fixes it.

## What it is, and is not

| It does | It does not |
| --- | --- |
| Start an allowlisted container image with the env, command, timeout and limits a task names | Queue, schedule, retry or chain tasks. Your orchestrator does that |
| Take work two ways: **push** (`POST /v1/jobs`) and **pull** (the worker polls a URL of yours) | Coordinate machines. Each worker knows only itself |
| Cap concurrency (`max_slots`), time (`timeout_secs`, then SIGKILL), CPU and memory per job | Sandbox the job. A container has Docker's default privileges and network |
| Refuse work when full, draining, or short of memory or disk | Mount host folders, publish ports, or run privileged. There are no such fields |
| Keep a job history in SQLite and call a webhook when a job ends | Return a job's output. A job reports an exit code; its data is sent by the container |
| Show a local page with status and settings, on the machine only | Offer a dashboard, or any page reachable from the network |

## How it works

```text
            your orchestrator (n8n, Kestra, your own API)
            ---------------------------------------------
              keeps the queue, picks the machine, gets the callback
                 |                                   ^         ^
   push: POST /v1/jobs                  pull: POST poll_url    | callback_url
   202 accepted / 429 full / 503        "any work for me?"     | when a job ends
                 v                                   |         |
   Linux server (yours, trusted)        Windows 11 PC (a customer's, shared)
   -----------------------------        ------------------------------------
   shutup-and-run.service               shutup-and-run.exe: supervisor, logon task
   HTTPS :8787, bearer token              WSL2 distribution "shutup-and-run"
   Docker Engine                            the same Linux binary, in pull mode
                                            Docker Engine (no Docker Desktop)
                                            local page on 127.0.0.1:8787
```

- **Linux** runs the service under systemd. It usually takes pushes over HTTPS. It can also pull, for a machine behind NAT.
- **Windows 11** runs the same Linux binary inside a WSL2 distribution of its own, in pull mode only. No port is opened on the PC and nothing is pushed to it. The `.exe` is a supervisor: it installs the distribution, keeps the service running while the user is logged on, and never shows a window. Details: [docs/WINDOWS.md](docs/WINDOWS.md).
- **A job** is one container. `202` means the machine took it and holds a slot; the image is pulled afterwards. When it ends, by itself, at its timeout, or because it could not start, the result is POSTed to the task's `callback_url`.
- **The contract** for an orchestrator, with JSON Schemas, is [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md). A complete n8n example, with worker approval, is in [examples/n8n](examples/n8n/README.md).

## Quick start

### Windows 11 PC, pull mode

WSL2 has to be enabled once, as administrator, followed by a reboot: `wsl --install --no-distribution`. Then, in PowerShell, as the user who stays logged on:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

```powershell
shutup-and-run wsl install --poll-url "https://orchestrator.example/poll?user=ana"
```

The first line downloads the `.exe`, checks its SHA-256 and puts it on the user's `PATH` (open a new terminal if `shutup-and-run` is not found). The second takes a few minutes and ends with the doctor. It is safe to run again. Leave `--poll-url` out to install a machine idle and check it first.

```powershell
shutup-and-run doctor
```

The worker now polls your URL with a token of its own. Your orchestrator sees an unknown token and approves the machine or not: [enrollment](docs/WINDOWS.md#install). The person at the PC has a desktop shortcut to the local page, `http://127.0.0.1:8787/`.

### Linux server, push mode

Ubuntu with a public address:

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
```

It installs Docker if needed, starts the service on port 8787 over HTTPS with a self-signed certificate, and prints the URL and the certificate fingerprint. Read the token, then send a job from anywhere:

```bash
sudo sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' /etc/shutup-and-run/env
```

```bash
curl -sk -X POST https://203.0.113.10:8787/v1/jobs -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"image":"ghcr.io/you/worker","timeout_secs":600,"job_key":"hello-1","env":{"JOB_NAME":"hello"}}'
```

```bash
curl -sk -H "Authorization: Bearer $SAR_TOKEN" "https://203.0.113.10:8787/v1/jobs/PASTE_ID?logs=true"
```

`-k` skips certificate verification; pinning the fingerprint is in [docs/RECIPES.md](docs/RECIPES.md). Ready-made flows: [examples/kestra-dispatch.yml](examples/kestra-dispatch.yml), [examples/n8n-dispatch.json](examples/n8n-dispatch.json), and for pull [examples/kestra-poll.yml](examples/kestra-poll.yml), [examples/n8n-poll.json](examples/n8n-poll.json).

## Protecting the PC

The customer's interactive work has priority over job throughput. On a shared PC these layers enforce that:

| Layer | `shared-pc` default | Set with |
| --- | --- | --- |
| The WSL2 VM (Windows only) | about 40% of the RAM, half the logical processors | `%UserProfile%\.wslconfig`, through `shutup-and-run wsl limits` |
| Each job | 1 CPU, 2 GiB of memory, no swap beyond it | `max_job_cpus`, `max_job_memory` |
| Jobs at once | 1 | `max_slots` |
| Starting a job | only with 2 GiB of memory and 5 GiB of disk free | `min_free_memory`, `min_free_disk` |

- The limits are enforced by Docker (`NanoCpus`, `Memory`, `MemorySwap`), on a pushed job and on a pulled one. A task that names no `cpu` or `memory` gets the machine's limit; one that names less gets what it asked; one that names more is refused with the limit in the answer.
- While memory or disk is short, a push is answered `503` `insufficient resources` and a pull worker simply does not ask for work, so no task is claimed and then dropped. It resumes by itself. `status` shows `waiting_for`.
- Where the service sees under 4 GiB (an 8 GiB PC gives WSL2 about 3.2 GiB), the two memory figures are half of what it sees. Not every PC can run a 2 GiB job.
- The `shared-pc` profile is the default on Windows, also for a worker installed before 0.2.6. On a Linux desktop: `shutup-and-run config set resource_profile shared-pc --apply`. A Linux server is unchanged unless you set it.
- `.wslconfig` belongs to the user and covers every WSL2 distribution. The install writes it only when it does not exist and the PC has no other distribution. An existing file is changed only by `shutup-and-run wsl limits --apply`, which keeps every other line and a backup. A change takes effect when WSL restarts; this program never runs `wsl --shutdown`.

```
shutup-and-run status --json          # "limits" in force, and "waiting_for"
shutup-and-run doctor                 # the "resources" line; on Windows also "wsl-limits"
shutup-and-run config set max_job_memory 1g --apply
shutup-and-run wsl limits             # Windows: the PC, the budget, what .wslconfig says
```

**This reduces the risk; it does not remove it.** Free memory is measured inside the WSL2 VM, which is not the same as free memory on the Windows host, and only at the moment a job starts. Disk and network use of a job are not limited. There is no idle detection, no monitoring daemon, and a running job is never stopped to make room. A PC can still get slow. The pause button on the local page, or `shutup-and-run pause`, is how the person at the PC says "not now". More: [docs/WINDOWS.md](docs/WINDOWS.md#sharing-the-pc).

## The local page

With `status_page = true` (the Windows install turns it on; a Linux install does not), the service answers a browser **on the machine itself** at `http://127.0.0.1:8787/`, the address in `listen`. On Windows the install puts a `shutup-and-run` shortcut to it on the desktop.

| Page | What is there | Login |
| --- | --- | --- |
| `/` | Running, paused or Docker down. Whether the PC can be turned off. Jobs now, the last job, whether the queue answers, the machine's name. A button to pause or resume taking jobs. Reloads every 10 seconds | none |
| Settings (`/local/config`) | Poll URL, poll interval, jobs at once, machine name | none |
| Advanced settings (`/local/advanced`) | Allowed images, any-image, time limits, both tokens, the admin login | `admin` / `admin` until changed |

- The page edits the same `config.json` as the CLI, through the same validation. A saved change restarts the service on the new file; running containers are left alone. An invalid value is refused and nothing is written.
- Change the admin password with `shutup-and-run config set admin_password "NEW" --apply`.
- The resource limits and the other keys are CLI only. `shutup-and-run config keys` says where each key can be edited.
- It is in English or Portuguese, one at a time; a link switches and the choice is remembered.

Security of the page, plainly. It needs no token, so it answers only a loopback connection that names a loopback host, and accepts a form only from its own origin. It never shows a token, a job's env or logs. The admin login keeps a curious person out and is **not** a security boundary: whoever uses the PC can read the config file. Nothing on it is reachable from the network, and it is not meant to be put behind a proxy. On a machine set up with `config.toml` the page shows status only.

## Configuration

A machine has one config source.

**`config.json`** holds every setting, tokens included, mode 600, with no environment on top. It is `/etc/shutup-and-run/config.json`; on Windows that path is inside the WSL distribution, and the same commands reach it. The Windows install writes it. It changes only through the CLI or the local page. Schema: [docs/schemas/worker-config.schema.json](docs/schemas/worker-config.schema.json).

```
shutup-and-run config show                      # the whole file, secrets as ***
shutup-and-run config keys                      # every key, its type, where it is edited
shutup-and-run config get max_slots
shutup-and-run config set poll_url "https://queue.example/poll?user=ana" --apply
shutup-and-run config unset max_job_memory --apply
shutup-and-run config validate                  # exit 3 and the reason when invalid
shutup-and-run config apply                     # restart on the file; containers are left alone
shutup-and-run config reset                     # factory config; tokens, name and address kept
shutup-and-run status                           # idle, active, draining, failed, stopped
shutup-and-run doctor [--report]                # every failed line has a fix
shutup-and-run pause | resume
```

All are non-interactive and take `--json`. Exit codes: `0` done, `1` failed or unhealthy, `2` wrong command line, `3` invalid config or value (nothing written). An invalid value is never saved, and a file damaged some other way does not stop the service: it runs on the copy from the last save, or on idle defaults, and says so.

**`config.toml` plus environment** is what the Linux installer still writes: `/etc/shutup-and-run/config.toml`, or the file named by `SHUTUP_AND_RUN_CONFIG`. Environment variables override the file. After a change: `sudo systemctl restart shutup-and-run`; running jobs survive. On such a machine `shutup-and-run config` refuses and says so; `status`, `doctor`, `pause`, `resume` and `bans` work. A Linux machine uses `config.json` instead when that file exists and `SHUTUP_AND_RUN_CONFIG` does not name a TOML file. There is no converter.

The keys are the same in both. An unknown key is refused.

| Key | Environment | Meaning |
| --- | --- | --- |
| `listen` | `SHUTUP_AND_RUN_LISTEN` | `ip:port`. Default `127.0.0.1:8787`. The Linux installer writes `0.0.0.0:8787`. |
| `token` | `SHUTUP_AND_RUN_TOKEN` | Required. At least 16 characters. The Linux installer keeps it in `/etc/shutup-and-run/env`. |
| `max_slots` | `SHUTUP_AND_RUN_MAX_SLOTS` | Required. 1 to 256. Windows installs 1. The Linux installer suggests one per CPU and 2 GiB. |
| `allowlist` | `SHUTUP_AND_RUN_ALLOWLIST` | Images that may run. Required unless `poll_any_image`. Comma-separated in the environment. |
| `resource_profile` | `SHUTUP_AND_RUN_RESOURCE_PROFILE` | `shared-pc` or `none`. Default `shared-pc` when `worker_os` is `windows`, otherwise `none`. See [Protecting the PC](#protecting-the-pc). |
| `max_job_cpus` | `SHUTUP_AND_RUN_MAX_JOB_CPUS` | Most CPU per job, and the default: `"1"`, `"0.5"`. Unset under `none`: no limit. |
| `max_job_memory` | `SHUTUP_AND_RUN_MAX_JOB_MEMORY` | The same for memory: `"2g"`, `"512m"`. |
| `min_free_memory` | `SHUTUP_AND_RUN_MIN_FREE_MEMORY` | No new job below this much available memory. `"0"`: not checked. |
| `min_free_disk` | `SHUTUP_AND_RUN_MIN_FREE_DISK` | The same for the disk Docker writes to. |
| `default_timeout_secs` | `SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS` | Default 3600. |
| `max_timeout_secs` | `SHUTUP_AND_RUN_MAX_TIMEOUT_SECS` | Default 21600. At most 7 days. |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Optional. Turns pull mode on. |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Default 15. |
| `poll_token` (JSON only) | `SHUTUP_AND_RUN_POLL_TOKEN` | Who this machine is to the poll URL. Not a TOML key. |
| `poll_any_image` | `SHUTUP_AND_RUN_POLL_ANY_IMAGE` | A polled task may name any image. Needs `poll_url`. Lets `allowlist` be empty. Never applies to a push. |
| `worker_name`, `worker_os` | `SHUTUP_AND_RUN_WORKER_NAME`, `_WORKER_OS` | Sent in the poll body. Defaults: the hostname, the build OS. |
| `status_page` | `SHUTUP_AND_RUN_STATUS_PAGE` | The local page. Default false. |
| `admin_user`, `admin_password` (JSON only) | | The login of the page's advanced settings. |
| `http` | `SHUTUP_AND_RUN_HTTP` | `true` serves plaintext. For localhost only. |
| `tls_cert`, `tls_key` | `SHUTUP_AND_RUN_TLS_CERT`, `_TLS_KEY` | PEM files. Required unless `http = true`. |
| `auth_failures`, `ban_hours` | `SHUTUP_AND_RUN_AUTH_FAILURES`, `_BAN_HOURS` | Wrong tokens before a ban (3), and its length (48). |
| `trust_x_forwarded_for` | `SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR` | Only behind a proxy you control. Default false. |
| `history_path`, `history_days` | `SHUTUP_AND_RUN_HISTORY_PATH`, `_HISTORY_DAYS` | The SQLite file, and how long finished jobs are kept (30). |
| `log_lines` | `SHUTUP_AND_RUN_LOG_LINES` | Log lines kept per job. Default 200. |
| `prune_interval_secs` | `SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS` | Remove finished containers and unused images this often. Default 0, off. |
| `reaper_interval_secs`, `purge_interval_secs` | `SHUTUP_AND_RUN_REAPER_INTERVAL_SECS`, `_PURGE_INTERVAL_SECS` | How often timeouts are enforced (2) and old history deleted (3600). |
| `docker_config` | `SHUTUP_AND_RUN_DOCKER_CONFIG` | Path to Docker's `config.json`, for private images. |

**Allowlist.** `ghcr.io/you/worker` allows any tag or digest of that repository and nothing else: not `ghcr.io/you/worker-evil`, not `ghcr.io/you/other`. An entry with a tag allows exactly that string. A Windows worker has `poll_any_image` instead: its one source of work is your poll URL.

**Private images.** `sudo docker login ghcr.io -u YOU` on the machine. A task cannot supply registry credentials.

**Logs.** Linux: `journalctl -u shutup-and-run -f`. Windows: `shutup-and-run wsl logs`. The token and job environment are never logged.

## For an AI agent

[docs/FOR-AGENTS.md](docs/FOR-AGENTS.md) is the guide: install, the known problems and their fixes. The whole loop on a Windows PC, with no question asked and nothing edited by hand:

```powershell
# 1. Install. Exit 0 only when the worker runs.
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
shutup-and-run wsl install --poll-url "https://queue.example/poll?user=ana"

# 2. Configure. Exit 3 and a reason on an invalid value; nothing is written then.
shutup-and-run config set max_job_memory 1g --apply --json

# 3. Inspect what is in force.
shutup-and-run status --json        # state, slots, limits, waiting_for, poll_error
shutup-and-run wsl limits --json    # the PC, the WSL2 budget, .wslconfig

# 4. Diagnose. checks[] has status ok|warn|fail, detail, and fix for each problem.
shutup-and-run doctor --json

# 5. Repair with the command in "fix", then run the doctor again. Still stuck:
shutup-and-run doctor --report      # a report with secrets masked, and an issue link

# 6. Factory reset, from mild to complete.
shutup-and-run config reset         # the config only; tokens kept
shutup-and-run wsl reset            # rebuild the distribution; config kept
shutup-and-run wsl reset --factory  # forget the config too; needs approval again
```

On Linux the same `status`, `doctor`, `pause`, `resume` and `config` commands run as root. Secrets print as `***` everywhere, `doctor --report` included.

## Push, pull and the API

Every route needs `Authorization: Bearer <token>`. The one exception is the local page.

| Route | |
| --- | --- |
| `POST /v1/jobs` | Start a job. `202` accepted, `200` this `job_key` is already here, `429` full, `503` draining or short of memory or disk, `400` invalid or above the machine's limits, `403` image not allowed. |
| `GET /v1/jobs/{id}` | Status, exit code, reason. `?logs=true` adds the last log lines. |
| `GET /health` | `{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}` |
| `GET /metrics` | Prometheus text. |
| `POST /v1/drain` | `{"draining": true}` refuses new jobs and lets running ones finish. |

A task, the same JSON on a push, in a poll answer, and in a manifest:

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

Only `image` is required. Unknown fields are refused. `job_key` makes a retry safe on that one machine: the same key returns the same job and starts nothing. `reason` in the result is `exited`, `timeout_kill` or `start_failed`. There is no cancel call.

**Pull.** With `poll_url` set, the machine POSTs to it every `poll_interval_secs`, only when it has a free slot, is not paused and is not short of memory or disk:

```json
{"worker": "DESK-07", "os": "windows", "arch": "x86_64", "version": "0.2.6", "max_slots": 1, "free_slots": 1, "cpus": 4, "memory_bytes": 3435973836}
```

The answer is nothing (`204`), a task, `{"task": {...}}`, or `{"manifest_url": "https://..."}`, which is fetched once and must be a task. `401` or `403` means "not approved yet": the worker starts nothing and keeps asking, backing off to five minutes. The server decides which machines get work; the worker's token is its identity.

Field limits, every status code, the callback, idempotency, and what is not guaranteed: [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md).

**`status.json`** has the same facts as the local page, for a script that should not call HTTP: `state` (`idle`, `active`, `draining`, `failed`, `stopped`), `slots_total`, `slots_used`, `job_ids`, `last_job_at`, `last_error`, `poll_error`, `waiting_for`, `limits`, `config_warning`, `version`, `pid`, `started_at`, `updated_at`. Linux: beside the history file, `/var/lib/shutup-and-run/status.json`, root only. Windows: `C:\ProgramData\shutup-and-run\status.json`, readable by the user. If `updated_at` is more than 60 seconds old, the worker is dead. `shutup-and-run status --json` reads it for you.

## Security

- **The API token is a root password for the machine.** Whoever holds it can run any allowlisted image with any environment. Every route needs it, `/health` and `/metrics` included.
- Three wrong tokens from one address ban it for 48 hours; a banned address gets `403` even with the right token. `sudo shutup-and-run bans`, `sudo shutup-and-run bans lift 203.0.113.5`.
- A Linux install speaks HTTPS with a self-signed certificate. Pin the fingerprint it prints, and firewall the port to your orchestrator.
- A job cannot mount the host, publish a port or run privileged. It is still not a sandbox.
- **A Windows worker is a computer you do not control.** Its user can read the poll token, every task's env and the image layers. Send it only short-lived secrets scoped to one task, and treat what comes back as a claim. Keep work with real secrets on your own Linux machines.
- **The customer is trusting your orchestrator.** With `poll_any_image`, the poll URL decides which image runs on their PC.
- The local page is for the person at the machine and is not a security boundary. See [The local page](#the-local-page).

How the process defends itself: [SECURITY.md](SECURITY.md). What an operator accepts by running it: [docs/CONVENTIONS.md](docs/CONVENTIONS.md).

## Installers

**Linux**, `scripts/install.sh`:

| Variable | |
| --- | --- |
| `SHUTUP_AND_RUN_ALLOWLIST` | Images for the first `config.toml`. Without it the installer writes a placeholder and warns. |
| `SHUTUP_AND_RUN_MAX_SLOTS` | Overrides the suggested slot count. |
| `SHUTUP_AND_RUN_PORT` | Default 8787. |
| `SHUTUP_AND_RUN_NO_UFW=1` | Do not open the port in ufw. |
| `SHUTUP_AND_RUN_SHOW_TOKEN=1` | Print the token. |
| `SHUTUP_AND_RUN_BIN=./shutup-and-run` | Install a binary you already have. No download, no checksum. |

The download is checked against `SHA256SUMS` of the same release. Running it again upgrades the binary and keeps the token, certificate and config. `--uninstall` removes the service and the binary; `--uninstall --purge` also removes `/etc/shutup-and-run` and `/var/lib/shutup-and-run`. A container image, `ghcr.io/nailtongomes/shutup-and-run`, exists for people who run everything under Docker; it needs the host's Docker socket, which is root on the host. The systemd install is the supported path.

**Windows 11**, `scripts/install.ps1`, then `shutup-and-run wsl install`:

| Command | |
| --- | --- |
| `wsl install [--poll-url URL] [--max-slots N] [--worker-name NAME] [--wsl-memory 4GB] [--wsl-processors 2]` | Install or upgrade. Keeps the config; changes only the options given. |
| `wsl doctor`, `doctor` | Check everything, with a fix for each problem. |
| `wsl limits [--apply]` | What WSL2 may take of the PC. |
| `wsl start`, `wsl stop`, `wsl logs` | The logon task and the supervisor log. |
| `wsl reset [--factory]` | Rebuild a broken distribution. |
| `wsl uninstall [--purge]` | Remove the task and the distribution. |

No Docker Desktop and no administrator, apart from enabling WSL2 once. The worker runs while the installing user is logged on. Everything else is in [docs/WINDOWS.md](docs/WINDOWS.md). A PC that cannot run WSL2 cannot run the `.exe`; the [shell fallback](docs/WINDOWS.md#when-the-exe-cannot-run) is a different, weaker thing.

## Testing

`scripts/smoke.sh` checks a running instance through its API, twelve scenarios, safe on a live machine. On the machine, as root:

```bash
sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker SMOKE_RESTART_CMD="systemctl restart shutup-and-run" bash scripts/smoke.sh
```

From another machine:

```bash
SMOKE_URL=https://203.0.113.10:8787 SMOKE_TOKEN=... SMOKE_FINGERPRINT=AA:BB:... SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh
```

The image needs `sh` and `sleep`; the default is `ghcr.io/nailtongomes/shutup-and-run-smoke`, built from `examples/smoke-image`. A check that needs something you did not give is `SKIP`, with the reason. The header of the script lists every variable.

`scripts/accept.sh` is the developer's test: its own instance against a real Docker daemon, the per-job limits on the container included. CI runs it and `smoke.sh` on every push, a pull cycle against `tests/poll-mock`, and `smoke.sh` again against a service installed by `install.sh`. A hand test of a Windows PC and a Linux server, step by step, is [docs/TESTING.md](docs/TESTING.md).

## Limits

- One process per Docker daemon. Two can exceed `max_slots`.
- On Linux, stopping the service does not stop containers, and while it is stopped nothing enforces timeouts.
- A timeout is SIGKILL, with no grace period.
- `job_key` is per machine. Another machine does not know it.
- No cancel call, no queue, no retry of a job that failed.
- In pull mode the server is never told that a task was refused or dropped; the worker's own log is. See [the contract's limits](docs/INTEGRATION-CONTRACT.md#9-limits-of-the-current-contract).
- Resource protection is admission control and container limits, not a guarantee. See [Protecting the PC](#protecting-the-pc).
- Windows runs the Linux service inside WSL2, pull mode only, while the installing user is logged on. Stopping it, signing out or shutting down ends the containers. macOS is planned: [docs/ROADMAP-windows-macos.md](docs/ROADMAP-windows-macos.md).

## Similar projects

This one is a single process on a machine you already have, and your orchestrator chooses the machine.

- **Nomad** schedules jobs across a cluster. Use it when one control plane should place the work.
- **HTCondor** matchmakes high-throughput jobs across a pool.
- **GitLab Runner** and the **GitHub Actions self-hosted runner** run their own CI systems' jobs.
- **Tork** is a workflow engine with a coordinator, a queue and workers. Use it when you want that queue and retries.
- **BOINC** is volunteer computing: strangers run a published workload. Here, your images run on machines you set up.

## Development

```bash
cargo fmt --all -- --check
```

```bash
cargo clippy --all-targets --locked -- -D warnings
```

```bash
cargo test --locked
```

The static binary that releases ship:

```bash
rustup target add x86_64-unknown-linux-musl && sudo apt-get install -y musl-tools build-essential && CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl
```

A release is a `v*` tag pushed from `main`. It attaches the Linux binary, the Windows `.exe` and `SHA256SUMS`, publishes the image, then runs the one-line install and the smoke test on a fresh runner.

MIT license. More reading:

- [docs/INTEGRATION-CONTRACT.md](docs/INTEGRATION-CONTRACT.md): the contract for an orchestrator, with JSON Schemas in `docs/schemas`.
- [docs/WINDOWS.md](docs/WINDOWS.md): the Windows worker, and sharing the PC.
- [docs/FOR-AGENTS.md](docs/FOR-AGENTS.md): for an AI agent setting up or repairing a worker.
- [examples/n8n](examples/n8n/README.md): n8n as the orchestrator, and a smoke test.
- [docs/RECIPES.md](docs/RECIPES.md): copy-and-paste answers for common cases.
- [docs/CONVENTIONS.md](docs/CONVENTIONS.md): what the operator agrees to.
- [SECURITY.md](SECURITY.md): how the process defends itself.
- [docs/DECISIONS.md](docs/DECISIONS.md): why it is built this way.
- [docs/HANDOFF.md](docs/HANDOFF.md): notes for the next maintainer or reviewer.
- [docs/SIZING.md](docs/SIZING.md): RAM and CPU for the launcher process.
- [CONTRIBUTING.md](CONTRIBUTING.md).
- [AGENTS.md](AGENTS.md): notes for an AI coding agent.

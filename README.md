# shutup-and-run

[Português](README.pt-BR.md)

Push a job to a machine. It runs your container or tells you it is full. That is the whole product.

shutup-and-run is one static binary that sits on a machine with Docker. Your orchestrator (Kestra, n8n, a shell script) keeps a list of machines and POSTs a job to the first one:

| Answer | Meaning | What the caller does |
| --- | --- | --- |
| `202` | Accepted. A slot is taken and the container is being started. | Done. |
| `429` | Every slot is busy. | Try the next machine. |
| `503` | The machine is draining for maintenance. | Try the next machine. |

There is no queue, no scheduler, and no UI. The launcher does not know what your image does. It starts an allowlisted image, enforces the timeout and the slot limit, keeps a history, and calls you back when the job ends.

```text
 Kestra / n8n                 machine A                    machine B
 ------------                 ---------                    ---------
 POST /v1/jobs  ------------> 429 (full)
 POST /v1/jobs  -----------------------------------------> 202 (accepted)
                                                           pull image, run container
 webhook        <----------------------------------------- callback when it ends
```

A machine behind NAT can ask for work instead of being called. See [Pull](#pull). A Windows 11 machine runs the same thing in pull mode inside WSL2: [docs/WINDOWS.md](docs/WINDOWS.md).

## Quickstart

Everything here is copy and paste. Replace `ghcr.io/you/worker` with an image you own and `203.0.113.10` with the machine's address.

### 1. Install, on the machine

Ubuntu, with a public address. Needs a published release (a `v*` tag).

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
```

It installs Docker if needed, starts the service on port 8787 over HTTPS, and prints the URL and the certificate fingerprint. The token is not printed. Read it with:

```bash
sudo sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' /etc/shutup-and-run/env
```

### 2. First job, from anywhere

```bash
export SAR=https://203.0.113.10:8787
```

```bash
export SAR_TOKEN=paste-the-token-here
```

```bash
curl -sk -X POST "$SAR/v1/jobs" -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d '{"image":"ghcr.io/you/worker","timeout_secs":600,"env":{"JOB_NAME":"hello"}}'
```

The answer has an `id`. Status and the last log lines:

```bash
curl -sk -H "Authorization: Bearer $SAR_TOKEN" "$SAR/v1/jobs/PASTE_ID?logs=true"
```

`-k` skips certificate verification. To pin the installer's certificate instead, see [docs/RECIPES.md](docs/RECIPES.md).

### 3. Kestra

Create the secret `SHUTUP_AND_RUN_TOKEN`. One task per machine. The second runs only when the first said full or draining. The full flow with retries is `examples/kestra-dispatch.yml`.

```yaml
id: push_job
namespace: example.jobs

tasks:
  - id: host_1
    type: io.kestra.plugin.core.http.Request
    uri: https://203.0.113.10:8787/v1/jobs
    method: POST
    contentType: application/json
    allowFailure: true
    options:
      allowFailed: true
      ssl:
        insecureTrustAllCertificates: true
    headers:
      Authorization: "Bearer {{ secret('SHUTUP_AND_RUN_TOKEN') }}"
    body: |
      {{ { "image": "ghcr.io/you/worker", "job_key": execution.id, "timeout_secs": 3600 } }}

  - id: host_2
    type: io.kestra.plugin.core.http.Request
    runIf: "{{ outputs.host_1.code == 429 or outputs.host_1.code == 503 }}"
    uri: https://203.0.113.11:8787/v1/jobs
    method: POST
    contentType: application/json
    options:
      ssl:
        insecureTrustAllCertificates: true
    headers:
      Authorization: "Bearer {{ secret('SHUTUP_AND_RUN_TOKEN') }}"
    body: |
      {{ { "image": "ghcr.io/you/worker", "job_key": execution.id, "timeout_secs": 3600 } }}
```

### 4. n8n

Copy this and paste it onto the canvas. It is one HTTP Request node. Set `SHUTUP_AND_RUN_TOKEN` on the n8n process (with `N8N_BLOCK_ENV_ACCESS_IN_NODE=false`), or swap the header for a Header Auth credential. The two-machine workflow is `examples/n8n-dispatch.json`.

```json
{
  "nodes": [
    {
      "name": "Push job",
      "type": "n8n-nodes-base.httpRequest",
      "typeVersion": 4.2,
      "position": [260, 300],
      "retryOnFail": true,
      "maxTries": 3,
      "waitBetweenTries": 2000,
      "parameters": {
        "method": "POST",
        "url": "https://203.0.113.10:8787/v1/jobs",
        "sendHeaders": true,
        "headerParameters": {
          "parameters": [
            { "name": "Authorization", "value": "={{ 'Bearer ' + $env.SHUTUP_AND_RUN_TOKEN }}" },
            { "name": "Content-Type", "value": "application/json" }
          ]
        },
        "sendBody": true,
        "specifyBody": "json",
        "jsonBody": "={{ JSON.stringify({ image: 'ghcr.io/you/worker', job_key: $execution.id, timeout_secs: 3600 }) }}",
        "options": {
          "allowUnauthorizedCerts": true,
          "timeout": 20000,
          "response": { "response": { "fullResponse": true, "neverError": true } }
        }
      }
    }
  ],
  "connections": {}
}
```

The node's output has `statusCode`. Route `429` and `503` to a copy of the node that points at the next machine.

### 5. Locked out

Three wrong tokens ban the caller's address for 48 hours. On the machine, over SSH:

```bash
sudo shutup-and-run bans
```

```bash
sudo shutup-and-run bans lift 203.0.113.5
```

### 6. Check the machine

Twelve checks against the running service. See [Testing](#testing) for the image it uses.

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/smoke.sh | sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker bash
```

### 7. Stop everything

The service, then every job it started:

```bash
sudo systemctl stop shutup-and-run
```

```bash
sudo docker ps -q --filter label=shutup-and-run=1 | xargs -r sudo docker kill
```

More cases are in [docs/RECIPES.md](docs/RECIPES.md): the firewall rule, a machine behind NAT, a machine a client lends you, draining, rotating the token.

## Security in one minute

- The token is a root password for the machine. Anyone who holds it can run any allowlisted image with any environment. Every route needs it, `/health` and `/metrics` included.
- Three wrong tokens from one address ban it for 48 hours. A banned address gets `403` with an empty body, even with the right token.
- The service speaks HTTPS with a self-signed certificate the installer generates. Pin the fingerprint it prints.
- A job cannot mount the host, publish a port, or run privileged. It is still not a sandbox: the container has Docker's default privileges and network.
- The port is open to the internet unless you firewall it. Allow only your orchestrator. The command is in [docs/RECIPES.md](docs/RECIPES.md).

How the process behaves is in [SECURITY.md](SECURITY.md). What you accept by running it is in [docs/CONVENTIONS.md](docs/CONVENTIONS.md).

## API

Every route requires `Authorization: Bearer <token>`.

### `POST /v1/jobs`

```json
{
  "image": "ghcr.io/you/worker:latest",
  "job_key": "order-1001",
  "env": {"JOB_NAME": "example"},
  "command": ["python", "main.py"],
  "timeout_secs": 3600,
  "cpu": 1,
  "memory": "2g",
  "shm": "2g",
  "callback_url": "https://n8n.example/webhook/done"
}
```

| Field | |
| --- | --- |
| `image` | Required. Must be on the allowlist. Lowercase. |
| `job_key` | Optional. Makes a retry safe: the same key returns the same job. 1 to 200 characters: letters, digits, `.`, `_`, `:`, `-`. |
| `env` | Optional object of strings. Put secrets here, not in the image. |
| `command` | Optional Docker `Cmd`. Omit it to run the image as built. |
| `timeout_secs` | Optional. The container is killed after this long. Default 3600, at most `max_timeout_secs`. |
| `cpu` | Optional CPU count, such as `0.5` or `2`. |
| `memory`, `shm` | Optional. Bytes, or `512m`, `1g`. Without `memory` the container is not capped. |
| `callback_url` | Optional. Receives the final job JSON when the job ends. |

Unknown fields are refused.

| Code | Body | Meaning |
| --- | --- | --- |
| `202` | the job | Accepted. The slot is taken. The image is pulled and the container started after this answer. |
| `200` | the job | This `job_key` was already accepted. Nothing new starts. |
| `429` | `{"error":"no free slots","slots_total":2,"slots_used":2,"slots_free":0}` | Full. |
| `503` | `{"error":"draining","draining":true}` | Draining. |
| `403` | `{"error":"image is not allowlisted"}` | |
| `400` | `{"error":"..."}` | Bad body, timeout, key, or callback URL. |
| `401` | `{"error":"unauthorized"}` | Counts toward a ban. |
| `502` | `{"error":"docker is unavailable"}` | Docker or the history file cannot be reached. |

The job:

```json
{"id":"5f479abd3087cf193ed4c1da8460784d","status":"running","image":"ghcr.io/you/worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":null,"exit_code":null,"reason":null,"duration_secs":null}
```

`202` is an answer in milliseconds. It does not wait for the registry. If the pull or the start then fails, the job ends with `reason` `start_failed`, its `logs` say why, and the callback is sent. The `job_key` of a failed job stays taken; use a new key for a new attempt. `timeout_secs` starts counting when the container starts.

### `GET /v1/jobs/{id}`

Add `?logs=true` for the last 200 log lines.

```json
{"id":"5f479abd3087cf193ed4c1da8460784d","status":"exited","image":"ghcr.io/you/worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":1700000012,"exit_code":0,"reason":"exited","duration_secs":12,"logs":"..."}
```

`status` is `running` or `exited`. `reason` is `exited`, `timeout_kill`, or `start_failed`. History is in SQLite, so this works after a restart, for 30 days by default. An unknown id is `404` `{"id":"...","status":"not_found"}`.

There is no cancel call. To end a job early, kill its container on the machine: it is named `shutup-and-run-<id>`.

### Callback

When a job ends, the same JSON (without logs) is POSTed to `callback_url`. Three tries, no redirects. Point it at a Kestra or n8n webhook.

### `GET /health`

```json
{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}
```

A used slot is a running container labeled `shutup-and-run=1`, or a job that was accepted and is still pulling its image. `503` when Docker cannot be reached.

### `GET /metrics`

Prometheus text: `shutup_and_run_slots_total`, `_slots_used`, `_slots_free`, `_draining`, `_jobs_started_total`, `_jobs_finished_total`, `_jobs_timeout_total`.

### `POST /v1/drain`

`{"draining": true}` refuses new jobs with `503` and lets running ones finish. `{"draining": false}` goes back. The flag survives a restart.

## Pull

For a machine that cannot be reached from outside. Set `poll_url`, and the machine asks that URL for one task every `poll_interval_secs`, only when a slot is free and it is not draining. Leave it unset and nothing calls out.

```http
POST {poll_url}
Authorization: Bearer {SHUTUP_AND_RUN_POLL_TOKEN}
Content-Type: application/json

{"worker": "desk-07", "os": "linux", "free_slots": 1}
```

The body says who is asking, so one URL can serve every machine: `worker` is `worker_name` (default: the hostname), `os` is `worker_os` (default: the OS the binary was built for; the Windows worker says `windows`), and `free_slots` is how many tasks the machine could take right now.

- Nothing to do: `204`, or `200` with an empty body, `null`, `{}`, `{"task": null}`, or `{"manifest_url": null}`.
- A task: `200` with the same JSON as `POST /v1/jobs`, or `{"task": { ... }}`.
- A manifest: `200` with `{"manifest_url": "https://..."}`. The machine fetches that URL once with a `GET` and the body must be a task, in either shape above. A manifest cannot name another manifest. The poll token goes with the `GET` only when the manifest is on the same scheme, host and port as `poll_url`; a manifest on object storage or a CDN is fetched without it. Same 64 KiB cap, no redirects.

The task goes through the same checks as a POST. Offering a task is not an acknowledgement: the machine may refuse it (slot just taken, image not allowlisted). Keep offering the same `job_key` until its callback arrives. Once accepted, that key never starts a second container.

`poll_any_image = true` lets a polled task name any image, allowlisted or not. The image name is still validated, and a pushed job is still held to the allowlist, which may then be empty. It needs `poll_url`. It is for a machine whose only source of work is your orchestrator, such as the Windows worker; on a server that also takes pushes, leave it off.

Errors (connection failure, redirect, any other status, a body over 64 KiB) are logged, and the wait doubles up to 5 minutes. The process does not exit. The poll token is read from the environment only. Examples: `examples/kestra-poll.yml`, `examples/n8n-poll.json`. The setup commands are in [docs/RECIPES.md](docs/RECIPES.md).

## Configuration

`/etc/shutup-and-run/config.toml`, or the file named by `SHUTUP_AND_RUN_CONFIG`. Environment variables override the file. An unknown key refuses to start. After a change: `sudo systemctl restart shutup-and-run`. Running jobs survive the restart.

| toml | environment | meaning |
| --- | --- | --- |
| `listen` | `SHUTUP_AND_RUN_LISTEN` | `ip:port`. Default `127.0.0.1:8787`. The installer writes `0.0.0.0:8787`. |
| `token` | `SHUTUP_AND_RUN_TOKEN` | Required. At least 16 characters. The installer keeps it in `/etc/shutup-and-run/env`. |
| `max_slots` | `SHUTUP_AND_RUN_MAX_SLOTS` | Required. 1 to 256. The installer suggests one per CPU and 2 GiB. |
| `allowlist` | `SHUTUP_AND_RUN_ALLOWLIST` | Required unless `poll_any_image` is set. Images that may run. Comma-separated in the environment. |
| `default_timeout_secs` | `SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS` | Default 3600. |
| `max_timeout_secs` | `SHUTUP_AND_RUN_MAX_TIMEOUT_SECS` | Default 21600. At most 7 days. |
| `history_path` | `SHUTUP_AND_RUN_HISTORY_PATH` | SQLite file. Default `/var/lib/shutup-and-run/jobs.sqlite`. |
| `history_days` | `SHUTUP_AND_RUN_HISTORY_DAYS` | Finished jobs older than this are deleted. Default 30. |
| `log_lines` | `SHUTUP_AND_RUN_LOG_LINES` | Log lines kept per job. Default 200. |
| `prune_interval_secs` | `SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS` | Remove finished containers and unused images this often. Default 0, off. |
| `auth_failures` | `SHUTUP_AND_RUN_AUTH_FAILURES` | Wrong tokens before a ban. Default 3. |
| `ban_hours` | `SHUTUP_AND_RUN_BAN_HOURS` | Default 48. |
| `tls_cert`, `tls_key` | `SHUTUP_AND_RUN_TLS_CERT`, `SHUTUP_AND_RUN_TLS_KEY` | PEM files. Required unless `http = true`. |
| `http` | `SHUTUP_AND_RUN_HTTP` | `true` serves plaintext. For localhost only. |
| `trust_x_forwarded_for` | `SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR` | Only behind a proxy you control. Default false. |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Optional. See [Pull](#pull). |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Default 15. |
| (none) | `SHUTUP_AND_RUN_POLL_TOKEN` | Environment only. |
| `poll_any_image` | `SHUTUP_AND_RUN_POLL_ANY_IMAGE` | A polled task may name any image. Default false. Needs `poll_url`. Lets `allowlist` be empty. |
| `worker_name` | `SHUTUP_AND_RUN_WORKER_NAME` | Sent in the poll body. Default: the hostname. |
| `worker_os` | `SHUTUP_AND_RUN_WORKER_OS` | Sent in the poll body. Default: the build OS. |
| `reaper_interval_secs` | `SHUTUP_AND_RUN_REAPER_INTERVAL_SECS` | How often expired containers are killed. Default 2. |
| `purge_interval_secs` | `SHUTUP_AND_RUN_PURGE_INTERVAL_SECS` | How often old history is deleted. Default 3600. |
| `docker_config` | `SHUTUP_AND_RUN_DOCKER_CONFIG` | Path to Docker's `config.json`, for private images. |

**Allowlist.** `ghcr.io/you/worker` allows any tag or digest of that repository and nothing else: not `ghcr.io/you/worker-evil`, not `ghcr.io/you/other`. An entry with a tag, such as `ghcr.io/you/worker:1.4`, allows exactly that string.

**Private images.** `sudo docker login ghcr.io -u YOU` on the machine. The caller cannot supply registry credentials.

**Logs.** `journalctl -u shutup-and-run -f`. The token and job environment are never logged.

## Installer

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/you/worker bash
```

| Variable | |
| --- | --- |
| `SHUTUP_AND_RUN_ALLOWLIST` | Images for the first `config.toml`. Without it the installer writes a placeholder and warns. |
| `SHUTUP_AND_RUN_MAX_SLOTS` | Overrides the suggested slot count. |
| `SHUTUP_AND_RUN_PORT` | Default 8787. |
| `SHUTUP_AND_RUN_NO_UFW=1` | Do not open the port in ufw. |
| `SHUTUP_AND_RUN_SHOW_TOKEN=1` | Print the token. |
| `SHUTUP_AND_RUN_BIN=./shutup-and-run` | Install a binary you already have. No download, no checksum. |

The download is checked against `SHA256SUMS` from the same release, and a mismatch stops the install. Running it again upgrades the binary and keeps the token, certificate, and config. `--uninstall` removes the service and the binary. `--uninstall --purge` also removes `/etc/shutup-and-run` and `/var/lib/shutup-and-run`.

A container image is also published at `ghcr.io/nailtongomes/shutup-and-run` for people who run everything under Docker. It needs the host Docker socket mounted, which is root on the host. The systemd install is the supported path.

### Windows 11

The release also ships `shutup-and-run-x86_64-pc-windows-msvc.exe`. It is a supervisor: it imports its own WSL2 distribution, installs Docker Engine and the Linux binary inside it, and keeps the service running there in pull mode. No Docker Desktop, no open port. Once WSL2 is enabled (`wsl --install --no-distribution` as administrator, then reboot):

```powershell
$env:SHUTUP_AND_RUN_POLL_TOKEN = "<token from your orchestrator>"
$env:SHUTUP_AND_RUN_POLL_URL = "https://orchestrator.example/poll"
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

`shutup-and-run wsl doctor` checks WSL, the distribution, Docker, runs one container, and checks the service. `wsl logs`, `wsl stop`, `wsl start`, `wsl uninstall`. Everything, including what a lent Windows machine gives up, is in [docs/WINDOWS.md](docs/WINDOWS.md).

## Testing

`scripts/smoke.sh` checks a running instance through its API. It needs `curl` and `python3` where it runs, and one image on the instance's allowlist:

- **Your own worker**, if it has `sh` and `sleep`. This is the best check, because it is what you will run.
- **`ghcr.io/nailtongomes/shutup-and-run-smoke`**, the default. A small Python image built from `examples/smoke-image`. With no command it prints `python ok` and exits.
- **`hello-world`**. It has no shell, so the five checks that need a job to stay up are skipped.

On the machine, as root. `SMOKE_LOCAL=1` reads the URL, token and fingerprint from `/etc/shutup-and-run`:

```bash
sudo SMOKE_LOCAL=1 SMOKE_IMAGE=ghcr.io/you/worker SMOKE_RESTART_CMD="systemctl restart shutup-and-run" bash scripts/smoke.sh
```

From another machine:

```bash
SMOKE_URL=https://203.0.113.10:8787 SMOKE_TOKEN=... SMOKE_FINGERPRINT=AA:BB:... SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh
```

One job with every field set, and nothing else:

```bash
SMOKE_URL=... SMOKE_TOKEN=... SMOKE_IMAGE=ghcr.io/you/worker bash scripts/smoke.sh preflight
```

```text
PASS: 1 health returns slot counts; 401 without a token
PASS: 2 job accepted, running, exited 0, logs returned
PASS: 3 3 slots filled, then 429, then 202 after one job ended
PASS: 4 job past timeout_secs was killed, reason timeout_kill
PASS: 5 image off the allowlist is 403 and nothing started
PASS: 6 same job_key is 202 then 200 with the same id
PASS: 7 drain is 503 for new jobs; undrain is 202 again
PASS: 8 restart kept history, kept the running job, and freed its slot when it ended
PASS: 9 three wrong tokens from 127.0.0.2 is 403 even with the right token; bans lift restores it
PASS: 10 poll took one task, started exactly one container, then got empty answers
PASS: 11 callback_url received the final job JSON once
PASS: 12 preflight: one job end to end (env, cpu, memory, shm, job_key, callback), slot freed

RESULT: 12 passed, 0 failed, 0 skipped
```

It is safe on a live machine. Its jobs only print and sleep, each with a `timeout_secs` of 120 at most. It never sends a wrong token from your address. A check that needs something you did not give is `SKIP`, with the reason. Two things a live machine will notice: check 3 fills every free slot for a few seconds, and check 7 drains for about a second. The header of the script lists every variable.

`scripts/accept.sh` is the developer's test. It starts its own instance against a real Docker daemon. `SHUTUP_AND_RUN_SMOKE=1` runs `smoke.sh` on that instance afterwards. CI runs both on every push, and runs `smoke.sh` again against a service installed by `install.sh`.

## Limits

- One process per Docker daemon. Two can exceed `max_slots`.
- Stopping the service does not stop containers, and while it is stopped nothing enforces timeouts.
- A timeout is SIGKILL. There is no grace period.
- `job_key` is per machine. Another machine does not know it.
- No cancel call, no queue, no retry of a job that failed.
- Windows runs the Linux service inside WSL2, pull mode only, while the installing user is logged on. Stopping it kills the containers inside the VM. [docs/WINDOWS.md](docs/WINDOWS.md). macOS is planned: [docs/ROADMAP-windows-macos.md](docs/ROADMAP-windows-macos.md).

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

A release is a `v*` tag pushed from `main`. It attaches the binary and `SHA256SUMS`, publishes the image, then runs the one-line install and the smoke test on a fresh runner.

MIT license. More reading:

- [docs/RECIPES.md](docs/RECIPES.md): copy-and-paste answers for common cases.
- [docs/CONVENTIONS.md](docs/CONVENTIONS.md): what the operator agrees to.
- [SECURITY.md](SECURITY.md): how the process defends itself.
- [docs/DECISIONS.md](docs/DECISIONS.md): why it is built this way.
- [docs/HANDOFF.md](docs/HANDOFF.md): notes for the next maintainer or reviewer.
- [docs/SIZING.md](docs/SIZING.md): RAM and CPU for the launcher process.
- [CONTRIBUTING.md](CONTRIBUTING.md).
- [AGENTS.md](AGENTS.md): notes for an AI coding agent.

# fairlead

fairlead is a single static binary. One machine with Docker receives an HTTP POST, and if it has a free slot it starts an allowlisted container and returns immediately. If it does not, it refuses, and the caller tries the next machine on its own list. There is no queue, no scheduler, and no UI. The container writes its own result wherever it already does.

A slot is a running container labeled `fairlead=1`. Restarting the process does not forget them. Finished jobs stay in one SQLite file so `GET /v1/jobs/{id}` still works after a restart.

## Security

This process speaks to the Docker socket. Anyone with the token can run any allowlisted image with any environment. Treat the token as a root password.

**Bind `listen` to a private interface or a Tailscale address.** Do not use `0.0.0.0`. If you omit `listen`, the default is `127.0.0.1:8787`. See [SECURITY.md](SECURITY.md).

The token is not in the source. Comparison is a SHA-256 digest checked in constant time. Registry credentials come from the machine's existing `docker login`, never from the request.

## Install

The release workflow builds `fairlead-x86_64-unknown-linux-musl` and, on a GitHub-compatible forge, attaches it to the release for tag `v*`. Check `SHA256SUMS` from the same release. The compile does not require GitHub; publishing the file to a Release does. See the note in `.github/workflows/release.yml`.

On the machine, as root, with Docker already running:

```bash
sudo install -m 0755 fairlead-x86_64-unknown-linux-musl /usr/local/bin/fairlead
sudo mkdir -p /etc/fairlead /var/lib/fairlead
sudo tee /etc/fairlead/config.toml >/dev/null <<'EOF'
listen = "192.0.2.11:8787"
max_slots = 2
allowlist = [
  "ghcr.io/owner/selenium-worker",
]
default_timeout_secs = 3600
max_timeout_secs = 21600
history_path = "/var/lib/fairlead/jobs.sqlite"
EOF
echo "FAIRLEAD_TOKEN=$(openssl rand -hex 32)" | sudo tee /etc/fairlead/env >/dev/null
sudo chmod 600 /etc/fairlead/env /etc/fairlead/config.toml
sudo cp deploy/fairlead.service /etc/systemd/system/fairlead.service
sudo systemctl daemon-reload
sudo systemctl enable --now fairlead
curl -sS http://192.0.2.11:8787/health
```

`192.0.2.11` is a documentation address. Replace it with this machine's private address. Image names are lowercase. An allowlist entry without a tag allows any tag of that repository; `ghcr.io/owner/selenium-worker` does not allow `ghcr.io/owner/selenium-worker-evil`.

`openssl rand` prints the token once. Put the same value in the orchestrator's secret. To rotate it, edit the env file and `sudo systemctl restart fairlead`.

### Private registries

```bash
sudo docker login ghcr.io -u USER
```

That writes `/root/.docker/config.json` because the unit sets `HOME=/root`. A desktop credential helper will not work on a headless server. The file is read on every pull, so a new login applies without a restart. This login is not `FAIRLEAD_TOKEN`.

## Configuration

TOML from `FAIRLEAD_CONFIG`, or `/etc/fairlead/config.toml` when that variable is unset. Environment variables override the file. An unknown TOML field refuses to start.

| toml | environment | meaning |
| --- | --- | --- |
| `listen` | `FAIRLEAD_LISTEN` | `ip:port`. Default `127.0.0.1:8787`. |
| `token` | `FAIRLEAD_TOKEN` | Required, at least 16 characters, no spaces. |
| `max_slots` | `FAIRLEAD_MAX_SLOTS` | Required, 1 to 256. |
| `allowlist` | `FAIRLEAD_ALLOWLIST` | Images. Comma-separated in the environment. |
| `default_timeout_secs` | `FAIRLEAD_DEFAULT_TIMEOUT_SECS` | Used when the request omits a timeout. Default 3600. |
| `max_timeout_secs` | `FAIRLEAD_MAX_TIMEOUT_SECS` | Ceiling. Default 21600. At most 7 days. |
| `reaper_interval_secs` | `FAIRLEAD_REAPER_INTERVAL_SECS` | How often expired containers are killed. Default 2. |
| `history_path` | `FAIRLEAD_HISTORY_PATH` | SQLite file. Default `/var/lib/fairlead/jobs.sqlite`. |
| `history_days` | `FAIRLEAD_HISTORY_DAYS` | Delete finished rows older than this. Default 30. `0` deletes them on the next purge. |
| `purge_interval_secs` | `FAIRLEAD_PURGE_INTERVAL_SECS` | How often that delete runs. Default 3600. |
| `log_lines` | `FAIRLEAD_LOG_LINES` | Log lines kept per job. Default 200. |
| `prune_interval_secs` | `FAIRLEAD_PRUNE_INTERVAL_SECS` | `0` (default) disables pruning. |
| `docker_config` | `FAIRLEAD_DOCKER_CONFIG` | Path to Docker's `config.json`. |

Logs: `journalctl -u fairlead -f`. Level comes from `RUST_LOG` (default `info`). The token and job environment are not logged.

## API

`Authorization: Bearer <token>` except for `/health` and `/metrics`.

### `GET /health`

No token.

```bash
curl -sS http://192.0.2.11:8787/health
```

```json
{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}
```

While draining, `status` is `"draining"` and `draining` is `true`. `503` if Docker cannot be listed: `{"error":"docker is unavailable","draining":false}`.

### `GET /metrics`

No token. Prometheus text, so Uptime Kuma or Prometheus can scrape it on the private interface.

```text
# HELP fairlead_slots_total Configured slot capacity.
# TYPE fairlead_slots_total gauge
fairlead_slots_total 2
# HELP fairlead_slots_used Running containers labeled fairlead=1.
# TYPE fairlead_slots_used gauge
fairlead_slots_used 1
# HELP fairlead_slots_free Slots not currently taken. Not offered while draining.
# TYPE fairlead_slots_free gauge
fairlead_slots_free 1
# HELP fairlead_draining 1 while new jobs are refused.
# TYPE fairlead_draining gauge
fairlead_draining 0
# HELP fairlead_jobs_started_total Jobs recorded in SQLite, including those still running.
# TYPE fairlead_jobs_started_total counter
fairlead_jobs_started_total 4
# HELP fairlead_jobs_finished_total Jobs that have ended.
# TYPE fairlead_jobs_finished_total counter
fairlead_jobs_finished_total 3
# HELP fairlead_jobs_timeout_total Jobs ended with reason timeout_kill.
# TYPE fairlead_jobs_timeout_total counter
fairlead_jobs_timeout_total 1
```

### `POST /v1/jobs`

```bash
curl -sS -D - -o - -X POST http://192.0.2.11:8787/v1/jobs \
  -H "Authorization: Bearer $FAIRLEAD_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "image": "ghcr.io/owner/selenium-worker:latest",
    "job_key": "order-1001",
    "env": {"JOB_NAME": "example"},
    "timeout_secs": 3600,
    "cpu": 1,
    "memory": "2g",
    "shm": "2g",
    "callback_url": "https://n8n.example/webhook/fairlead"
  }'
```

`202` when a container is started. `200` when `job_key` already exists: the same job is returned and nothing new is started, including while it is still running and after it has finished. A retry of the same URL is safe. The key is local to this machine's SQLite file; another machine does not know it.

```json
{"id":"0123abcd","status":"running","image":"ghcr.io/owner/selenium-worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":null,"exit_code":null,"reason":null,"duration_secs":null}
```

| field | |
| --- | --- |
| `image` | Required. Must be on the allowlist. |
| `job_key` | Optional. 1–200 characters: letters, digits, `.`, `_`, `:`, `-`. |
| `env` | Optional object of strings. |
| `timeout_secs` | Optional. Otherwise the default. Above the maximum is `400`. |
| `cpu` | Optional CPU count, such as `0.5` or `1`. |
| `memory`, `shm` | Optional bytes, or a Docker-style string: `512m`, `1g`, `64mb`. |
| `command` | Optional Docker `Cmd`. Omit it to use the image's own entrypoint. |
| `callback_url` | Optional `http` or `https` URL. Called when the job ends. |

`memory` sets Docker `Memory` and `MemorySwap` to the same value, so the container gets no extra swap.

Other responses:

| code | body |
| --- | --- |
| `429` | `{"error":"no free slots","slots_total":2,"slots_used":2,"slots_free":0}` |
| `503` | `{"error":"draining","draining":true}` while draining. Running jobs are left alone. |
| `401` | `{"error":"unauthorized"}` |
| `403` | `{"error":"image is not allowlisted"}` |
| `400` | `{"error":"..."}` for a bad body, timeout, key, or callback URL. |
| `502` | `{"error":"failed to start container","id":"...","reason":"start_failed"}` or `{"error":"failed to pull image"}`. A `job_key` on a failed start is kept, so a retry returns `200` with that failure instead of starting again. Use a new key for a new attempt. |

### `GET /v1/jobs/{id}`

```bash
curl -sS -H "Authorization: Bearer $FAIRLEAD_TOKEN" \
  "http://192.0.2.11:8787/v1/jobs/0123abcd?logs=true"
```

The row is in SQLite, so this survives a process restart. `?logs=true` adds `logs` (the last `log_lines` lines). While the container is still running those lines are read from Docker; after it exits they are the copy stored before the container is removed.

```json
{"id":"0123abcd","status":"exited","image":"ghcr.io/owner/selenium-worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":1700000012,"exit_code":0,"reason":"exited","duration_secs":12,"logs":"..."}
```

`reason` is `exited`, `timeout_kill`, `launcher_kill`, or `start_failed`. `404` is `{"id":"...","status":"not_found"}` for an unknown id or a row that purge has deleted.

When the job ends, fairlead POSTs the same fields (without logs) to `callback_url`. It tries three times, does not follow redirects, and does not hold a slot. Point it at the Kestra or n8n webhook.

### `POST /v1/drain`

```bash
curl -sS -X POST http://192.0.2.11:8787/v1/drain \
  -H "Authorization: Bearer $FAIRLEAD_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"draining":true}'
```

`200` `{"draining":true}`. `{"draining":false}` leaves drain mode. The flag is stored in SQLite, so a restart stays drained. New jobs get `503`. Running jobs finish.

## Kestra

Create the secret `FAIRLEAD_TOKEN`. The task `timeout` is how long Kestra waits for fairlead to answer, not how long the container may run. That duration is `timeout_secs` in the JSON.

`job_key` is `execution.id`, and POST transport errors are retried on the same URL, so a lost response does not start a second container on that machine. The next machine is used only for `429` and `503`. A machine that never answers fails the flow instead of failing over, because a request that actually landed would be duplicated on the other host (keys are not shared).

The same file is `examples/kestra-dispatch.yml`.

```yaml
id: dispatch_job
namespace: example.jobs

inputs:
  - id: job_name
    type: STRING
    defaults: example-job

tasks:
  - id: host_1
    type: io.kestra.plugin.core.http.Request
    uri: http://192.0.2.11:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
      retryableTransportFailureMethods:
        - POST
      retry:
        type: constant
        interval: PT2S
        maxAttempts: 3
    headers:
      Authorization: "Bearer {{ secret('FAIRLEAD_TOKEN') }}"
    body: |
      {{ {
        "image": "ghcr.io/owner/selenium-worker:latest",
        "job_key": execution.id,
        "env": { "JOB_NAME": inputs.job_name },
        "timeout_secs": 3600,
        "shm": "2g",
        "callback_url": "https://kestra.example/api/v1/executions/webhook/example/jobs/dispatch_job/finish"
      } }}

  - id: host_2
    type: io.kestra.plugin.core.http.Request
    runIf: "{{ outputs.host_1 is defined and (outputs.host_1.code == 429 or outputs.host_1.code == 503) }}"
    uri: http://192.0.2.12:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
      retryableTransportFailureMethods:
        - POST
      retry:
        type: constant
        interval: PT2S
        maxAttempts: 3
    headers:
      Authorization: "Bearer {{ secret('FAIRLEAD_TOKEN') }}"
    body: |
      {{ {
        "image": "ghcr.io/owner/selenium-worker:latest",
        "job_key": execution.id,
        "env": { "JOB_NAME": inputs.job_name },
        "timeout_secs": 3600,
        "shm": "2g",
        "callback_url": "https://kestra.example/api/v1/executions/webhook/example/jobs/dispatch_job/finish"
      } }}

  - id: none_accepted
    type: io.kestra.plugin.core.execution.Fail
    runIf: "{{ (outputs.host_1 is not defined or (outputs.host_1.code != 200 and outputs.host_1.code != 202)) and (outputs.host_2 is not defined or (outputs.host_2.code != 200 and outputs.host_2.code != 202)) }}"
    errorMessage: "No machine accepted the job (full, draining, or unreachable)."
```

## n8n

Import `examples/n8n-dispatch.json`. Each HTTP Request node retries the same URL (`retryOnFail`, 3 tries). `200` and `202` stop the chain. `429` and `503` go to the next host. Anything else stops with an error, so a connection failure is not treated as "try the other machine" after the retries are spent.

Set `FAIRLEAD_TOKEN` on the n8n process and allow environment access (`N8N_BLOCK_ENV_ACCESS_IN_NODE=false`), or replace the header expression with a Header Auth credential. `job_key` is `$execution.id`. Put your webhook URL in `callback_url`.

## Acceptance test

On Ubuntu with Docker, preferably with no other fairlead and no container already labeled `fairlead=1`:

```bash
sudo bash scripts/accept.sh
sudo bash scripts/accept.sh /path/to/fairlead-x86_64-unknown-linux-musl
```

With systemd it installs `/usr/local/bin/fairlead`, starts `fairlead-accept.service`, and removes that unit at the end. The binary stays. It does not touch a production `fairlead.service`. You want:

```text
PASS: accepted until full
PASS: 429 when full
PASS: slot freed after a job ends
PASS: metrics exposed
PASS: idempotent job_key
PASS: logs returned
PASS: status survives restart
PASS: callback delivered
PASS: timeout kill
PASS: drain rejects new jobs
PASS: history purged
RESULT: all passed
```

## Limits

- One process per Docker daemon. Two processes can exceed `max_slots`.
- Stopping the service does not kill containers.
- A timeout is SIGKILL, with no SIGTERM.
- `job_key` does not cross machines.
- Pruning is off unless `prune_interval_secs` is set. It removes stopped containers with the fairlead label, and images recorded in this SQLite file that no container is using. It does not delete images fairlead never launched.
- Container JSON logs can still fill the disk. Turn on rotation in the Docker daemon.
- There is no queue, no cross-machine coordination, and no restart of a container that exits on its own.

## Development

```bash
cargo test
cargo build --release
```

Static binary:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools build-essential
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --target x86_64-unknown-linux-musl
```

SQLite is bundled, so the musl build needs a C compiler. The socket is `DOCKER_HOST` when it is `unix://...`, otherwise `/var/run/docker.sock`.

License: MIT. See [CONTRIBUTING.md](CONTRIBUTING.md).

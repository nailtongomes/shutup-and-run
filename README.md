# shutup-and-run

shutup-and-run is a single static binary. One machine with Docker starts an allowlisted container when a slot is free. Work arrives as an HTTP POST or, when `poll_url` is set, the machine asks a URL for a task. A POST with no free slot is `429`, and the caller tries the next machine on its own list. A poll is skipped until a slot is free. There is no queue, no scheduler, and no UI. The container writes its own result wherever it already does.

A slot is a running container labeled `shutup-and-run=1`. Restarting the process does not forget them. Finished jobs stay in one SQLite file so `GET /v1/jobs/{id}` still works after a restart.

## Security

This process speaks to the Docker socket. Anyone with the token can run any allowlisted image with any environment. Treat the token as a root password. Every route, including `/health` and `/metrics`, requires it. See [SECURITY.md](SECURITY.md). What you are accepting by deploying it is [docs/CONVENTIONS.md](docs/CONVENTIONS.md). The binary does not try to enforce those conventions.

The installer listens on `0.0.0.0` and serves HTTPS from a self-signed certificate it generates. It prints the certificate's SHA-256 fingerprint. Plain HTTP is only `http = true`, meant for localhost. Three bad tokens from one IP ban that IP for 48 hours (`403` with an empty body). Lift it over SSH with `shutup-and-run bans lift <ip>`.

## Install

One command on a new Ubuntu machine. A tag `v*` builds the musl binary and attaches it to the GitHub release, which is what this downloads.

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash
```

Offline, with a binary you already copied onto the machine:

```bash
sudo SHUTUP_AND_RUN_BIN=./shutup-and-run bash scripts/install.sh
```

The script installs Docker if it is missing, checks the downloaded binary against `SHA256SUMS` from the same release, picks `max_slots` from CPU and RAM (about one CPU and 2 GiB per slot, leaving one CPU free), writes a token to `/etc/shutup-and-run/env` without printing it, generates a self-signed cert if you do not have one, installs the systemd unit, and opens the port in ufw when ufw is already active. A mismatch stops the install. `SHUTUP_AND_RUN_BIN` skips the download and the check. Running it again upgrades the binary and restarts the service. It does not rotate an existing token or certificate. `SHUTUP_AND_RUN_SHOW_TOKEN=1` prints the token. `sudo bash scripts/install.sh --uninstall` removes the unit and the binary; add `--purge` to delete `/etc/shutup-and-run` and `/var/lib/shutup-and-run`.

Image names are lowercase. An allowlist entry without a tag allows any tag of that repository; `ghcr.io/owner/selenium-worker` does not allow `ghcr.io/owner/selenium-worker-evil`. Edit the allowlist after install. To rotate the token, edit the env file and `sudo systemctl restart shutup-and-run`.

### Container image

The systemd install above is the path to use. A container image is also published to `ghcr.io/nailtongomes/shutup-and-run` (`:edge` from `main`, and the tag name from `v*`). It is a scratch image around the static binary. It can only start jobs if you hand it the host Docker socket, and that socket is root on the host:

```bash
docker run -d --name shutup-and-run --restart unless-stopped \
  -p 8787:8787 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  -v /etc/shutup-and-run:/etc/shutup-and-run:ro \
  -v /var/lib/shutup-and-run:/var/lib/shutup-and-run \
  ghcr.io/nailtongomes/shutup-and-run:edge
```

Put the same config, token, and certificate the installer would write under those two directories. Mount `/root/.docker` as well if jobs pull private images. The image is linux/amd64.

### Private registries

```bash
sudo docker login ghcr.io -u USER
```

That writes `/root/.docker/config.json` because the unit sets `HOME=/root`. A desktop credential helper will not work on a headless server. The file is read on every pull, so a new login applies without a restart. This login is not `SHUTUP_AND_RUN_TOKEN`.

## Configuration

TOML from `SHUTUP_AND_RUN_CONFIG`, or `/etc/shutup-and-run/config.toml` when that variable is unset. Environment variables override the file. An unknown TOML field refuses to start.

| toml | environment | meaning |
| --- | --- | --- |
| `listen` | `SHUTUP_AND_RUN_LISTEN` | `ip:port`. Default `127.0.0.1:8787`. |
| `token` | `SHUTUP_AND_RUN_TOKEN` | Required, at least 16 characters, no spaces. |
| `max_slots` | `SHUTUP_AND_RUN_MAX_SLOTS` | Required, 1 to 256. |
| `allowlist` | `SHUTUP_AND_RUN_ALLOWLIST` | Images. Comma-separated in the environment. |
| `default_timeout_secs` | `SHUTUP_AND_RUN_DEFAULT_TIMEOUT_SECS` | Used when the request omits a timeout. Default 3600. |
| `max_timeout_secs` | `SHUTUP_AND_RUN_MAX_TIMEOUT_SECS` | Ceiling. Default 21600. At most 7 days. |
| `reaper_interval_secs` | `SHUTUP_AND_RUN_REAPER_INTERVAL_SECS` | How often expired containers are killed. Default 2. |
| `history_path` | `SHUTUP_AND_RUN_HISTORY_PATH` | SQLite file. Default `/var/lib/shutup-and-run/jobs.sqlite`. |
| `history_days` | `SHUTUP_AND_RUN_HISTORY_DAYS` | Delete finished rows older than this. Default 30. `0` deletes them on the next purge. |
| `purge_interval_secs` | `SHUTUP_AND_RUN_PURGE_INTERVAL_SECS` | How often that delete runs. Default 3600. |
| `log_lines` | `SHUTUP_AND_RUN_LOG_LINES` | Log lines kept per job. Default 200. |
| `prune_interval_secs` | `SHUTUP_AND_RUN_PRUNE_INTERVAL_SECS` | `0` (default) disables pruning. |
| `docker_config` | `SHUTUP_AND_RUN_DOCKER_CONFIG` | Path to Docker's `config.json`. |
| `http` | `SHUTUP_AND_RUN_HTTP` | `true` serves plaintext. Default is HTTPS, which then requires the cert paths. |
| `tls_cert` | `SHUTUP_AND_RUN_TLS_CERT` | PEM certificate. |
| `tls_key` | `SHUTUP_AND_RUN_TLS_KEY` | PEM private key. |
| `auth_failures` | `SHUTUP_AND_RUN_AUTH_FAILURES` | Failed auths from one IP before a ban. Default 3. |
| `ban_hours` | `SHUTUP_AND_RUN_BAN_HOURS` | How long a ban lasts. Default 48. |
| `trust_x_forwarded_for` | `SHUTUP_AND_RUN_TRUST_X_FORWARDED_FOR` | Use the first `X-Forwarded-For` address. Default false. |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Optional. When set, ask this URL for one task. Unset means push only. |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Seconds between polls. Default 15. Range 1 to 3600. Used only when `poll_url` is set. |
| (none) | `SHUTUP_AND_RUN_POLL_TOKEN` | Bearer token for the poll request. Environment only. A `poll_token` key in the file is refused. |

Logs: `journalctl -u shutup-and-run -f`. Level comes from `RUST_LOG` (default `info`). The token and job environment are not logged.

## API

Every route requires `Authorization: Bearer <token>`.

### `GET /health`

```bash
curl -fsS --cacert /etc/shutup-and-run/cert.pem \
  -H "Authorization: Bearer $(sudo sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' /etc/shutup-and-run/env)" \
  https://127.0.0.1:8787/health
```

```json
{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}
```

While draining, `status` is `"draining"` and `draining` is `true`. `503` if Docker cannot be listed: `{"error":"docker is unavailable","draining":false}`.

### `GET /metrics`

Prometheus text. Uptime Kuma or Prometheus must send the bearer token. The installer does not leave this open without TLS.

```text
# HELP shutup_and_run_slots_total Configured slot capacity.
# TYPE shutup_and_run_slots_total gauge
shutup_and_run_slots_total 2
# HELP shutup_and_run_slots_used Running containers labeled shutup-and-run=1.
# TYPE shutup_and_run_slots_used gauge
shutup_and_run_slots_used 1
# HELP shutup_and_run_slots_free Slots not currently taken. Not offered while draining.
# TYPE shutup_and_run_slots_free gauge
shutup_and_run_slots_free 1
# HELP shutup_and_run_draining 1 while new jobs are refused.
# TYPE shutup_and_run_draining gauge
shutup_and_run_draining 0
# HELP shutup_and_run_jobs_started_total Jobs recorded in SQLite, including those still running.
# TYPE shutup_and_run_jobs_started_total counter
shutup_and_run_jobs_started_total 4
# HELP shutup_and_run_jobs_finished_total Jobs that have ended.
# TYPE shutup_and_run_jobs_finished_total counter
shutup_and_run_jobs_finished_total 3
# HELP shutup_and_run_jobs_timeout_total Jobs ended with reason timeout_kill.
# TYPE shutup_and_run_jobs_timeout_total counter
shutup_and_run_jobs_timeout_total 1
```

### `POST /v1/jobs`

```bash
curl -sS --cacert /etc/shutup-and-run/cert.pem -D - -o - -X POST https://192.0.2.11:8787/v1/jobs \
  -H "Authorization: Bearer $SHUTUP_AND_RUN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "image": "ghcr.io/owner/selenium-worker:latest",
    "job_key": "order-1001",
    "env": {"JOB_NAME": "example"},
    "timeout_secs": 3600,
    "cpu": 1,
    "memory": "2g",
    "shm": "2g",
    "callback_url": "https://n8n.example/webhook/shutup-and-run"
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
curl -sS --cacert /etc/shutup-and-run/cert.pem \
  -H "Authorization: Bearer $SHUTUP_AND_RUN_TOKEN" \
  "https://192.0.2.11:8787/v1/jobs/0123abcd?logs=true"
```

The row is in SQLite, so this survives a process restart. `?logs=true` adds `logs` (the last `log_lines` lines). While the container is still running those lines are read from Docker; after it exits they are the copy stored before the container is removed.

```json
{"id":"0123abcd","status":"exited","image":"ghcr.io/owner/selenium-worker:latest","job_key":"order-1001","started_at":1700000000,"ended_at":1700000012,"exit_code":0,"reason":"exited","duration_secs":12,"logs":"..."}
```

`reason` is `exited`, `timeout_kill`, `launcher_kill`, or `start_failed`. `404` is `{"id":"...","status":"not_found"}` for an unknown id or a row that purge has deleted.

When the job ends, shutup-and-run POSTs the same fields (without logs) to `callback_url`. It tries three times, does not follow redirects, and does not hold a slot. Point it at the Kestra or n8n webhook.

### `POST /v1/drain`

```bash
curl -sS -X POST http://192.0.2.11:8787/v1/drain \
  -H "Authorization: Bearer $SHUTUP_AND_RUN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"draining":true}'
```

`200` `{"draining":true}`. `{"draining":false}` leaves drain mode. The flag is stored in SQLite, so a restart stays drained. New jobs get `503`. Running jobs finish.

## Kestra

Create the secret `SHUTUP_AND_RUN_TOKEN`. The task `timeout` is how long Kestra waits for shutup-and-run to answer, not how long the container may run. That duration is `timeout_secs` in the JSON.

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
    uri: https://192.0.2.11:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
      ssl:
        insecureTrustAllCertificates: true
      retryableTransportFailureMethods:
        - POST
      retry:
        type: constant
        interval: PT2S
        maxAttempts: 3
    headers:
      Authorization: "Bearer {{ secret('SHUTUP_AND_RUN_TOKEN') }}"
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
    uri: https://192.0.2.12:8787/v1/jobs
    method: POST
    contentType: application/json
    timeout: PT20S
    allowFailure: true
    options:
      allowFailed: true
      ssl:
        insecureTrustAllCertificates: true
      retryableTransportFailureMethods:
        - POST
      retry:
        type: constant
        interval: PT2S
        maxAttempts: 3
    headers:
      Authorization: "Bearer {{ secret('SHUTUP_AND_RUN_TOKEN') }}"
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

Set `SHUTUP_AND_RUN_TOKEN` on the n8n process and allow environment access (`N8N_BLOCK_ENV_ACCESS_IN_NODE=false`), or replace the header expression with a Header Auth credential. `job_key` is `$execution.id`. Put your webhook URL in `callback_url`. The exported nodes set `allowUnauthorizedCerts` for the installer's self-signed certificate; turn that off if you replace the cert with one n8n already trusts, and compare the fingerprint the installer printed.

## Pull

Leave `poll_url` unset and the process only wakes up on `POST /v1/jobs`. Set it when this machine cannot be reached from outside. The HTTP server stays up either way.

On each interval, if a slot is free and the process is not draining, it sends:

```http
POST {poll_url}
Accept: application/json
Content-Type: application/json
Authorization: Bearer {SHUTUP_AND_RUN_POLL_TOKEN}

{}
```

The Authorization header is omitted when `SHUTUP_AND_RUN_POLL_TOKEN` is unset. Redirects are not followed. The timeout is 10 seconds. A body over 64 KiB is an error. Startup logs the host only, not the URL, so a webhook key in the path is not written to the log line. Do not put a secret in the URL userinfo; that form is rejected. Put `SHUTUP_AND_RUN_POLL_URL` and `SHUTUP_AND_RUN_POLL_TOKEN` in `/etc/shutup-and-run/env` (the unit already loads that file) when the URL itself is a secret. `poll_token` is not a TOML key.

Nothing to do, logged at debug, next wait is the normal interval:

- `204`
- `200` with an empty body, `null`, `{}`, or `{"task": null}`

A task is `200` and either the same JSON object as `POST /v1/jobs`, or `{"task": { ...that object, and no sibling fields... }}`. Unknown fields are rejected. The object goes through the same checks as POST: allowlist, `job_key`, limits, `callback_url`, then start. A refusal is a warning. The process stays up. `job_key` idempotency matches POST, including a failed start keeping the key.

Offering a task is not an acknowledgement. The launcher may still refuse if the slot is taken between the check and the start, or if the image is not allowlisted. A refusal other than a failed start does not store the key. Keep offering the same `job_key` until the callback arrives if the job must land. Once it has started, further offers of that key do not start a second container.

Any other outcome (connection failure, timeout, a redirect, any status other than 200 or 204) is a warning. The wait doubles, up to 300 seconds, or stays at `poll_interval_secs` when that is already longer. The next empty response or accepted task resets the wait. The process does not exit.

`examples/kestra-poll.yml` is a Kestra webhook with `wait` and `returnOutputs`. Kestra's body is the flow outputs, so the output id is `task` and the body is `{"task": null}` or `{"task": {image, ...}}`. The webhook key is in the URL path. Kestra does not read the bearer header.

`examples/n8n-poll.json` is an n8n Webhook plus Respond to Webhook. A matching bearer token returns one static task so you can see the shape. Replace that object with your queue, and return `{}` when the queue is empty. Set `SHUTUP_AND_RUN_POLL_TOKEN` on the n8n process (`N8N_BLOCK_ENV_ACCESS_IN_NODE=false`).

## Acceptance test

On Ubuntu with Docker, preferably with no other shutup-and-run and no container already labeled `shutup-and-run=1`:

```bash
sudo bash scripts/accept.sh
sudo bash scripts/accept.sh /path/to/shutup-and-run-x86_64-unknown-linux-musl
```

With systemd it installs `/usr/local/bin/shutup-and-run`, starts `shutup-and-run-accept.service`, and removes that unit at the end. The binary stays. It does not touch a production `shutup-and-run.service`. You want:

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
PASS: banned after 3 failures
PASS: ban lifted
RESULT: all passed
```

## Limits

- One process per Docker daemon. Two processes can exceed `max_slots`.
- Stopping the service does not kill containers.
- A timeout is SIGKILL, with no SIGTERM.
- `job_key` does not cross machines.
- Pruning is off unless `prune_interval_secs` is set. It removes stopped containers with the shutup-and-run label, and images recorded in this SQLite file that no container is using. It does not delete images shutup-and-run never launched.
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

License: MIT. See [CONTRIBUTING.md](CONTRIBUTING.md). Decisions: [docs/DECISIONS.md](docs/DECISIONS.md). Operator conventions: [docs/CONVENTIONS.md](docs/CONVENTIONS.md). A future Windows and macOS client: [docs/ROADMAP-windows-macos.md](docs/ROADMAP-windows-macos.md). Notes for a reviewer: [docs/HANDOFF.md](docs/HANDOFF.md).

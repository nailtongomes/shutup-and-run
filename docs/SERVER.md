# A dedicated Linux server

One Linux machine that does nothing but run jobs, many at once, for one orchestrator that pushes them. For example: a Flask API with a queue for about 200 people, about 1000 jobs a day, one Selenium container per job (1 CPU, 1.5 GiB, about 5 minutes), on a 48-core, 128 GiB server with an NVMe disk and native Docker.

This is the `server` resource profile (0.2.34; push only, prepared images and the cleanup since 0.2.35). A client's PC (Windows, WSL2, `shared-pc`) is unchanged by it, and so is a Linux machine that does not ask for it.

The orchestrator decides what runs and when, keeps the queue, the state and the retries. The server starts a container, says no at once when it cannot, and reports how each job ended. Everything below follows from that split. The containers are workers that know nothing of each other: one per request, it starts, runs and is removed. When the server is full, the orchestrator sends the rest elsewhere (a serverless overflow, outside this repository).

## The daily cycle

```text
once a day        POST /v1/images/pull {"image": ...}     GET /v1/images/pull?image=... until "ready"
every ~4 minutes  up to 200 x POST /v1/jobs                 202 accepted / 200 already here / 429, 409, 503 refused
when idle         the cleanup (Sunday 01-03 by itself, or POST /v1/maintenance/cleanup)
any time          GET /health                               slots, disk, memory, what is short, the last cleanup
```

1. **Prepare the image once a day.** `POST /v1/images/pull` starts the download and answers `202` at once. `GET /v1/images/pull?image=NAME` says `pulling`, `ready`, `failed` (with `error`) or `absent`. Under the server's `pull_policy` `never`, a job never pulls: a job whose image is not on the machine is refused with `409` `IMAGE_NOT_PRESENT`, so a burst never waits on a registry. Preparing a moving tag (`:latest`) every day is how the jobs follow it; the old version, untagged, goes at the next cleanup once no container uses it.
2. **Bursts.** The orchestrator sends what it has queued, up to 200 pushes at once, every few minutes. Each has `image`, `env`, `job_key` (the task and its attempt) and `concurrency_key` (whose work it is: the lawyer). The server accepts while it has free slots and answers the rest at once with the reason; it keeps no queue of its own. Answers may take a moment in a burst: admission is one at a time.
3. **The repeat is harmless.** The next burst sends the same tasks again. A `job_key` already here answers `200` with its job and starts nothing; a new attempt of a lawyer whose two jobs still run is `429` `CONCURRENCY_KEY_BUSY`; a full machine is `429` `no free slots`. Nothing is stored for a refusal.
4. **Cleanup.** Inside `cleanup_hours` (`sun 01-03` unless set; `01-03` for every night), once per window, after 30 minutes with no job running or starting and no image being prepared: stopped containers of jobs that ended, images no job used or prepared for 30 days, untagged images, anonymous volumes no container uses, job logs older than 7 days. `POST /v1/maintenance/cleanup` does the same now, or answers `409` `JOBS_RUNNING` with nothing done.

`tests` in `src/api.rs` run that day on a fake Docker: 200 concurrent pushes against 20 slots that start 5 at a time are exactly 20 accepted and 180 refused with a reason, no slot counted twice, the same burst again starts nothing, every slot comes back after the cancels, and the env values reach the container and no answer, metric, log line or callback.

## What the profile changes

| | `none` (Linux default) | `shared-pc` (Windows default) | `server` |
| --- | --- | --- | --- |
| CPU per job (`max_job_cpus`) | no limit | 1 | 1 |
| Memory per job (`max_job_memory`) | no limit | 2g | 1.5g |
| `/dev/shm` of a job that names none (`default_job_shm`) | Docker's 64m | Docker's 64m | 1g |
| Memory a new job needs free (`min_free_memory`) | not checked | 2g | a sixteenth of the memory, 2g to 8g (8g of 128) |
| Disk a new job needs free (`min_free_disk`) | not checked | 5g | 10g |
| Memory limits of the jobs that have not ended add up (`JOBS_MEMORY_COMMITTED`) | no | yes | yes |
| Jobs starting at once (`max_starting`) | no limit | no limit | 5 |
| Jobs at once per `concurrency_key` (`max_jobs_per_key`) | 2 | 2 | 2 |
| Finished jobs kept in the history | 500 | 500 | 50 000 |
| Safety assessment and the owner's consent | no | yes | no: nobody uses the machine |
| A job's image (`pull_policy`) | `always` pulled | `always` pulled | `never`: prepared ahead, else `409` |
| Poll URL (`poll_url`) | allowed | allowed | refused: push only |
| Cleanup | once a day, images unused for 30 days | once a day, also untagged images, then `fstrim` | `cleanup_hours` when idle, and `POST /v1/maintenance/cleanup` |

Each figure is a key of its own and can be set under any profile, except two: `poll_url` cannot be set on a server at all (the config is refused, from the file or the environment), and `cleanup_hours` is a server's only. `pull_policy` is `always`, `missing` (pull only what is not here) or `never`.

How the numbers work on 128 GiB:

- The jobs may commit 128 − 8 = 120 GiB together. At 1.5 GiB each that is 80 jobs, whatever `max_slots` says.
- The installer sets `max_slots` to the lesser of that and 1.5 jobs per CPU: 72 on 48 cores. A browser job waits on pages most of the time, so a CPU per job is a ceiling, not a reservation.
- With `max_starting` 5, a burst of 75 pushes is accepted at once and starts five at a time. Each start counts for 20 seconds after its container starts (`launcher::STARTING_SECS`, a constant). That is about five minutes to fill the machine, and no second in which 75 browsers start together.

## Install over SSH

Ubuntu 22.04 or 24.04 (cgroup v2), root over SSH. Nothing asks a question. Every step has a command whose output says whether it worked.

1. **Install.** One line. Put your image in the allowlist and your orchestrator's address in `ALLOW_FROM` (the addresses here are made up):

   ```bash
   curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh \
     | sudo SHUTUP_AND_RUN_PROFILE=server \
            SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/nailtongomes/rpa-worker-selenium \
            SHUTUP_AND_RUN_ALLOW_FROM=203.0.113.20 bash
   ```

   It installs Docker if it is missing, writes `/etc/shutup-and-run/config.json` (mode 600) with a new token and a new admin password, makes a self-signed certificate, and installs the systemd unit `shutup-and-run` with `Restart=always`. It caps the journal at 1 GiB (`/etc/systemd/journald.conf.d/shutup-and-run.conf`). Every container's own log is already capped by the worker at two files of 5 MB. With `ufw` active, port 8787 is opened to `ALLOW_FROM` only.

   Optional variables:

   - `SHUTUP_AND_RUN_MAX_SLOTS=75` sets the number of slots instead of computing it.
   - `SHUTUP_AND_RUN_BIND=127.0.0.1` listens on loopback only (see [Reaching the API](#reaching-the-api)).
   - `SHUTUP_AND_RUN_PORT` changes the port.

   Run the same line again to update the binary: it keeps `config.json`.

   The server takes pushes only. A `poll_url` in its config, or `SHUTUP_AND_RUN_POLL_URL`, is refused when the config is read: there is no way to turn the poll on under this profile.

2. **Check it.** All three should succeed:

   ```bash
   sudo shutup-and-run doctor --json | jq '{ok, worker_ready, codes: [.checks[] | select(.status != "ok") | .code]}'
   sudo shutup-and-run config show --json | jq '{valid, effective}'
   curl -s --cacert /etc/shutup-and-run/cert.pem -H "Authorization: Bearer $(sudo shutup-and-run config get token --reveal)" https://127.0.0.1:8787/health
   ```

   `doctor` exits 0 when the machine can run jobs. On a server it also reports:

   - `docker`: whether Docker answers (`DOCKER_UNAVAILABLE`).
   - `cgroup`: v2, or v1 with a warning (`CGROUP_V1`).
   - `resources`: memory and disk free now, and slots against the committed memory (`SLOTS_EXCEED_MEMORY`).
   - `service`: running, failed, or its port taken (`SERVICE_PORT_IN_USE`).

   Each code that is not ok carries one next step in `hint`.

3. **Change a setting.** `config set` validates before it writes, and `--apply` restarts the service:

   ```bash
   sudo shutup-and-run config set max_slots 75 --apply
   sudo shutup-and-run config set pull_policy missing --apply
   sudo shutup-and-run config set allowlist ghcr.io/nailtongomes/rpa-worker-selenium --apply
   sudo shutup-and-run config keys --json      # every key, its type and where it is edited
   ```

4. **Log in to the registry**, if the image is private: `sudo docker login ghcr.io` with a read-only token. A request never carries registry credentials.

5. **Hand the orchestrator three values:**
   - the URL (`https://SERVER_IP:8787`);
   - the token (`sudo shutup-and-run config get token --reveal`);
   - the certificate `/etc/shutup-and-run/cert.pem`, to verify against.

   Keep the token in the orchestrator's secret store, not in its code.

6. **Prepare the image**, then check that it is there:

   ```bash
   TOKEN=$(sudo shutup-and-run config get token --reveal)
   curl -s --cacert /etc/shutup-and-run/cert.pem -H "Authorization: Bearer $TOKEN" \
     -d '{"image":"ghcr.io/nailtongomes/rpa-worker-selenium:latest"}' https://127.0.0.1:8787/v1/images/pull
   curl -s --cacert /etc/shutup-and-run/cert.pem -H "Authorization: Bearer $TOKEN" \
     'https://127.0.0.1:8787/v1/images/pull?image=ghcr.io/nailtongomes/rpa-worker-selenium:latest'
   ```

### A config for 128 GiB

What the installer writes, plus the keys you would most often set. The values are examples; the token and the password are generated on the machine and never leave it.

```json
{
  "listen": "0.0.0.0:8787",
  "token": "GENERATED_ON_THE_MACHINE",
  "admin_password": "GENERATED_ON_THE_MACHINE",
  "tls_cert": "/etc/shutup-and-run/cert.pem",
  "tls_key": "/etc/shutup-and-run/key.pem",
  "history_path": "/var/lib/shutup-and-run/jobs.sqlite",
  "status_page": true,
  "resource_profile": "server",
  "allowlist": ["ghcr.io/nailtongomes/rpa-worker-selenium"],
  "max_slots": 75,
  "max_job_cpus": "1",
  "max_job_memory": "1536m",
  "default_job_shm": "1g",
  "min_free_memory": "8g",
  "min_free_disk": "20g",
  "max_starting": 5,
  "max_jobs_per_key": 2,
  "pull_policy": "never",
  "cleanup_hours": "sun 01-03",
  "default_timeout_secs": 900,
  "max_timeout_secs": 3600
}
```

`pull_policy: never` (the server's default) means no job ever talks to the registry: the thousand jobs a day use what the daily preparation brought. `missing` pulls an image a job names that is not here, while the job waits holding its slot. `always` asks the registry for every job's tag, and a registry that does not answer falls back to the local image.

## Reaching the API

Every route needs the token. Three wrong tokens from one address ban it for 48 hours, the right one included (`sudo shutup-and-run bans list`, `bans lift IP`).

- **The default:** the API listens on `0.0.0.0:8787` over TLS with the self-signed certificate. `ufw` lets in only `ALLOW_FROM`. If the provider has its own firewall (EVEO's panel), open 8787 there to the orchestrator's address only as well.
- **The orchestrator on the same machine, or a private network:** `SHUTUP_AND_RUN_BIND=127.0.0.1` at install, or `config set listen 10.0.0.5:8787 --apply` for a private interface. A WireGuard tunnel between the two machines is the same.
- **Behind a reverse proxy:** only one you run, restricted to the orchestrator. Set `trust_x_forwarded_for` only when that proxy sets the header, or every address is the proxy's and one bad token bans all of them.
- **Verify, do not skip:** copy `/etc/shutup-and-run/cert.pem` to the orchestrator and verify against it. Python: `requests.post(..., verify="sar-cert.pem")`. The certificate names the server's address as the installer saw it. A different address needs a new certificate: delete `cert.pem` and `key.pem`, then run the install line again.

### The status page

A page of slots in use and free, the jobs running with what each uses, the last jobs and their logs. It answers only on the machine itself, never on the network: that is by design, and the API above is what the network gets. From your computer, through SSH:

```bash
ssh -N -L 8787:127.0.0.1:8787 root@SERVER_IP
# then open https://127.0.0.1:8787/ (the browser warns about the self-signed certificate)
```

The settings behind "Advanced" ask for the admin login: `admin`, and the password from `sudo shutup-and-run config get admin_password --reveal`. Change either there, or with `config set admin_user` / `config set admin_password`. Whoever has the SSH key already has the machine. The login keeps the page's dangerous settings away from someone who only has the tunnel.

For a program, the same is in the API. `GET /v1/jobs?state=running` lists each running job. `GET /health` answers the slots and the machine:

```json
{"status":"ok","draining":false,"mode":"jobs","slots_total":75,"slots_used":12,"slots_free":63,
 "machine":{"profile":"server","cpus":48,"load1":6.2,
            "memory_total_bytes":135089553408,"memory_available_bytes":98784247808,"min_free_memory_bytes":8589934592,
            "disk_free_bytes":512110190592,"min_free_disk_bytes":10737418240,
            "shortage":null,"last_cleanup":{"at":1700000000,"images":2,"freed_bytes":3221225472}}}
```

`disk_free_bytes` is the disk Docker writes to. `shortage` is what blocks a new job right now (`{"resource":"disk","free_bytes":...,"needed_bytes":...}`), the same as the `503` a push would get. Alert on it, and on `disk_free_bytes` getting near `min_free_disk_bytes`.

## Pushing a job

```bash
curl -s --cacert sar-cert.pem -X POST https://SERVER_IP:8787/v1/jobs \
  -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' \
  -d '{
    "image": "ghcr.io/nailtongomes/rpa-worker-selenium:2026-11-27",
    "job_key": "req-1042-attempt-1",
    "concurrency_key": "account-0007",
    "env": {"SCRIPT_URL": "https://scripts.example.com/protocolar.py", "TASK_ID": "1042",
            "USE_XVFB": "1", "USE_OPENBOX": "1"},
    "timeout_secs": 900,
    "callback_url": "https://api.example.com/sar/callback/REPLACE_WITH_CALLBACK_SECRET"
  }'
```

- `job_key` is your task and its attempt. It makes a repeat of the same push harmless: the second one answers `200` with the first job.
- `concurrency_key` is your own id for whose work it is: a lawyer, a login, an account. It must be an opaque id (`account-0007`), never a person's tax id, bar number or e-mail. At most `max_jobs_per_key` (2) jobs with the same key run at once. Use it when two sessions of one login would knock each other out.
- `cpu`, `memory` and `shm` can be left out: the machine's 1 CPU, 1.5 GiB and 1 GiB apply. A job may ask for less, or for up to the machine's limit. More than the limit is a `400`, never cut down.
- The full field list and every answer: [INTEGRATION-CONTRACT.md](INTEGRATION-CONTRACT.md).

n8n: an HTTP Request node with `fullResponse` and `neverError`, routing on `statusCode` ([the contract's example](INTEGRATION-CONTRACT.md#examples)). Kestra: [examples/kestra-dispatch.yml](../examples/kestra-dispatch.yml).

### The answers and what the orchestrator does

Every refusal of `POST /v1/jobs` has `reason` (0.2.35), one stable word, the same as the label of `shutup_and_run_refusals_total`. Route on it, not on `error`'s text.

| Answer | `reason` | Means | The orchestrator |
| --- | --- | --- | --- |
| `202` | | Accepted, a slot is held, the job will start (it may first wait its turn under `max_starting`). | Store `id`, mark the task as sent. |
| `200` | | This `job_key` is already here (running or ended). | Same as `202`; read `status`. Nothing started. |
| `429` | `no_slots` | Every slot is taken. | Keep it queued for the next burst, or send it to the overflow. |
| `429` | `concurrency_key_busy` | Two jobs of this lawyer (`concurrency_key`) have not ended. | Keep it queued, behind the other lawyers. |
| `409` | `image_not_present` | The image was not prepared (`code` `IMAGE_NOT_PRESENT`, with `image`). | Prepare it (`POST /v1/images/pull`); send it after `ready`. Alert: the daily step failed. |
| `503` | `memory_committed` | The memory limits of the running jobs leave no room for this one's. | Keep it queued; it clears when a job ends. |
| `503` | `low_disk`, `low_memory` | Disk or memory free is below the minimum now (`resource`, `free_bytes`, `needed_bytes`). | Keep it queued or overflow; alert: `GET /health` says how much. |
| `503` | `draining` | Paused for maintenance. | Keep it queued or overflow. |
| `400` | `bad_request` | The request is wrong, or asks for more than the machine allows. | Do not retry it: fix the task. |
| `403` | `not_allowlisted` | The image is not on the allowlist. | Fix the config or the image name. |
| `401` | | Wrong token. | Stop before the third (a ban). |
| `502` | `unavailable` | Docker or the history did not answer. | Keep it queued; alert. |

Nothing is stored for a refusal. The same `job_key` can be sent again, here or elsewhere.

## State, checkpoints and the callback

The server keeps no queue, no task state and no checkpoint. It does not retry a job, start a refused one later, or reconcile what it ran with what the orchestrator thinks. Those belong to the orchestrator (Flask, with Redis or its database) and to the worker inside the container:

- **Checkpoints are the worker's.** A job that may be killed (timeout, cancel, a restart) writes its progress where the next attempt reads it, for example a Redis key per task (`task:1042:step`), through the `env` it got (`REDIS_URL` and a key, never a password in the image). The server neither reads nor keeps any of it.
- **Deduplication is `job_key` and the orchestrator's.** The server only promises that one `job_key` starts at most one container while its row is in the history (`history_days`).
- **The callback is best effort.** It is sent when the job ends and repeated until a `2xx` or for one hour, and then given up. A server restart, a network cut or a receiver down for an hour loses it. The orchestrator must not depend on it alone: a task it sent and heard nothing about is asked with `GET /v1/jobs/{id}`, or simply sent again with the same `job_key` (`200` with the job as it is).

### Return to the queue (in the orchestrator)

The queue's states live in the orchestrator's own database, for example:

```text
queued ──push 202/200──▶ leased(job_id, lease_until) ──callback──▶ done | failed | queued (requeue)
   ▲                         │
   └──── 429 / 503 / 502 ────┘ (nothing was stored on the server)
```

- **The lease.** `lease_until` = now + `timeout_secs` + 35 minutes. That covers the pull limit of 30 minutes, a wait under `max_starting`, and a callback's retries. Most jobs end long before.
- **A lost callback.** When a lease expires without a callback, ask the server: `GET /v1/jobs/{id}`. `status` `exited` is the end you missed (the callback is the same body). `running` means it is still running. `404` means the history has forgotten it (after `history_days`): treat that as unknown. A sweep every few minutes over `GET /v1/jobs?state=running` finds leases whose job the server no longer runs.
- **Retry with a new key.** A second attempt is `req-1042-attempt-2`: a `job_key` that already ran returns the old job and starts nothing.
- **Cancel.** `DELETE /v1/jobs/{id}` (below), then mark the task canceled. Its callback still arrives, with `reason` `launcher_kill`.

### The callback and the agreed exit code

The callback is `POST callback_url` with the job as it ended. It is sent once and repeated until a `2xx` or one hour:

```json
{"id":"5f479abd3087cf193ed4c1da8460784d","job_key":"req-1042-attempt-1","image":"ghcr.io/nailtongomes/rpa-worker-selenium:2026-11-27","status":"exited","exit_code":75,"reason":"exited","started_at":1700000000,"ended_at":1700000042,"duration_secs":42}
```

`reason` says how it ended. `exit_code` says what the job itself said:

| `reason` | `exit_code` | Outcome | The orchestrator |
| --- | --- | --- | --- |
| `exited` | `0` | ok | done |
| `exited` | `75` | the login is busy (agreed below) | back in the queue, after a pause |
| `exited` | anything else | failed | failed, or a new attempt if the job is safe to repeat |
| `timeout_kill` | `137` | timeout | failed by timeout; look at the logs |
| `launcher_kill` | `137`, or `null` if it had not started | canceled (`DELETE`) | canceled |
| `start_failed` | `null` | never ran (pull or start failed) | safe to retry; alert if it repeats |
| `interrupted` | `null` or not `0` | unknown: the server or Docker stopped under it | check the business system before a retry |

A job that was refused never gets a callback. It was never stored, and the refusal was the push's own answer (`429`, `503`, `400`).

`oom_killed: true` comes along when the job hit its memory limit. Send it again with more `memory`, or less work.

**The agreed exit code for "login busy" is 75** (`EX_TEMPFAIL` in `sysexits.h`: "try again later"). It is a convention between your script and your orchestrator. The server passes it through and does nothing else with it. In the container's script:

```python
import sys

EXIT_LOGIN_BUSY = 75  # agreed with the orchestrator: put the task back in the queue

def main() -> int:
    session = login()
    if session.already_in_use():
        print("login busy: another session is open for this account")
        return EXIT_LOGIN_BUSY
    run_task(session)
    return 0

if __name__ == "__main__":
    sys.exit(main())
```

In the Flask app:

```python
from flask import Flask, request, abort

app = Flask(__name__)
CALLBACK_SECRET = load_secret("SAR_CALLBACK_SECRET")  # from your secret store
EXIT_LOGIN_BUSY = 75

@app.post("/sar/callback/<secret>")
def sar_callback(secret: str):
    if secret != CALLBACK_SECRET:
        abort(404)
    job = request.get_json(force=True)
    if already_handled(job["id"]):          # a callback can arrive twice
        return "", 204
    task = task_by_job_key(job["job_key"])  # "req-1042-attempt-1" -> task 1042
    reason, code = job["reason"], job.get("exit_code")
    if reason == "exited" and code == 0:
        task.mark_done()
    elif reason == "exited" and code == EXIT_LOGIN_BUSY:
        task.requeue(delay_secs=120, same_attempt=False)
    elif reason == "launcher_kill":
        task.mark_canceled()
    elif reason == "start_failed":
        task.requeue(delay_secs=30, same_attempt=False)
    elif reason == "interrupted":
        task.mark_unknown()                 # check the portal before trying again
    else:                                   # exited non-zero, timeout_kill
        task.mark_failed(reason=reason, exit_code=code)
    remember_handled(job["id"])
    return "", 204                          # answer at once; work afterwards
```

And the push, with requests:

```python
import requests

SAR_URL = "https://203.0.113.10:8787"
SAR_TOKEN = load_secret("SAR_TOKEN")
SAR_CERT = "/etc/myapp/sar-cert.pem"        # copied from the server

IMAGE = "ghcr.io/nailtongomes/rpa-worker-selenium:latest"   # prepared once a day

def push(task) -> str:
    body = {
        "image": IMAGE,
        "job_key": f"req-{task.id}-attempt-{task.attempt}",
        "concurrency_key": f"account-{task.account_id}",
        "env": task.env(),
        "timeout_secs": 900,
        "callback_url": f"https://api.example.com/sar/callback/{CALLBACK_SECRET}",
    }
    r = requests.post(f"{SAR_URL}/v1/jobs", json=body, timeout=20, verify=SAR_CERT,
                      headers={"Authorization": f"Bearer {SAR_TOKEN}"})
    if r.status_code in (200, 202):
        task.lease(job_id=r.json()["id"], secs=900 + 35 * 60)
        return "leased"
    reason = r.json().get("reason", "") if r.headers.get("content-type", "").startswith("application/json") else ""
    if reason == "image_not_present":
        prepare_image(IMAGE)                # and alert: the daily step did not run
    if r.status_code in (409, 429, 503, 502):
        task.requeue(delay_secs=15)         # nothing was stored; the next burst sends it again
        return reason or "busy"
    task.mark_failed(reason=f"refused {r.status_code} {reason}")
    return "refused"

def prepare_image(image: str) -> None:     # once a day, before the first burst
    requests.post(f"{SAR_URL}/v1/images/pull", json={"image": image}, timeout=20, verify=SAR_CERT,
                  headers={"Authorization": f"Bearer {SAR_TOKEN}"})
```

A burst is the same function over the queued tasks, in a thread pool of, say, 20. A refusal costs a millisecond and stores nothing, so sending the whole queue every 4 minutes is fine.

A push that times out may have been accepted: send the same `job_key` again before you move on. `200` or `202` both mean it is there.

## Cancel, list, metrics

```bash
# Cancel: SIGTERM, SIGKILL 10 s later. 200 with the ended job; 202 if it had
# not started yet (it ends in a moment); 404 for an unknown id. Repeating it is harmless.
curl -s --cacert sar-cert.pem -X DELETE -H "Authorization: Bearer $SAR_TOKEN" https://SERVER_IP:8787/v1/jobs/ID

# What runs now, oldest first.
curl -s --cacert sar-cert.pem -H "Authorization: Bearer $SAR_TOKEN" 'https://SERVER_IP:8787/v1/jobs?state=running'
```

```json
{"state":"running","slots_total":75,"slots_used":2,"slots_free":73,"jobs":[
  {"id":"5f47...","image":"ghcr.io/nailtongomes/rpa-worker-selenium:2026-11-27","job_key":"req-1042-attempt-1",
   "concurrency_key":"account-0007","phase":"running","started_at":1700000000,
   "cpu_ms":41200,"memory_peak_bytes":812000000,"network_bytes":5300000,
   "pull_done_bytes":null,"pull_total_bytes":null}]}
```

`phase` is `starting` while the job waits its turn or pulls its image, then `running`. `cpu_ms`, `memory_peak_bytes` and `network_bytes` come from one reading every 30 seconds: `null` in a job's first seconds.

`GET /metrics` (Prometheus text, with the token):

| Metric | |
| --- | --- |
| `shutup_and_run_slots_total`, `_slots_used`, `_slots_free`, `_draining` | Slots. |
| `shutup_and_run_jobs{state="starting"\|"running"}` | Jobs that have not ended. |
| `shutup_and_run_jobs_ended{reason="..."}` | Ended jobs in the history, by reason. |
| `shutup_and_run_refusals_total{reason="..."}` | Refusals since the service started, by the `reason` of the answer: `no_slots`, `concurrency_key_busy`, `image_not_present`, `memory_committed`, `low_memory`, `low_disk`, `draining`, `bad_request`, `not_allowlisted`, `unavailable`. |
| `shutup_and_run_pull_seconds_sum`, `_count` | Image pulls and their time, the preparations included. |
| `shutup_and_run_pulls_skipped_total` | Jobs that did not pull: `pull_policy` `never`, or `missing` with the image here. |
| `shutup_and_run_start_seconds_sum`, `_count` | From acceptance to a running container: the wait for `max_starting`, the pull, the create. |

The refusal and timing counters are in memory and start again at zero when the service restarts, like any Prometheus counter.

## Shared memory (`/dev/shm`)

Chrome keeps its rendering buffers in `/dev/shm`, and Docker's 64 MiB makes tabs crash ("Aw, Snap!", `session deleted because of page crash`). The server gives every job 1 GiB unless the request names `shm`.

- The pages written to `/dev/shm` count against the job's memory limit: 1 GiB of shm used inside a 1.5 GiB job leaves 0.5 GiB for everything else. A browser rarely fills it. A job that does needs `memory` of 2g, or Chrome's `--disable-dev-shm-usage` (it then writes to `/tmp`, on the container's disk).
- `shm` is not part of the committed memory sum; `memory` is the ceiling that includes it.
- `default_job_shm` sets the default for the machine (`"0"`: Docker's 64m). A request's `shm` wins, with no ceiling of its own beyond `memory`.

## Updating without losing a job

```bash
curl -s --cacert sar-cert.pem -X POST -H "Authorization: Bearer $SAR_TOKEN" -d '{"draining":true}' https://SERVER_IP:8787/v1/drain
# wait for slots_used 0 in /health, then on the server:
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_PROFILE=server bash
curl -s --cacert sar-cert.pem -X POST -H "Authorization: Bearer $SAR_TOKEN" -d '{"draining":false}' https://SERVER_IP:8787/v1/drain
```

A drain answers every push `503` `draining`, so the orchestrator keeps its tasks queued. A restart without a drain also keeps running containers: the service finds them again when it comes back, and only enforces their timeouts again from then on.

## What is not in the binary, and why

- **A poll, faster or not.** Polling is for a machine the orchestrator cannot reach. This one it reaches, and a push is answered in milliseconds. A server refuses `poll_url`; the 30-second minimum stays for the machines that do poll.
- **An allowlist by prefix.** An entry is one repository and already allows all its tags and digests (`ghcr.io/nailtongomes/rpa-worker-selenium` allows `:latest`, `:2026-11-27` and `@sha256:...`). A prefix would also allow `ghcr.io/nailtongomes/rpa-worker-selenium-evil`; list each repository instead.
- **A guaranteed callback, reconciliation, leases.** The callback is best effort (above). The orchestrator's database is the record of what it sent; the server's history is a help for a person, not a second source of truth.
- **A queue, retries, a later start of a refused job.** The orchestrator's queue already exists. A second one on the server would disagree with it after the first crash.
- **Several servers, autoscaling, billing.** Out of scope. Each server is its own `job_key` namespace; which server got which task is the orchestrator's record.
- **The status page on the network.** It edits the config. The API with its token is what the network gets; the page is reached through SSH.

# n8n as the orchestrator

Two workflows that speak the runner's existing contract, and a smoke test for a Linux server and a Windows PC. The contract itself is [docs/INTEGRATION-CONTRACT.md](../../docs/INTEGRATION-CONTRACT.md); this is one way to implement its server side.

| File | What it is |
| --- | --- |
| [worker-orchestrator.json](worker-orchestrator.json) | The poll URL for workers behind NAT, the manifest URL, the callback receiver, and a small admin webhook to approve machines and queue jobs. |
| [push-to-linux.json](push-to-linux.json) | One job sent from n8n to a Linux runner with `POST /v1/jobs`. |

Every token, URL and customer identifier in them is a placeholder. Nothing here is a production secret, and the workflows hold no business logic: they move a `JobRequest` to a machine and record how the container ended.

Checked on n8n 2.35.7: both workflows import, the first one was driven by the runner's own poll client through enrollment, approval, a direct task, a manifest and two callbacks, and the second got its `202` from the runner's HTTP API. Containers were not part of that check. The procedure at the end of this page is the one that runs a real container.

## What n8n does and what the runner does

| | n8n | runner |
| --- | --- | --- |
| Which machines exist, and which are approved | yes | no |
| The queue, and which machine gets which job | yes | no |
| Retries, and whether a retry is safe | yes | no |
| Start one container, enforce slots and the timeout | no | yes |
| Remember a `job_key` on that one machine | no | yes |
| Report how the container ended | no | yes |

## 1. Import

In n8n: **Workflows**, **Add workflow**, the menu at the top right, **Import from File**, once for each JSON file. From a shell on the n8n host it is `n8n import:workflow --input=worker-orchestrator.json`.

They use only built-in nodes: Webhook, Crypto, Code, Switch, Respond to Webhook, Edit Fields, HTTP Request. No credential, no community node, no environment variable.

## 2. Set the placeholders

Each of the four Code nodes in `worker-orchestrator.json` starts with the same `CFG` block. Set the same values in all four.

| Placeholder | Set it to |
| --- | --- |
| `https://n8n.example.com/webhook` | The address a worker can reach your n8n at, ending in `/webhook`. It must be the same scheme, host and port as the poll URL you give the workers, or they will fetch manifests without their token. |
| `REPLACE_WITH_CALLBACK_SECRET` | A long random string: `openssl rand -hex 32`. It goes in every `callback_url`, because the runner adds no header to a callback. |
| `REPLACE_WITH_ADMIN_TOKEN` | Another one. It is yours. No worker ever sees it. |

In `push-to-linux.json`, the **Job** node holds `runner_url`, `runner_token` (the Linux machine's `SHUTUP_AND_RUN_TOKEN`) and the `callback_url`.

In real use, move these out of the workflow: n8n credentials, or environment variables read with `$env`.

## 3. Activate

Activate (publish) `worker-orchestrator`. Two reasons it must be active and not run with **Test workflow**:

- The production URLs are `/webhook/...`. A test run listens on `/webhook-test/...` for one call only.
- The workflow keeps its state in workflow static data, which n8n saves only for an active workflow.

The four URLs, with your own host:

| | |
| --- | --- |
| `POST https://n8n.example.com/webhook/n3-worker-poll` | The `poll_url` you give to workers. |
| `GET https://n8n.example.com/webhook/n3-manifest?job_key=...` | Fetched by a worker that was answered with a manifest. |
| `POST https://n8n.example.com/webhook/n3-callback?secret=...` | The `callback_url` of every job. |
| `POST https://n8n.example.com/webhook/n3-admin` | Yours: list, approve, revoke, enqueue. |

The poll URL must have a certificate from a public authority. The runner does not read the machine's certificate store, so a private authority fails.

## 4. What the poll workflow does

```text
Worker poll ─ Hash poll token ─ Identify, authorize, pick a job ─ Poll answer ─┬─ 403 not approved
 (Webhook)     (Crypto)          (Code)                            (Switch)    ├─ 204 no job
                                                                               ├─ 200 task
                                                                               └─ 200 manifest_url
```

1. **Webhook for worker polling.** `POST n3-worker-poll`. The body is the [worker poll request](../../docs/schemas/worker-poll-request.schema.json).
2. **Bearer-token validation.** There must be an `Authorization: Bearer` header with at least 16 characters. The Crypto node turns the token into its SHA-256, and only the hash is kept.
3. **Worker identification.** The hash is the machine. `worker`, `os` and the rest of the body are recorded beside it as what the machine says about itself. The address comes from `CF-Connecting-IP` when the request came through Cloudflare, then from `X-Real-IP`, then from the first entry of `X-Forwarded-For`, which your reverse proxy sets. Never from the body.
4. **Authorization decision.** A hash that was never seen becomes a `pending` record and gets `403`. So does a `pending` or `revoked` one. A `403` has no body: no task, no image name, no secret. At most 200 pending records are kept.
5. **No-job response.** `204`, when the machine is approved and nothing is queued for it.
6. **Job response.** `200` `{"task": {...}}`. The workflow adds `job_key` and `callback_url` to the task you queued.
7. **Manifest response.** `200` `{"manifest_url": ".../n3-manifest?job_key=..."}`, for a job queued with `via_manifest`. The **Manifest** webhook then serves the `JobRequest`, and only to the token the job was offered to.

**Callback receiver.** `POST n3-callback?secret=...`. A wrong secret is `403`. Otherwise it answers `204` at once and records the result once per job `id`, so a callback that arrives twice changes nothing.

**Offer policy.** One job per poll. A job offered to a machine stays with that machine, and is offered to it again every 120 seconds until its callback arrives. That repeat is safe: the machine remembers the `job_key`. The job is never moved to another machine by itself, because another machine would run it a second time. Moving it is a decision for you, under a new `job_key`.

**Storage.** The workflow's static data, so the example needs nothing else. It is a demonstration store: two executions that write at the same moment overwrite each other, so two callbacks in the same second can lose one. That is fine for one machine and the smoke test below. With several machines, keep the two collections (`workers`, `jobs`) in a table of your own; the four Code nodes are the only place that reads or writes them.

## 5. Approve a machine and queue a job

```bash
export N8N=https://n8n.example.com/webhook
```

```bash
export ADMIN=REPLACE_WITH_ADMIN_TOKEN
```

Machines that have polled, with their status:

```bash
curl -s -X POST "$N8N/n3-admin" -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -d '{"action":"list"}'
```

Approve one, by the `token_hash` from that list:

```bash
curl -s -X POST "$N8N/n3-admin" -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -d '{"action":"approve","token_hash":"PASTE_THE_HASH"}'
```

Queue a job. `task` is a [JobRequest](../../docs/schemas/job-request.schema.json) without `job_key` and `callback_url`, which the workflow adds:

```bash
curl -s -X POST "$N8N/n3-admin" -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -d '{"action":"enqueue","job":{"job_key":"smoke-001","task":{"image":"ghcr.io/nailtongomes/shutup-and-run-smoke","timeout_secs":120}}}'
```

The same through a manifest:

```bash
curl -s -X POST "$N8N/n3-admin" -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -d '{"action":"enqueue","job":{"job_key":"smoke-002","via_manifest":true,"task":{"image":"ghcr.io/nailtongomes/shutup-and-run-smoke","timeout_secs":120}}}'
```

Other fields of `job`: `os` (`"windows"` or `"linux"`) offers it only to such machines, and `worker_hash` only to that one. `{"action":"revoke","token_hash":"..."}` stops serving a machine. `{"action":"reset"}` empties everything.

A `job_key` that is already queued is refused. A new attempt at the same work is a new key.

## 6. Push a job to a Linux runner

`push-to-linux.json`: **Manual**, **Job**, **POST /v1/jobs**, **Read the answer**, **Outcome**, and after an accepted job **Wait 20 s**, **GET /v1/jobs/{id}** and **Succeeded, failed or still running**.

```text
202, 200          accepted: wait for the callback
429, 503          full or draining: nothing was stored, try the next machine with the same job_key
anything else     refused: read statusCode and job.error
no answer         ambiguous: send the same job_key to the same machine again first
```

The HTTP Request node has **Never Error** and **Full Response** on, so the status code is data to route on and not an exception, and **Retry on Fail**, which is safe only because the job has a `job_key`. **Ignore SSL Issues** is on because the runner's certificate is self-signed; to pin it instead, see [docs/RECIPES.md](../../docs/RECIPES.md).

The three nodes after **Accepted** read the job's status on demand: `result` is `succeeded` (`reason` `exited`, `exit_code` 0), `failed`, or `running` when 20 seconds were not enough. That works because n8n can reach a Linux runner. A Windows worker cannot be asked; its callback is the only answer.

The job's `callback_url` is the same `n3-callback` webhook, so a pushed job that is not in the queue is recorded by its `id` in `results`.

## 7. Smoke test

About 20 minutes. Each step says what to expect. If a step does not match, stop there and keep the output.

### A. Linux: push

On the Linux machine:

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/nailtongomes/shutup-and-run-smoke bash
```

```bash
sudo sed -n 's/^SHUTUP_AND_RUN_TOKEN=//p' /etc/shutup-and-run/env
```

- [ ] The installer prints the URL and the certificate fingerprint. The second command prints the token.

In n8n, set `runner_url` and `runner_token` in the **Job** node of `push-to-linux`, and run it with **Test workflow**.

- [ ] **Read the answer** shows `outcome: accepted`, `statusCode: 202`, and a `job` with an `id` and `status: running`.
- [ ] Run it again without changing `job_key`: `statusCode: 200` and the same `id`. Nothing new started.
- [ ] Within a minute the `n3-callback` webhook has an execution whose body has that `id`, `reason: exited`, `exit_code: 0`.
- [ ] On the machine, `curl -sk -H "Authorization: Bearer $TOKEN" https://127.0.0.1:8787/v1/jobs/THE_ID?logs=true` shows `python ok` in `logs`.

Change `image` to `ghcr.io/nailtongomes/does-not-exist` and `job_key` to `smoke-bad-1`, and run it.

- [ ] `statusCode: 403`, `job.error: image is not allowlisted`. Nothing ran.

### B. Windows: pull, enrollment, callback

On the Windows 11 PC, WSL2 already enabled (`wsl --install --no-distribution` as administrator, then a reboot), in PowerShell as the user who stays logged on:

```powershell
$env:SHUTUP_AND_RUN_POLL_URL = "https://n8n.example.com/webhook/n3-worker-poll"
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
```

- [ ] Steps `[1/8]` to `[8/8]`, then `made a poll token for this machine` and `It gets no work until the poll URL approves it.`
- [ ] `shutup-and-run wsl logs` shows `poll returned 403 Forbidden: the poll URL has not approved this machine`.
- [ ] The `list` call of section 5 shows one worker, `status: pending`, with the PC's name and `os: windows`.

Approve it, then queue `smoke-001` and `smoke-002` with the two `enqueue` calls of section 5.

- [ ] Within five minutes (the worker backs off while it is refused) `shutup-and-run wsl logs` shows `poll started a job`, twice.
- [ ] `list` shows both jobs with `state: done`, `result.reason: exited`, `result.exit_code: 0`. The second one went through the manifest.
- [ ] `http://127.0.0.1:8787/` in a browser on the PC shows `RODANDO / RUNNING` and the last job.

### C. Windows: re-install

This is the check for the two defects fixed in the release after 0.2.3. Run the install again, on the same PC, with a different option:

```powershell
shutup-and-run wsl install --poll-url https://n8n.example.com/webhook/n3-worker-poll --max-slots 2 --worker-name SMOKE-RENAMED
```

- [ ] `kept this machine's poll token`. In n8n, `list` shows no second worker.
- [ ] The doctor table ends with `ok   service   answers on 127.0.0.1:8787: {"status":"ok",...,"slots_total":2,...}`. `slots_total` is the new value: the service that answers is the one that read the new config.
- [ ] The install ends with `done. The worker polls ... as SMOKE-RENAMED with 2 slot(s).` An install that could not prove that ends with an error instead.
- [ ] No address is banned:

```powershell
wsl -d shutup-and-run -u root -e /usr/local/bin/shutup-and-run bans
```

- [ ] It prints `no bans`.
- [ ] Within five minutes of the next poll, the worker's record in `list` says `worker: SMOKE-RENAMED` and `max_slots: 2`.

Run `shutup-and-run wsl install ...` a third time while a `shutup-and-run wsl serve` is open in another terminal.

- [ ] Either the install completes, having ended that supervisor (its terminal returns to the prompt), or it stops before step 3 with `a shutup-and-run wsl serve is still running`. It never ends with `done` while the old service is the one running. This path cannot run in CI; note which of the two you saw.

### D. What a failure looks like

Queue a job that cannot start:

```bash
curl -s -X POST "$N8N/n3-admin" -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -d '{"action":"enqueue","job":{"job_key":"smoke-bad-2","task":{"image":"ghcr.io/nailtongomes/does-not-exist","timeout_secs":60}}}'
```

- [ ] `list` shows it `done` with `result.reason: start_failed` and `result.exit_code: null`, and `container_ok: false`.

Queue a job the worker must drop: an unknown field.

```bash
curl -s -X POST "$N8N/n3-admin" -H "Authorization: Bearer $ADMIN" -H 'Content-Type: application/json' -d '{"action":"enqueue","job":{"job_key":"smoke-bad-3","task":{"image":"ghcr.io/nailtongomes/shutup-and-run-smoke","volumes":["/:/host"]}}}'
```

- [ ] `shutup-and-run wsl logs` shows `poll task rejected` every two minutes. Nothing runs. `list` shows the job `offered` forever, with `offers` climbing. The server is never told why: this is limit 1 in the contract. Remove it with `reset`, or leave it.

## 8. From this example to real use

- **Storage.** Replace static data with a table. The claim of a job by a machine must be one atomic update (`UPDATE ... WHERE state = 'queued'`), or two machines that poll in the same instant can both be offered it.
- **A deadline per job.** A callback can be lost. Give each offered job a deadline of `timeout_secs` plus a margin for the image pull, and when it passes mark the job `unknown`, not `failed`.
- **The result is the job's to report.** The callback says how the container ended. Pass the container a URL of your own in `env` and have it POST what it did. That is also the only way to get a result from a Windows PC whose callback was lost.
- **Before a retry, check the outside world.** A job that files a document, sends a message or pays something must ask the external system whether it already happened. See the court portal example in the [contract](../../docs/INTEGRATION-CONTRACT.md#6-idempotency-and-delivery).
- **Secrets.** A Windows worker is a client's PC, and its user can read every task's `env`. Send it only secrets scoped to that task, and keep work that needs real ones on your Linux machines, choosing by `os` in the queue.
- **Rate limit the poll URL** in the reverse proxy. Anyone who knows it can create pending records.

Measures worth keeping once this runs for real: the share of offered jobs that got a callback before their deadline, the time from queued to accepted, the share of `start_failed` and `timeout_kill` per machine, and how many machines polled in the last hour against how many are approved.

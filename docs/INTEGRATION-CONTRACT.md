# Integration contract

What an orchestrator (n8n, Kestra, your own API) needs in order to give work to a shutup-and-run machine and learn how it ended, without reading the Rust source. It describes what the runner does today, release 0.2.4. Where the behavior is awkward it is written down as it is, under [Limits](#9-limits-of-the-current-contract), and not as it might be wished.

Machine-readable, JSON Schema Draft 2020-12:

| Schema | Describes |
| --- | --- |
| [schemas/job-request.schema.json](schemas/job-request.schema.json) | A job: the body of a push, a task in a poll answer, a manifest. |
| [schemas/worker-poll-request.schema.json](schemas/worker-poll-request.schema.json) | What a worker sends when it polls. |
| [schemas/worker-poll-response.schema.json](schemas/worker-poll-response.schema.json) | The JSON a poll URL may answer with. |
| [schemas/manifest.schema.json](schemas/manifest.schema.json) | The body a `manifest_url` must return. |
| [schemas/job-result.schema.json](schemas/job-result.schema.json) | A job as the runner reports it. `$defs/callback` is the callback body. |

[schemas/examples](schemas/examples) holds one file per example in this document. `src/contract.rs` sends every one of them through the real HTTP router, the real poll parser and the real callback, and checks what the runner emits against these schemas, on every `cargo test`. If this document and the runner disagree, that is a bug in one of them: say so in an issue before working around it.

A request that is valid by the schema can still be refused by one machine, because its allowlist, its `max_timeout_secs`, its free slots and its drain flag are that machine's configuration.

There is no version field in any request or answer, and a request that carries one is rejected like any unknown field. The worker's `version` in the poll body is how a server tells releases apart. See [Compatibility](#10-compatibility).

## The two directions

```text
 push (Linux, reachable)                    pull (Windows, or any machine behind NAT)
 -----------------------                    ----------------------------------------
 orchestrator --POST /v1/jobs--> runner     worker --POST poll_url--> orchestrator
              <--202 + JobResult--                 <--task | manifest_url | nothing--
                                            worker --GET manifest_url--> (optional)
 runner --POST callback_url--> orchestrator worker --POST callback_url--> orchestrator
```

The job is the same object in both. The runner is the same binary in both: on Windows it runs inside a WSL2 distribution and only pulls.

The runner starts a container and reports how the container ended. It keeps no queue, does not retry, does not coordinate with other machines, and does not know what the container does. Registration and approval of machines, the queue, retries, and what a result means for the business are the orchestrator's.

## 1. JobRequest

```json
{
  "image": "ghcr.io/you/worker:1.4.2",
  "job_key": "case-0001-attempt-1",
  "env": {
    "SCRIPT_URL": "https://scripts.example.com/protocolar.py",
    "CUSTOMER_ID": "customer-placeholder",
    "RESULT_URL": "https://n8n.example.com/webhook/n3-result/case-0001-attempt-1"
  },
  "command": ["python", "main.py", "--once"],
  "timeout_secs": 900,
  "cpu": 1.5,
  "memory": "2g",
  "shm": "1g",
  "callback_url": "https://n8n.example.com/webhook/n3-callback/REPLACE_WITH_CALLBACK_SECRET"
}
```

The smallest one is `{"image": "ghcr.io/nailtongomes/shutup-and-run-smoke"}`.

| Field | Required | Type and limits | Default | Notes |
| --- | --- | --- | --- | --- |
| `image` | yes | string, 1 to 512 bytes | | Lowercase. No whitespace or control characters, no `..`, no `//`, not starting with `-`, not ending with `:`. On a push it must match the allowlist. |
| `job_key` | no | string, 1 to 200 of `A-Z a-z 0-9 . _ : -` | none | Idempotency on this one machine. See [section 6](#6-idempotency-and-delivery). |
| `env` | no | object of string to string, at most 128 entries | `{}` | A name matches `^[A-Za-z_][A-Za-z0-9_]*$`, at most 256 bytes. A value is at most 16384 bytes and has no NUL. |
| `command` | no | array of at most 32 strings, each 1 to 4096 bytes, no NUL | the image's own | Docker `Cmd`. It replaces the image's default command, not its entrypoint. `[]` is the same as leaving it out. |
| `timeout_secs` | no | integer, 1 to the machine's `max_timeout_secs` | the machine's `default_timeout_secs` (3600) | `max_timeout_secs` is 21600 by default and never above 604800. Counted from the container's start. Then SIGKILL, with no grace. The image pull has the same limit, separately. |
| `cpu` | no | number, above 0, at most 128 | no limit | Cores. `0.5`, `2`. |
| `memory` | no | positive integer of bytes, or a string such as `512m`, `1.5g`, `2GiB` | no limit | Units `b k kb kib m mb mib g gb gib`, any case, all powers of 1024. Swap is set to the same value. |
| `shm` | no | as `memory` | Docker's 64 MiB | Size of `/dev/shm`. A browser usually needs `1g` or more. |
| `callback_url` | no | string, at most 2048 bytes, `http` or `https` with a host, no whitespace | none | See [section 5](#5-callback). |

Rules that hold for every field:

- **Unknown fields are rejected.** `{"image": "x", "volumes": [...]}` is a `400`, not a job without volumes. There is no field for a mount, a published port, privileged mode, a capability, a network mode, a user, a restart policy or registry credentials, and a request cannot get them another way.
- **`null` is the same as leaving the field out**, for every optional field except `env`, which must be an object if present.
- **`env` is opaque.** The runner passes the strings to the container and does nothing else with them. It does not read `SCRIPT_URL`, does not download anything a variable points at, does not recognise credentials or customer identifiers, and does not log names or values. What a variable means is between the orchestrator and the image.
- Numbers are JSON numbers. `"timeout_secs": "60"` and `"cpu": "2"` are rejected. `timeout_secs` has no fraction (`900`, not `900.0`). A `memory` or `shm` with a fraction is a string (`"1.5g"`), not a number.
- Lengths are counted in bytes of UTF-8.
- The whole body is at most 64 KiB (65536 bytes), in a push, in a poll answer and in a manifest.

What the container gets, and a request cannot change: no host mount, no published port, not privileged, Docker's default capabilities, the default bridge network with outbound access, no restart. See [CONVENTIONS.md](CONVENTIONS.md).

Private images: the machine's own Docker login is used (`docker_config`). A request cannot carry registry credentials.

## 2. Push: `POST /v1/jobs`

For a machine the orchestrator can reach: a Linux server.

```http
POST /v1/jobs HTTP/1.1
Host: 203.0.113.10:8787
Authorization: Bearer <runner-token>
Content-Type: application/json

<JobRequest>
```

`<runner-token>` is the machine's `SHUTUP_AND_RUN_TOKEN`. The listener is HTTPS with a self-signed certificate unless the machine was configured otherwise: pin the fingerprint the installer printed, or skip verification on purpose. The TLS listener speaks HTTP/1.1 only and closes a connection that sends no request for 10 seconds. `Content-Type` is not checked.

### Answers

| Status | Body | Meaning | What the orchestrator does |
| --- | --- | --- | --- |
| `202` | [JobResult](#jobresult), `status` `running` | Accepted. A slot is taken and the row is stored. | Keep `id`. Wait for the callback. |
| `200` | JobResult of the earlier job | This `job_key` is already in this machine's history. Nothing new starts, whatever the rest of this request said. | Treat as accepted. Read `status` and `reason`: the job may already have ended, or failed. |
| `400` | `{"error":"invalid JSON"}` | The body is not JSON, is not an object, has an unknown field, lacks `image`, or a field has the wrong type or an unparsable size. | Fix the request. Do not retry it. |
| `400` | `{"error":"<what>"}` | A field is out of range. The messages are listed below. | Fix the request. |
| `401` | `{"error":"unauthorized"}`, header `WWW-Authenticate: Bearer` | Missing or wrong token. It counts toward a ban. | Stop. Fix the token before the third try. |
| `403` | empty | This address is banned: 3 wrong tokens (`auth_failures`) ban it for 48 hours (`ban_hours`). The right token does not help until it expires or is lifted on the machine with `shutup-and-run bans lift <ip>`. | Stop. |
| `403` | `{"error":"image is not allowlisted"}` | | Fix the image or the machine's allowlist. |
| `413` | plain text | The body is over 64 KiB. | Fix the request. |
| `429` | `{"error":"no free slots","slots_total":2,"slots_used":2,"slots_free":0}` | Full. Nothing was stored, the `job_key` included. | Try the next machine, or this one later. |
| `503` | `{"error":"draining","draining":true}` | Draining for maintenance. Nothing was stored. | Try the next machine. |
| `502` | `{"error":"docker is unavailable"}` | The Docker daemon did not answer. | Try the next machine. Tell a person. |
| `502` | `{"error":"job history is unavailable"}` | The SQLite file could not be read or written. | Same. |

A request is checked in this order, and the first failure answers: ban, token, body size, JSON shape, `image` name, allowlist, `timeout_secs`, `env`, `command`, `job_key`, `callback_url`, `cpu`, drain, `job_key` lookup, slots. Two consequences: a draining machine answers `503` even for a `job_key` it already holds, and a repeat of a known `job_key` with an invalid body is a `400`.

The `400` messages: `invalid image name`, `timeout_secs must be greater than zero`, `timeout_secs <n> is above the maximum <max>`, `env accepts at most 128 variables`, `invalid environment variable: <name>`, `invalid value for <name>`, `command accepts at most 32 arguments`, `invalid command argument`, `job_key must be 1-200 characters of letters, digits, '.', '_', ':' or '-'`, `callback_url is invalid`, `callback_url must be http or https`, `cpu must be a number between 0 and 128`, `cpu is invalid`. Match on the status, not on the text.

### Acceptance is not a started container

`202` is answered in milliseconds, before the image is pulled. It means: this machine took the job, holds a slot for it, and will report its end. It does not mean the image exists, that the pull will succeed, or that the container will start.

If the pull or the start fails, the job ends with `reason` `start_failed` and `exit_code` `null`, and the callback is sent. That is not an HTTP error on the push, which was answered long before. A caller that needs to know reads the callback or `GET /v1/jobs/{id}`.

### JobResult

```json
{"id":"5f479abd3087cf193ed4c1da8460784d","status":"running","image":"ghcr.io/you/worker:1.4.2","job_key":"case-0001-attempt-1","started_at":1700000000,"ended_at":null,"exit_code":null,"reason":null,"duration_secs":null}
```

| Field | Type | |
| --- | --- | --- |
| `id` | string, 32 lowercase hex | Chosen by the runner. Unique on that machine. The container is named `shutup-and-run-<id>`. |
| `status` | `running` or `exited` | `running` from acceptance, the pull included, until the end is recorded. |
| `image` | string | As requested. |
| `job_key` | string or `null` | As requested. |
| `started_at` | integer | Unix seconds, UTC. When the job was accepted, before the pull. |
| `ended_at` | integer or `null` | Unix seconds, UTC. |
| `exit_code` | integer or `null` | The container's exit code. `137` after a timeout kill. `null` while running, after `start_failed`, and when the container was gone before the runner saw it end. |
| `reason` | `exited`, `timeout_kill`, `start_failed`, or `null` | `null` while running. |
| `duration_secs` | integer or `null` | `ended_at - started_at`. It includes the pull. |

All nine keys are always present.

`GET /v1/jobs/{id}` returns the same object, for as long as the row is in the history (`history_days`, 30 by default), across restarts. `?logs=true` adds `logs`, the last `log_lines` (200) lines the container printed, or one line saying why the start failed. An unknown id is `404` `{"id":"<id>","status":"not_found"}`. There is no listing of jobs and no lookup by `job_key` other than pushing the same key again. There is no cancel.

### Examples

curl:

```bash
curl -sk -X POST "https://203.0.113.10:8787/v1/jobs" -H "Authorization: Bearer $SAR_TOKEN" -H 'Content-Type: application/json' -d @docs/schemas/examples/job-request.full.json -w '\n%{http_code}\n'
```

Python, standard library only, with the certificate pinned:

```python
import hashlib, json, ssl, urllib.error, urllib.request

RUNNER = "https://203.0.113.10:8787"
TOKEN = "REPLACE_WITH_RUNNER_TOKEN"
FINGERPRINT = "REPLACE_WITH_SHA256_FINGERPRINT"  # as the installer printed it, AA:BB:...

def push(job: dict) -> tuple[int, dict]:
    ctx = ssl.create_default_context()
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE  # the pin below is the check
    req = urllib.request.Request(
        f"{RUNNER}/v1/jobs",
        data=json.dumps(job).encode(),
        headers={"Authorization": f"Bearer {TOKEN}", "Content-Type": "application/json"},
        method="POST",
    )
    opener = urllib.request.build_opener(PinnedHTTPS(ctx))
    try:
        with opener.open(req, timeout=20) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as err:
        body = err.read()
        return err.code, (json.loads(body) if body.startswith(b"{") else {})

class PinnedHTTPS(urllib.request.HTTPSHandler):
    def __init__(self, ctx):
        super().__init__(context=ctx)
        self._ctx = ctx

    def https_open(self, req):
        import http.client

        class Conn(http.client.HTTPSConnection):
            def connect(conn):
                super().connect()
                seen = hashlib.sha256(conn.sock.getpeercert(binary_form=True)).hexdigest()
                if seen != FINGERPRINT.replace(":", "").lower():
                    raise ssl.SSLError("certificate fingerprint does not match")

        return self.do_open(Conn, req, context=self._ctx)

status, job = push({"image": "ghcr.io/nailtongomes/shutup-and-run-smoke", "job_key": "smoke-001", "timeout_secs": 120})
if status in (200, 202):
    print("accepted", job["id"])
elif status in (429, 503):
    print("try the next machine")
else:
    print("refused", status, job.get("error"))
```

n8n, an HTTP Request node. Paste it onto a canvas. The full workflow is [examples/n8n/push-to-linux.json](../examples/n8n/push-to-linux.json):

```json
{
  "nodes": [
    {
      "name": "POST /v1/jobs",
      "type": "n8n-nodes-base.httpRequest",
      "typeVersion": 4.2,
      "position": [440, 0],
      "retryOnFail": true,
      "maxTries": 3,
      "waitBetweenTries": 2000,
      "onError": "continueRegularOutput",
      "parameters": {
        "method": "POST",
        "url": "https://203.0.113.10:8787/v1/jobs",
        "sendHeaders": true,
        "headerParameters": {
          "parameters": [{ "name": "Authorization", "value": "Bearer REPLACE_WITH_RUNNER_TOKEN" }]
        },
        "sendBody": true,
        "specifyBody": "json",
        "jsonBody": "={{ JSON.stringify({ image: 'ghcr.io/nailtongomes/shutup-and-run-smoke', job_key: 'smoke-' + $execution.id, timeout_secs: 120, callback_url: 'https://n8n.example.com/webhook/n3-callback?secret=REPLACE_WITH_CALLBACK_SECRET' }) }}",
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

`neverError` and `fullResponse` put `statusCode` in the node's output, so `429` and `503` can be routed to the next machine. `retryOnFail` is safe only because the body has a `job_key`.

## 3. Poll

For a machine the orchestrator cannot reach. The machine's config names one URL, `poll_url`, and the machine asks it for work. The orchestrator exposes that URL, for example `POST https://n8n.example.com/webhook/n3-worker-poll`.

### The request

```http
POST /webhook/n3-worker-poll HTTP/1.1
Host: n8n.example.com
Authorization: Bearer <poll-token>
Accept: application/json
Content-Type: application/json

{"worker":"DESK-07","os":"windows","arch":"x86_64","version":"0.2.4","max_slots":1,"free_slots":1,"cpus":4,"memory_bytes":4110753792}
```

| Field | Type | |
| --- | --- | --- |
| `worker` | string, 1 to 64 printable ASCII, no space, `"` or `\` | The machine's `worker_name`. Default: its hostname; on Windows, the computer name. |
| `os` | string, same limits | `worker_os`. `linux` by default. The Windows install writes `windows`. |
| `arch` | string | `x86_64`, `aarch64`. |
| `version` | string | The runner's version, such as `0.2.4`. |
| `max_slots` | integer, 1 to 256 | Containers the machine runs at once. |
| `free_slots` | integer, 1 to `max_slots` | Free when the poll was sent. Never `0`: a full machine does not poll. |
| `cpus` | integer or `null` | What the process sees. On Windows that is the WSL2 VM. |
| `memory_bytes` | integer or `null` | Same. |

All eight keys are always present. There is no address, token, config or job history in the body, and there will not be: read the machine's address from the connection.

**Authentication.** `Authorization: Bearer <poll-token>` is sent when the machine has `SHUTUP_AND_RUN_POLL_TOKEN` (at least 16 characters, no whitespace). Without it there is no `Authorization` header at all. The token is the machine's identity. `worker` is a name the machine chose for itself and proves nothing.

**The server decides which machines get work.** A Windows worker installed without a token makes a random one and starts polling with it. The poll URL has never seen it. The rule for the server:

- An unknown token is a pending machine. Record it (a hash of the token, what the body says, the address of the connection) and answer `403` or `401`, or `204`.
- Never answer an unknown or unapproved token with a task, an image name, a manifest URL or a secret.
- Approve a machine by hand, or by a rule of your own. Retire one by no longer serving its token.
- Limit how many pending records one address can create. Anyone who knows the URL can make them.

There is no register call. Enrollment is this: the first poll with a new token.

### The answers

| Status | Body | The worker |
| --- | --- | --- |
| `204` | none | Nothing to do. Asks again after `poll_interval_secs`. |
| `200` | empty, or only whitespace | Nothing to do. |
| `200` | `null` | Nothing to do. |
| `200` | `{}` | Nothing to do. |
| `200` | `{"task": null}` | Nothing to do. |
| `200` | `{"manifest_url": null}` | Nothing to do. |
| `200` | `{"task": <JobRequest>}` | Validates the task and launches it. |
| `200` | `<JobRequest>`, bare | The same. |
| `200` | `{"manifest_url": "https://..."}` | Fetches the manifest, [section 4](#4-manifest), then validates and launches. |
| `401`, `403` | ignored | "Not approved". Starts nothing, even if the body holds a task. Logs it and backs off. |
| anything else | ignored | An error. Logs it and backs off. That includes `201`, every `3xx` (redirects are not followed), `429` and `5xx`. |

A direct job:

```json
{
  "task": {
    "image": "ghcr.io/nailtongomes/shutup-and-run-smoke",
    "job_key": "smoke-001",
    "timeout_secs": 120
  }
}
```

A manifest reference:

```json
{
  "manifest_url": "https://example.com/manifests/smoke-001.json"
}
```

`{"task": ...}` and `{"manifest_url": ...}` must be the only key in the object. `{"task": {...}, "lease": "abc"}` is rejected whole. One task per poll: there is no array form, and `free_slots: 3` still gets one.

A `200` body that is none of the forms above (not JSON, an array, a task with an unknown field, a bad `manifest_url`) is **dropped**: the worker logs `poll task rejected`, starts nothing, and asks again after the normal interval. The server is not told. The same happens when the task is well formed and the machine then refuses it, for the reasons a push gets `400`, `403`, `429` or `503`.

### Timing and limits

| | |
| --- | --- |
| Interval | `poll_interval_secs`, default 15, 1 to 3600. Counted from the end of one poll to the start of the next. |
| When no poll is sent | While every slot is busy, and while the machine is draining (paused from its status page, or by `POST /v1/drain`). The server sees silence, the same as from a machine that is off. |
| After a task was launched | The next poll is one interval later, if a slot is still free. A machine with 4 slots fills them in four polls. |
| Request timeout | 10 seconds for the whole exchange, body included. A slow answer is an error. |
| Errors | Connection failure, timeout, an answer over 64 KiB, or a status that is not `200` or `204`. The wait doubles after each one, up to 300 seconds (or the interval, when that is longer), and returns to the interval after the next answer that is not an error. There is no jitter. |
| Dropped task | Not an error: no backoff. |
| Answer size | At most 64 KiB. |
| Redirects | Never followed, for the poll, the manifest or the callback. |
| TLS | An `https` poll URL must present a certificate from a public authority. The worker trusts the bundled Mozilla roots, not the machine's certificate store, so a private or self-signed authority fails. `http` is accepted and sends the token in clear text. |
| URL | `http` or `https`, with a host, at most 2048 bytes, and no `user:password@`. |

## 4. Manifest

A manifest is a `JobRequest` the worker fetches. The poll answer carries only its URL, so the poll endpoint can stay small and the job can live somewhere else: a queue's own API, object storage with a signed URL.

```text
worker --POST poll_url--------------------> {"manifest_url": "https://example.com/manifests/smoke-001.json"}
worker --GET  https://example.com/manifests/smoke-001.json--> {"image": "...", "job_key": "smoke-001", ...}
```

| | |
| --- | --- |
| Method | `GET`, once per poll answer, with `Accept: application/json` and no body. |
| URL | `http` or `https`, with a host, at most 2048 bytes, no whitespace, no `user:password@`. A query string is fine, so a signed URL works. |
| Expected answer | Status `200` exactly, and a body that is a `JobRequest`, bare or as `{"task": <JobRequest>}`. See [manifest.schema.json](schemas/manifest.schema.json). |
| Authentication | The poll token is sent as `Authorization: Bearer` **only when the manifest has the same scheme, host and port as `poll_url`**. To any other origin the request carries no credentials. A private manifest is served from the poll URL's own origin, or behind a secret in its URL. |
| Size | At most 64 KiB. |
| Redirects | Not followed. A `3xx` is a failure. |
| Timeout | 10 seconds. |
| Nesting | **A manifest cannot name another manifest.** `{"manifest_url": ...}` in a manifest body is rejected. There is one hop, never a chain. |
| Empty | A manifest that is empty, `null`, `{}` or `{"task": null}` is a failure: the poll said there was work. |
| On failure | Nothing runs and nothing is stored, the `job_key` included. The worker logs `manifest rejected` and polls again after the normal interval, with no backoff. The server is not told. |

After the fetch a manifest is treated exactly like a task that came inline: the same validation, the same allowlist rule, the same `job_key`.

The runner does not interpret anything else at that URL. It will not read a Kestra flow, a YAML file, a list of steps or a script. One JSON object, one container.

## 5. Callback

When a job that has a `callback_url` ends, the runner sends one request:

```http
POST /webhook/n3-callback/REPLACE_WITH_CALLBACK_SECRET HTTP/1.1
Host: n8n.example.com
Content-Type: application/json

{"id":"5f479abd3087cf193ed4c1da8460784d","job_key":"case-0001-attempt-1","image":"ghcr.io/you/worker:1.4.2","status":"exited","exit_code":0,"reason":"exited","started_at":1700000000,"ended_at":1700000042,"duration_secs":42}
```

The body is the [JobResult](#jobresult) of a job that has ended: the same nine keys, `status` always `exited`, `ended_at`, `reason` and `duration_secs` never `null`, and never `logs`.

| `reason` | `exit_code` | What happened |
| --- | --- | --- |
| `exited` | the container's | The container ended by itself. `0` is the image's own idea of success. |
| `exited` | not `0`, often `137` or `255` | It was killed by something other than the timeout: the machine shut down, someone ran `docker kill`, the kernel ran out of memory. |
| `exited` | `null` | The container was gone before the runner saw it end, for example the machine went down while the image was still being pulled. Whether anything ran is unknown. |
| `timeout_kill` | `137` | SIGKILL at `timeout_secs`. |
| `start_failed` | `null` | The image could not be pulled, or the container could not be started. Nothing ran. |

**Delivery.**

- No header is added: no `Authorization`, no signature. Authenticate the callback with a secret in the URL, in the path or the query, and treat the URL as a credential.
- Any `2xx` is success. Anything else, a `3xx` included, is a failed try.
- Three tries: at once, 0.2 seconds after the first fails, 0.4 seconds after the second. Each waits at most 5 seconds for an answer. After the third, the runner gives up for good and only its own log says so.
- **A callback can be missing.** The three tries take under 20 seconds in all, so an orchestrator that is down for a minute misses it. The tries are not stored: if the machine loses power between the end of the job and the request, it is not sent later.
- **A callback can arrive twice.** If the receiver takes more than 5 seconds to answer, the runner tries again though the first request arrived.
- A job that ended while the runner process was not running is reported when the process next starts. On a Windows PC that was switched off, that can be the next morning.
- The request comes from the machine that ran the job. From a Windows worker that is the client's network.

**What it means.** The callback says how a container ended. `reason: exited, exit_code: 0` is what the image's process returned, on a machine that for a Windows worker is not yours. It is a notification, not proof that the business operation happened, and its absence is not proof that it did not.

**What the receiver does.**

- Answer `2xx` at once and do the work afterwards.
- Process each `id` once. Store the ids already handled and ignore a repeat.
- Do not wait for a callback forever. Give each job a deadline of your own, a little over `timeout_secs` plus the time a pull can take.
- For anything that matters, have the container report its own result to a URL passed in `env`, and reconcile against the external system.

## 6. Idempotency and delivery

**`job_key` deduplicates on one machine, and nowhere else.** A machine remembers the keys in its own history file. A request with a known key returns the stored job (`200` on a push, a log line on a poll) and starts nothing. Another machine has never heard of the key and will run the job.

**A failed job keeps its key.** A job that ended with `start_failed`, `timeout_kill` or a non-zero exit still holds its `job_key`, for as long as its row is in the history: `history_days`, 30 by default, or until the Windows distribution is uninstalled. Sending the key again returns the failed job. It does not run it again.

**A new attempt needs a new key.** Derive it from the work and the attempt: `case-0001-attempt-2`.

**The first request with a key wins.** A later request with the same key and a different image or `env` returns the first job. The differences are ignored without an error.

**A poll answer is an offer, not an acceptance.** The worker may drop it: the slot was taken in the meantime, the machine is paused, the task is invalid, the manifest did not load. The worker does not say so, and it does not say when it accepts either. The first thing the server hears about an offered job is its callback, at the end. So the server keeps offering the same `job_key` **to the same machine** until the callback arrives or its own deadline passes. On that machine the repeat is harmless.

**Acceptance can be ambiguous.** A push that times out may have been accepted. Send the same `job_key` to the same machine again: `200` or `202` both mean it is there. Only then consider another machine. A `429` or `503` stored nothing and is safe to take elsewhere under the same key. A draining machine answers `503` even for a key it holds, so "is it there?" cannot be asked of it until it is undrained.

**Callbacks are not exactly once.** Zero, one or two can arrive. See [section 5](#5-callback).

**Cross-machine coordination is the orchestrator's.** Which machine holds which key, when to give up on a machine, whether to move work to another: none of it is known to a runner. Moving a job to another machine is a decision that it may now run twice.

**The business operation has its own rules.** None of the above makes the work inside the container idempotent.

A job files a document in a court portal for a case. The container logs in, uploads the PDF, clicks "protocolar", and the portal registers the filing. Then the client's PC goes to sleep before the container can report. The orchestrator sees no callback, or `reason: exited` with `exit_code: 255`. Every signal the runner can give says "failed". The document is filed.

If the orchestrator now retries, with a new key or on another machine, the court receives the same petition twice. In some courts that is a second protocol number, a fee charged twice, or a note from the clerk. The runner cannot prevent it: to the runner these are two jobs.

So the job, not the runner, must make it safe:

1. Before filing, ask the portal whether this document is already there for this case (by case number and a hash or a title you control).
2. File only when it is not.
3. Report the protocol number to the orchestrator from inside the container, to a URL given in `env`, before exiting.
4. On a retry, do step 1 again. A retry that finds the filing reports the existing protocol number and files nothing.

The orchestrator in turn does not mark the case "not filed" because a callback said `exit_code: 255`. It marks it "unknown" and sends a job that checks.

## 7. Other routes

Every route needs `Authorization: Bearer <runner-token>`, `/health` and `/metrics` included. A path that does not exist is `401` without the token and `404` with it. A wrong method is `405`.

| Route | |
| --- | --- |
| `GET /v1/jobs/{id}` | A [JobResult](#jobresult). `?logs=true` adds `logs`. `404` `{"id":"...","status":"not_found"}`. |
| `GET /health` | `200` `{"status":"ok","draining":false,"slots_total":2,"slots_used":1,"slots_free":1}`. `status` is `draining` while draining. `503` `{"error":"docker is unavailable","draining":false}`. |
| `GET /metrics` | Prometheus text. |
| `POST /v1/drain` | Body `{"draining": true}` or `{"draining": false}`. Answers the same object. The flag survives a restart. |

On a Windows worker these exist only on `127.0.0.1` inside the WSL2 distribution. Nothing outside the PC can call them.

## 8. Validating against the schemas

The schemas reference each other by relative `$ref`, so load all five and register them by `$id`:

```python
import json, pathlib
from jsonschema import Draft202012Validator
from referencing import Registry, Resource

SCHEMAS = pathlib.Path("docs/schemas")
resources = [Resource.from_contents(json.loads(p.read_text())) for p in SCHEMAS.glob("*.schema.json")]
registry = Registry().with_resources((r.id(), r) for r in resources)

def validator(name: str) -> Draft202012Validator:
    schema = json.loads((SCHEMAS / name).read_text())
    return Draft202012Validator(schema, registry=registry)

validator("job-request.schema.json").validate({"image": "ghcr.io/you/worker", "timeout_secs": 600})
callback = {"$ref": "https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/docs/schemas/job-result.schema.json#/$defs/callback"}
Draft202012Validator(callback, registry=registry).validate(json.loads((SCHEMAS / "examples/callback.exited.json").read_text()))
```

What the schemas do not express, and the runner still checks: control characters in `image`, NUL in a string, a byte length where a character is more than one byte, and everything that is a machine's configuration. The `memory` and `shm` pattern is the documented form; the runner's parser also accepts surrounding spaces and an exponent, which nothing should rely on.

`worker-poll-request.schema.json` has `additionalProperties: false`, because that is exactly what this release sends and the tests hold it to that. An endpoint in production should read the keys it knows and ignore the others: the body grew from zero keys to three to eight before 0.2.0.

## 9. Limits of the current contract

Found while writing this document. None was changed here. Each is something an orchestrator has to design around today.

1. **No acknowledgement in pull mode.** The server never learns that an offered task was accepted, dropped or invalid. A task with a typo is dropped on every poll, forever, with only a line in the worker's own log. Test a new task shape with a push to a Linux machine, or against the schema, before queueing it for a Windows worker.
2. **A busy worker is silent.** A machine with no free slot does not poll. With `max_slots = 1`, a job that runs for an hour is an hour without a poll. The server cannot tell busy from switched off. Use the offered job's own deadline, not the time since the last poll, to decide a machine is gone.
3. **A lost callback cannot be recovered in pull mode.** Nothing outside the PC can call `GET /v1/jobs/{id}` on a Windows worker, and offering the same `job_key` again returns the stored job to nobody: the callback is not sent a second time. If the three tries fail, the result exists only on that PC. Have the container report its own result.
4. **Callbacks are not authenticated or signed**, and are neither persisted nor retried beyond three quick tries. See [section 5](#5-callback).
5. **Drain hides `job_key`.** `503` comes before the key lookup. See [section 6](#6-idempotency-and-delivery).
6. **A repeat with a different body is not an error.** The first request with a key wins and the caller is not told that the bodies differ.
7. **No output in the contract.** A job returns an exit code and a reason. Its last log lines are readable only on the machine, with the machine's token. A result that is data is sent by the container itself.
8. **No cancel.** A job ends by itself, at its timeout, or because someone on the machine killed the container.
9. **The poll needs a public certificate.** See the TLS row in [section 3](#3-poll). An n8n behind a private certificate authority cannot be polled over `https`.
10. **One task per poll, no jitter.** Many machines started at the same moment poll at the same moment. With hundreds of them, spread the load at the server.
11. **`reason` is coarse.** A job cut by a shutdown, by an out-of-memory kill or by `docker kill` is `exited` with a non-zero or `null` `exit_code`. There is no separate reason for them.
12. **A machine's limits are not discoverable.** The poll body does not carry `max_timeout_secs` or the allowlist, by decision. A task over a machine's timeout limit is dropped there (limit 1 applies).
13. **A re-install on Windows ends the jobs that are running.** `shutup-and-run wsl install` restarts the WSL2 distribution. Their callbacks are sent when the service comes back, as `exited` with a non-zero or `null` `exit_code`.

One constant in the source, `launcher_kill`, is a `reason` that no code path emits. It is not part of the contract.

## 10. Compatibility

What an orchestrator may rely on, and what `src/contract.rs` fails on if it changes:

- The nine keys of a JobResult, the eight keys of the poll body, and the five keys of `/health`.
- The status codes and error bodies in [section 2](#2-push-post-v1jobs).
- Every poll answer form in [section 3](#3-poll), the bare task and the empty forms included.
- Unknown fields rejected in a JobRequest, a poll answer and a manifest.
- No `schema_version`, no register call, no new required field.

A release that has to break one of these says so in [DECISIONS.md](DECISIONS.md) first. The poll body's `version` is there so that a server can treat releases differently for as long as it must.

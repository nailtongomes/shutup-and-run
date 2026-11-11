# Decisions

Short records of choices that are easy to second-guess later. Each one is context, the decision, and what it costs.

## Push, no queue

Orchestrators already know the list of machines. shutup-and-run accepts a job or refuses. It does not queue, retry across hosts, or elect a leader. A full machine is `429`. A draining machine is `503`. The caller moves on.

## Slots come from Docker labels

A slot is a running container labeled `shutup-and-run=1`. The count is a Docker list, not a counter in memory, so a restart does not forget work that is still running. The check and the create share one process mutex. Two processes on the same daemon can still go over `max_slots`.

A job that was accepted and is still pulling its image also holds a slot. That part is a set in memory, because no container exists yet. See the next record.

## 202 before the pull

`POST /v1/jobs` used to pull the image and start the container before it answered. A slow registry made Kestra and n8n wait, and a full machine pulled before it said `429`.

Now the request takes the slot, writes the row, and answers `202` at once. The pull and the start happen afterwards. The job runs either way. If the pull or the start fails, the job ends with reason `start_failed`, its stored log line says which, and the callback is sent. That is no longer a `502`. The pull has the job's own `timeout_secs` as its limit, and the container's timeout starts when the container does.

To answer before a container exists, the launcher picks the job id itself and names the container `shutup-and-run-<id>`.

The cost: `202` no longer proves the image exists, so a caller that needs to know must read the callback or the job. And a job accepted but not yet started lives only in this process. On shutdown the process waits up to 30 seconds for such jobs to become containers. One that is still pulling after that is recorded as ended when the process comes back.

## No cancel call

There is no `DELETE /v1/jobs/{id}`. The owner decided against it. A job ends by itself, by its timeout, or because someone with a shell on the machine kills the container named `shutup-and-run-<id>`. One fewer thing a stolen token can do, and one fewer state to reason about.

## SQLite for history, logs, and bans

One file. Job rows keep id, image, `job_key`, start, end, exit code, and reason, so `GET /v1/jobs/{id}` works after a restart. The last log lines are stored before the container is removed. Bans live in the same file. Finished rows older than `history_days` (default 30) are deleted on a timer. There is no other database.

## Idempotency is `job_key`, on one machine

The same key returns the existing row and does not start another container, including while it is running. A retry of the same URL is safe. The key is not shared with other machines. A job whose pull or start fails still holds the key; a new attempt needs a new key.

## Callback when the job ends

An optional `callback_url` receives status, exit code, duration, and reason. Three tries, no redirects, and it does not hold a slot. The container still writes its own result wherever it already does. The callback is only a nudge to Kestra or n8n.

## Drain

`POST /v1/drain` with `{"draining": true}` refuses new work with `503` and lets running jobs finish. The flag is in SQLite, so a restart stays drained.

## Every route requires the bearer token

Including `/health` and `/metrics`. A missing or wrong token is `401`. Nothing on the port is anonymous.

## Three failures, then 48 hours

Three `401`s from the same TCP peer ban that IP for 48 hours. Both numbers are config. The ban is `403` with an empty body, even if the next request has the right token, and it survives a restart. A later success clears the failure count. The peer is the socket address. `X-Forwarded-For` counts only when `trust_x_forwarded_for` is set. `shutup-and-run bans` and `shutup-and-run bans lift <ip>` fix a lock-out over SSH.

## Public IP and TLS in the binary

One owner, one host, a public address. The token must not cross the internet in clear text, so the binary serves HTTPS with rustls. The installer makes a self-signed certificate and prints its SHA-256 fingerprint. Clients pin it or skip verification on purpose. Plain HTTP is an explicit `http = true`, for localhost or a network that is already private.

## Connection deadlines on the TLS listener

The 2026-11-11 review held about a thousand idle TLS connections open from one host. The process ran out of file descriptors, stopped answering, and could not reach Docker to enforce timeouts. No token was sent, so nothing was banned.

The TLS listener now speaks HTTP/1.1 only. The first request must arrive within 10 seconds, and so must the headers of each later request on a kept-alive connection. HTTP/2 is off because an idle HTTP/2 connection has no such deadline in the server library. The systemd unit sets `LimitNOFILE=65536`.

The cost is no HTTP/2, which no caller here needs, and a client that waits more than 10 seconds between requests reconnects. A flood that keeps reconnecting is still possible. The answer to that is a firewall rule, not more code. The plain HTTP listener is unchanged, because it is for localhost.

## One owner

There is no user database and no per-tenant token. One token is the whole ACL.

## A machine someone else lends

The owner runs jobs on idle machines, including machines a client offers. That does not change the model: the token holder is still the single caller, and the token is still root on that machine. What changes is that the person who owns the hardware is not the person who holds the token.

So the risk is shared knowingly, and it is written down. The client installs the service, chooses the allowlist and `max_slots`, can watch every job, and can stop everything with two commands. [CONVENTIONS.md](CONVENTIONS.md) says what each side accepts, and [RECIPES.md](RECIPES.md) has the commands. No code enforces any of it.

## Timeouts are SIGKILL

A container past its deadline is killed with SIGKILL. There is no SIGTERM grace and no restart of a container that exits on its own.

## No volumes, published ports, or privileged

The request can set image, env, command, timeout, cpu, memory, and shm. It cannot mount the host, publish a port, or run privileged. The image's own entrypoint is what runs.

## MIT

The license is MIT. It is short, it is what operators expect to copy onto a server, and it does not need a NOTICE file. SQLite, which is bundled, is public domain.

## CI on every push and pull request

The repo is public at github.com/nailtongomes/shutup-and-run. GitHub Actions runs `cargo fmt --check`, `clippy -D warnings`, `cargo test`, and shellcheck, and a separate job runs `scripts/accept.sh` against the runner's Docker daemon. A musl job checks that the release binary stays static. The cost is minutes of CI, and the acceptance job needs sudo and a real dockerd.

## GHCR image

`ghcr.io/nailtongomes/shutup-and-run` is the static musl binary on `scratch`. `:edge` tracks `main`. A `v*` tag is pushed under that tag name. linux/amd64 only. The systemd installer stays the way to put it on a machine. The image exists for people who already run everything under Docker. Publishing uses `GITHUB_TOKEN` with `packages: write`. The first push may create a private package; the owner has to switch it to public once if anonymous pulls should work.

## GitHub release

A `v*` tag builds `shutup-and-run-x86_64-unknown-linux-musl`, writes `SHA256SUMS`, and attaches both to the GitHub release. `install.sh` downloads that exact asset from `releases/latest` and checks it against `SHA256SUMS` before installing. `SHUTUP_AND_RUN_BIN` skips the download. No release tag, no one-line install.

## Pull, optional

Some machines cannot accept an inbound connection. The orchestrator already has the list of hosts for the ones that can. Adding a queue inside this process would throw away the "accept or refuse" decision above.

When `poll_url` is unset, nothing calls out. When it is set, the process asks that URL for one task every `poll_interval_secs` (default 15), and only when a slot is free and the process is not draining. The returned object, if any, is the same JSON as `POST /v1/jobs` and goes through `launch`: allowlist, `job_key`, limits, `callback_url`. Empty responses and errors are logged. Errors back off, up to five minutes. The process does not exit. The poll body is capped at the same 64 KiB as a POST. Redirects are not followed.

`SHUTUP_AND_RUN_POLL_TOKEN` is environment only. A `poll_token` field in the TOML is an unknown field and startup fails. The URL is not logged, only its host. Credentials in the URL userinfo are rejected.

The cost is an outbound request to whatever URL the operator configured, and a task that can be dropped if the slot disappears between the check and the launch. A refused task does not store `job_key`. An accepted one does, even if its image then fails to pull. The server should offer the same key again until the callback arrives. There is still no queue.

## Security is a convention

The process does not know what the container does. It starts the image it was given. Sandboxes, signature checks, a secret store, and per-tenant tokens would be a second product, and the owner does not want that product.

Operator obligations live in [CONVENTIONS.md](CONVENTIONS.md): a dedicated machine, allowlist entries that are repositories they own, secrets only in the task env, firewall and bind address, token rotation, and the short list of host settings a request cannot override because those fields do not exist on the request. No enforcement code was added for rules a person can follow. The request schema already rejects unknown fields, and the container host config is still built only from image, env, command, cpu, memory, shm, and a deadline.

The cost is that a deployer who skips the document is running whatever image the token holder names, as the image's user, with Docker's default capabilities, on the default bridge. This repository will not stop them.

## Windows and macOS: the same product

Decided 2026-11-11. Nothing is built yet.

Windows and macOS get the same Rust binary and the same contract: accept a pushed job or poll for one, enforce slots and a timeout, keep history, call back. It is not a separate client.

On macOS a job is a container, as on Linux.

On Windows a job may instead be a script. The binary downloads it from a URL and runs it directly, with no WSL and no Docker around it. That is less isolation than a container, and it is a risk the machine's owner accepts in order to run the automation. This script mode is for Windows only.

[ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) has the details and the questions that are still open. The one that must be answered before any script runs is integrity: how the binary knows the script it downloaded is the one that was published.

The cost is a second kind of job with weaker isolation, inside a binary whose whole security story so far was "it only starts allowlisted containers".

## Smoke test against a running instance

`scripts/accept.sh` starts its own instance. `scripts/smoke.sh` tests one that is already installed, through the API only, with the URL and token from the environment. It is safe on a live machine: its jobs echo and sleep under a short `timeout_secs`, it undrains what it drained, and it sends wrong tokens from `127.0.0.2`, never from the caller. What it cannot do from where it runs, it skips and says why.

CI runs it twice: after `accept.sh` on the same instance, and against a service that `install.sh` installed from a local copy of the release assets. The release workflow runs it a third time after the real one-line install.

The cost is that the instance must allowlist an image for the test. The owner chose two: a small Python image built from `examples/smoke-image` and published as `ghcr.io/nailtongomes/shutup-and-run-smoke`, which is the default, and Docker's `hello-world`, which has no shell and so runs a reduced set. A worker image of your own works too if it has `sh` and `sleep`. There is no cancel call in the API, so cleanup is waiting for the test's jobs to end.

## The name

The owner named it `shutup-and-run`. It is the whole product: send a job, get a container, or get a refusal. Earlier nautical names (`berth`, `kedge`, `cleat`, and others) were already used by other Docker tools.

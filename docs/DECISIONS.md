# Decisions

Short records of choices that are easy to second-guess later. Each one is context, the decision, and what it costs.

## Push, no queue

Orchestrators already know the list of machines. shutup-and-run accepts a job or refuses. It does not queue, retry across hosts, or elect a leader. A full machine is `429`. A draining machine is `503`. The caller moves on.

## Slots come from Docker labels

A slot is a running container labeled `shutup-and-run=1`. The count is a Docker list, not a counter in memory, so a restart does not forget work that is still running. The check and the create share one process mutex. Two processes on the same daemon can still go over `max_slots`.

## SQLite for history, logs, and bans

One file. Job rows keep id, image, `job_key`, start, end, exit code, and reason, so `GET /v1/jobs/{id}` works after a restart. The last log lines are stored before the container is removed. Bans live in the same file. Finished rows older than `history_days` (default 30) are deleted on a timer. There is no other database.

## Idempotency is `job_key`, on one machine

The same key returns the existing row and does not start another container, including while it is running. A retry of the same URL is safe. The key is not shared with other machines. A start that fails still holds the key; a new attempt needs a new key.

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

## One owner

There is no user database and no per-tenant token. One token is the whole ACL.

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

A `v*` tag builds `shutup-and-run-x86_64-unknown-linux-musl`, writes `SHA256SUMS`, and attaches both to the GitHub release. `install.sh` downloads that exact asset from `releases/latest`. No release tag, no one-line install.

## Pull, optional

Some machines cannot accept an inbound connection. The orchestrator already has the list of hosts for the ones that can. Adding a queue inside this process would throw away the "accept or refuse" decision above.

When `poll_url` is unset, nothing calls out. When it is set, the process asks that URL for one task every `poll_interval_secs` (default 15), and only when a slot is free and the process is not draining. The returned object, if any, is the same JSON as `POST /v1/jobs` and goes through `launch`: allowlist, `job_key`, limits, `callback_url`. Empty responses and errors are logged. Errors back off, up to five minutes. The process does not exit. The poll body is capped at the same 64 KiB as a POST. Redirects are not followed.

`SHUTUP_AND_RUN_POLL_TOKEN` is environment only. A `poll_token` field in the TOML is an unknown field and startup fails. The URL is not logged, only its host. Credentials in the URL userinfo are rejected.

The cost is an outbound request to whatever URL the operator configured, and a task that can be dropped if the slot disappears between the check and the start. That refusal does not store `job_key` (a failed start still does, same as POST). The server should offer the same key again until the callback arrives. There is still no queue.

## Security is a convention

The process does not know what the container does. It starts the image it was given. Sandboxes, signature checks, a secret store, and per-tenant tokens would be a second product, and the owner does not want that product.

Operator obligations live in [CONVENTIONS.md](CONVENTIONS.md): a dedicated machine, allowlist entries that are repositories they own, secrets only in the task env, firewall and bind address, token rotation, and the short list of host settings a request cannot override because those fields do not exist on the request. No enforcement code was added for rules a person can follow. The request schema already rejects unknown fields, and the container host config is still built only from image, env, command, cpu, memory, shm, and a deadline.

The cost is that a deployer who skips the document is running whatever image the token holder names, as the image's user, with Docker's default capabilities, on the default bridge. This repository will not stop them.

## Windows and macOS later

Some work has to run on a person's computer, including a Windows machine that may not have Docker. Building that now would change what this binary is.

[ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) describes a future client: a small program, or on Windows possibly only a script, that polls or waits and then starts a container or, on Windows, runs a Python script fetched from a URL. The simplest behavior to mirror is an existing `.bat` bootstrap (config at the top, a per-task lock with an orphan TTL, Python via `py -3` then `python`, downloads with cache and backoff, `.tmp` then replace, offline fallback, exit codes 0, 1, and 2). That bootstrap does not check integrity. The roadmap requires a sha256 or a signature before a downloaded script runs. Open questions are listed there and are not decided here. There is no client code in this change.

The cost is a document that can drift, and a known gap (no integrity check) that stays open until someone builds the client.

## The name

The owner named it `shutup-and-run`. It is the whole product: send a job, get a container, or get a refusal. Earlier nautical names (`berth`, `kedge`, `cleat`, and others) were already used by other Docker tools.

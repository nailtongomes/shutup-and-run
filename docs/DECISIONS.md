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

## Apt in CI does not depend on one mirror

GitHub-hosted Ubuntu installs packages from `azure.archive.ubuntu.com`, sometimes through `mirror+file:/etc/apt/apt-mirrors.txt`. When that name does not resolve, `apt-get` retries for many minutes and the ci, image, and release jobs die before they compile. `Acquire::Retries` made the wait longer. It did not add a mirror that could succeed.

`scripts/ci-apt.sh` caps each `apt-get` at 90 seconds. If that fails, it leaves the Azure mirror and tries `https://archive.ubuntu.com/ubuntu`, then `https://mirrors.kernel.org/ubuntu`. The workflow step itself is capped at 10 minutes. The compiler stays `musl-gcc` from Ubuntu, the same one a local release build uses. The check job does not call apt: `shellcheck` is already on the GitHub-hosted image, and installing it was the step that died when every Ubuntu mirror was quiet. The cost is a couple of minutes when the Azure mirror is down, and one script the musl jobs share. A different musl toolchain would have avoided apt, and would also have made the release binary something we do not build locally.

## GHCR image

`ghcr.io/nailtongomes/shutup-and-run` is the static musl binary on `scratch`. `:edge` tracks `main`. A `v*` tag is pushed under that tag name. linux/amd64 only. The systemd installer stays the way to put it on a machine. The image exists for people who already run everything under Docker. Publishing uses `GITHUB_TOKEN` with `packages: write`. The first push may create a private package; the owner has to switch it to public once if anonymous pulls should work.

## GitHub release

A `v*` tag builds `shutup-and-run-x86_64-unknown-linux-musl`, writes `SHA256SUMS`, and attaches both to the GitHub release. `install.sh` downloads that exact asset from `releases/latest` and checks it against `SHA256SUMS` before installing. `SHUTUP_AND_RUN_BIN` skips the download. No release tag, no one-line install.

v0.1.0's `SHA256SUMS` names the file `dist/shutup-and-run-x86_64-unknown-linux-musl`, because `sha256sum` was run on that path. The installer matches the base name, so that release still installs, and a different hash still refuses. Later tags hash from inside `dist/` and record the base name only. The tag was not moved.

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

Decided 2026-11-11. Windows was built on 2026-11-12, below. macOS is still only the roadmap.

Windows and macOS get the same Rust binary and the same contract: accept a pushed job or poll for one, enforce slots and a timeout, keep history, call back. It is not a separate client.

On macOS a job is a container, as on Linux.

On Windows a job is a container too, inside WSL2. The 2026-11-11 idea of a script mode, where the binary downloads a script and runs it with no container around it, is deferred: it needs an integrity answer (sha256 or a signature) that nobody has given, and the owner chose WSL on 2026-11-12. [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) keeps the idea and its open questions.

## Windows is the Linux service inside WSL2

Decided 2026-11-12. The owner's Windows machines are lent by clients and only pull work; nothing is pushed to them.

Docker on Windows 11 is either Docker Desktop or Docker Engine inside a WSL2 distribution. Docker Desktop needs a licence above a company size, is installed per user, and would make the `.exe` a second Docker client. The choice is Docker Engine inside a distribution of our own, named `shutup-and-run`, imported from the Ubuntu 24.04 WSL rootfs that Canonical publishes with a `SHA256SUMS`. The Linux musl binary runs inside it unchanged. The `.exe` (`src/wsl.rs`) is a supervisor: `wsl install` provisions the distribution, `wsl doctor` checks it and runs one container, `wsl serve` keeps the service up and restarts it with backoff, and a scheduled task runs `wsl serve --background` at logon. The distribution mounts no Windows drive and has interop off, so a container, and the token holder, see only the VM.

The listener stays on, but on `127.0.0.1:8787` inside the distribution with `http = true`: the doctor and a shell use it, nothing outside can. The API token for it is generated at install and written beside the poll token in `/etc/shutup-and-run/env`, mode 600, inside the distribution. Neither token is passed on a command line.

The `.exe` downloads the musl binary of its own version from the GitHub release, so the two halves never disagree about the poll contract, and checks it against that release's `SHA256SUMS`. `install.ps1` does the same for the `.exe`. The release carries both binaries and one `SHA256SUMS`.

The costs. The service runs only while the installing user is logged on; a service account or a stored password would need administrator rights and was not worth it for the first version. `wsl stop` terminates the VM, so the containers end with it (after a 10 second grace since v0.2.0), unlike a Linux server where the service and the containers are independent. The rootfs is a 340 MiB download and `apt-get install docker.io` runs on the client's machine. The `.exe` is not code signed, so SmartScreen warns once. And CI can build the `.exe` but cannot run WSL2 on a GitHub runner, so the install path is tested by hand on a Windows 11 machine, not by a workflow.

## One instance, by a file lock

Decided 2026-11-12, for v0.2.0. A second `serve` used to run until its bind failed, after it had already reconciled and started polling, and with another port it kept running. On Windows a second supervisor restarted its failing service forever.

The service now takes an exclusive lock on `jobs.lock` beside the history file before it touches Docker, and the Windows supervisor takes one on `%ProgramData%\shutup-and-run\supervisor.lock`. A second start says so and exits 1. The OS drops the lock when the process dies, so nothing goes stale. It is `File::try_lock` from the standard library, which raised `rust-version` to 1.89.

The cost: the lock follows `history_path`. Two services with two configs and two history files on one Docker daemon are still possible, which is what `scripts/accept.sh` does on purpose.

## Windows for v0.2.0: what was left out

Decided 2026-11-12. This is an internal tool, and the Windows half cannot be tested in CI, so only what keeps a lent machine predictable went in.

- `wsl install` without `--poll-url` installs an idle worker: no token, no poll, the smoke image as the one allowlist entry, and the doctor as the only container.
- `wsl stop` stops the service and runs `docker stop -t 10` on our containers inside the distribution before it terminates the VM.
- The logon task's battery and 72 hour defaults are turned off with `Set-ScheduledTask` after `schtasks /Create`. It is a soft step, because it has not run in CI.
- No tray icon. It would be a window message loop in `unsafe` Win32 that no workflow can run. `wsl doctor` and `wsl logs` answer "is it working".
- No grace at sign-out or shutdown. Windows gives a process about five seconds there, and catching it needs the same hidden window. The job is killed, its row is closed at the next start with its exit code, and the callback is sent; there is no separate `reason` for it. Tasks sent to a lent machine must be safe to run twice.

The cost is that the person at the machine sees nothing on screen, and a job cut by a shutdown looks like a job that failed.

## Windows shell fallback when the .exe cannot run

Decided 2026-11-12. Some client PCs have no virtualization, so WSL2 cannot start and `shutup-and-run.exe` cannot run. [`scripts/windows/sar-launcher.bat`](../scripts/windows/sar-launcher.bat) is that case: `cmd` downloads a Python script and helpers from a URL the operator sets, caches them, takes a lock file, and runs the script.

It is not the script mode deferred above. The binary still does not download or run a script. This file does not speak the HTTP API, does not use the allowlist, and does not check a checksum. The integrity requirement in [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) still stands before any of that behavior moves into the binary. Use HTTPS and a host you control.

A lock younger than `LOCK_TTL` minutes counts as held (exit 2) and is not deleted. An older lock is stale and is replaced. The file does not record a PID, so a crashed run is abandoned only when the lock ages out. Every exit after the lock was taken deletes it. CI cannot run `cmd.exe`. [`scripts/windows/check-sar-launcher.py`](../scripts/windows/check-sar-launcher.py) checks that the file stayed ASCII, CRLF, and in that shape.

The cost is remote code execution with no integrity check, on a machine whose user can already read anything the script writes, and a second Windows path that is easy to confuse with the product.

## The poll body names the worker

Decided 2026-11-12. The poll used to send `{}`; the server told machines apart only by their bearer token.

With Windows workers spread across clients, one URL serves many machines and the server wants to know who is asking before it chooses a task. The body is now `{"worker": ..., "os": ..., "free_slots": n}`: `worker_name` (default: the hostname), `worker_os` (default: the build OS; the Windows worker says `windows`), and the slots the machine could fill. Nothing else: not the config, not the history, not a token. A server that ignored the old `{}` ignores this too.

The cost is a contract change for anyone who validated the body as empty, and two more config keys.

## The poll body says what the machine is, and the server enrolls it

Decided 2026-11-12, for v0.2.0. The owner's Linux machines are his own. The Windows machines are clients' PCs that he sets up over remote access and leaves running, so they are not trusted, and the queue must know a machine before it serves it.

The body grew from three keys to eight: `worker`, `os`, `arch`, `version`, `max_slots`, `free_slots`, `cpus`, `memory_bytes`. That is identity and capacity. Still no config, token or history. There is no IP in it: inside WSL2 the worker sees a private address, and an untrusted machine could claim any address, so the server reads the public one from the connection.

Enrollment is the token. `wsl install` with a URL and no `SHUTUP_AND_RUN_POLL_TOKEN` keeps the token already in the distribution or makes a random one. The poll URL sees an unknown token, records a pending machine, and refuses until the owner approves. A `401` or `403` is logged as "not approved" and backs off like any poll error. There is no separate register call and no new state on the worker.

The cost: anyone who knows the poll URL can create pending records, so the server must rate-limit them and must never answer an unapproved token with a task. A token made on the client's PC is readable by that client; it identifies the machine, it does not prove who is using it. And the poll contract changed again for a server that validated the body strictly.

## A poll answer may be a manifest URL

Decided 2026-11-12. A poll answer is a task inline, or `{"manifest_url": "..."}`. The machine fetches that URL once with a `GET` and the body must be a task in the same shape as `POST /v1/jobs`. A manifest cannot point at another manifest, so there is one hop, never a chain. Same 64 KiB cap, no redirects, same validation.

The poll token is sent with the manifest request only when the manifest is on the same scheme, host and port as `poll_url`. A manifest on object storage or a CDN is fetched without it, so the worker's token does not land in a third party's access log. The orchestrator that wants a private manifest serves it from its own origin.

The cost is a second request per task and one more URL the operator's orchestrator is trusted to choose.

## `poll_any_image`: no allowlist on a machine with one source of work

Decided 2026-11-12. The owner's Windows workers run whatever the orchestrator names; keeping an allowlist on each client machine in sync with the orchestrator was work with no reader.

`poll_any_image = true` makes a polled task's image whatever the poll answer says. It applies to a task from `poll_url` only; a pushed job is still matched against the allowlist, which may now be empty, and then matches nothing. The image name is still validated. The flag needs `poll_url`, so it cannot be set by accident on a push-only server. The allowlist matcher itself is unchanged: there is no wildcard entry, and `repo` still does not allow `repo-evil`.

The cost is that on such a machine the orchestrator, not a file on the lender's disk, decides what runs. That is said in [CONVENTIONS.md](CONVENTIONS.md), and the Windows installer turns the flag on.

## Smoke test against a running instance

`scripts/accept.sh` starts its own instance. `scripts/smoke.sh` tests one that is already installed, through the API only, with the URL and token from the environment. It is safe on a live machine: its jobs echo and sleep under a short `timeout_secs`, it undrains what it drained, and it sends wrong tokens from `127.0.0.2`, never from the caller. What it cannot do from where it runs, it skips and says why.

CI runs it twice: after `accept.sh` on the same instance, and against a service that `install.sh` installed from a local copy of the release assets. The release workflow runs it a third time after the real one-line install.

The cost is that the instance must allowlist an image for the test. The owner chose two: a small Python image built from `examples/smoke-image` and published as `ghcr.io/nailtongomes/shutup-and-run-smoke`, which is the default, and Docker's `hello-world`, which has no shell and so runs a reduced set. A worker image of your own works too if it has `sh` and `sleep`. There is no cancel call in the API, so cleanup is waiting for the test's jobs to end.

## The name

The owner named it `shutup-and-run`. It is the whole product: send a job, get a container, or get a refusal. Earlier nautical names (`berth`, `kedge`, `cleat`, and others) were already used by other Docker tools.

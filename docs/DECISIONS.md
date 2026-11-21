# Decisions

Short records of choices that are easy to second-guess later. Each one is context, the decision, and what it costs.

## Push, no queue

Orchestrators already know the list of machines. shutup-and-run accepts a job or refuses. It does not queue, retry across hosts, or elect a leader. A full machine is `429`. A draining machine is `503`. The caller moves on.

## Slots come from Docker labels

A slot is a running container labeled `shutup-and-run=1`. The count is a Docker list, not a counter in memory, so a restart does not forget work that is still running. The check and the create share one process mutex. Two processes on the same daemon can still go over `max_slots`.

A job that was accepted and is still pulling its image also holds a slot. That part is a set in memory, because no container exists yet. See the next record.

## 202 before the pull

`POST /v1/jobs` used to pull the image and start the container before it answered. A slow registry made Kestra and n8n wait, and a full machine pulled before it said `429`.

Now the request takes the slot, writes the row, and answers `202` at once. The pull and the start happen afterwards. The job runs either way. If the pull or the start fails, the job ends with reason `start_failed`, its stored log line says which, and the callback is sent. That is no longer a `502`. The pull has a limit of its own, 30 minutes (`launcher::PULL_LIMIT_SECS`; until 0.2.24 it was the job's `timeout_secs`), and the container's timeout starts when the container does.

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

`ghcr.io/nailtongomes/shutup-and-run` is the static musl binary on `scratch`. `:edge` tracks `main`. Each release is pushed as `v<version>` and `latest` by the release workflow, from the release's own binary (the tags that workflow creates with `GITHUB_TOKEN` start no other workflow, so `image.yml` on `v*` stopped at v0.2.9). linux/amd64 only. The systemd installer stays the way to put it on a machine. The image exists for people who already run everything under Docker. Publishing uses `GITHUB_TOKEN` with `packages: write`. The first push may create a private package; the owner has to switch it to public once if anonymous pulls should work.

## GitHub release

A `v*` tag builds `shutup-and-run-x86_64-unknown-linux-musl`, writes `SHA256SUMS`, and attaches both to the GitHub release. `install.sh` downloads that exact asset from `releases/latest` and checks it against `SHA256SUMS` before installing. `SHUTUP_AND_RUN_BIN` skips the download. No release tag, no one-line install.

v0.1.0's `SHA256SUMS` named the file `dist/shutup-and-run-x86_64-unknown-linux-musl`, because `sha256sum` was run on that path. The installer matches the base name, so a sums file in that shape still matches, and a different hash still refuses. Later tags hash from inside `dist/` and record the base name only. The v0.1.0 tag and its GitHub release were removed. Install from `releases/latest`.

## Pull, optional

Some machines cannot accept an inbound connection. The orchestrator already has the list of hosts for the ones that can. Adding a queue inside this process would throw away the "accept or refuse" decision above.

When `poll_url` is unset, nothing calls out. When it is set, the process asks that URL for one task every `poll_interval_secs` (default 30), and only when a slot is free and the process is not draining. The returned object, if any, is the same JSON as `POST /v1/jobs` and goes through `launch`: allowlist, `job_key`, limits, `callback_url`. Empty responses and errors are logged. Errors back off, up to five minutes. The process does not exit. The poll body is capped at the same 64 KiB as a POST. Redirects are not followed.

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
- No grace at sign-out or shutdown (v0.2.3 added three seconds of it, see "The Windows worker shows no window"). Windows gives a process about five seconds there, and catching it needs the same hidden window. The job is killed, its row is closed at the next start with its exit code, and the callback is sent; there is no separate `reason` for it. Tasks sent to a lent machine must be safe to run twice.

The cost is that the person at the machine sees nothing on screen, and a job cut by a shutdown looks like a job that failed.

## Windows shell fallback when the .exe cannot run

Decided 2026-11-12. Some client PCs have no virtualization, so WSL2 cannot start and `shutup-and-run.exe` cannot run. [`scripts/windows/sar-launcher.bat`](../scripts/windows/sar-launcher.bat) is that case: `cmd` downloads a Python script and helpers from a URL the operator sets, caches them, takes a lock file, and runs the script.

It is not the script mode deferred above. The binary still does not download or run a script. This file does not speak the HTTP API, does not use the allowlist, and does not check a checksum. The integrity requirement in [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md) still stands before any of that behavior moves into the binary. Use HTTPS and a host you control.

A lock younger than `LOCK_TTL` minutes counts as held (exit 2) and is not deleted. An older lock is stale and is replaced. The file does not record a PID, so a crashed run is abandoned only when the lock ages out. Every exit after the lock was taken deletes it. CI cannot run `cmd.exe`. [`scripts/windows/check-sar-launcher.py`](../scripts/windows/check-sar-launcher.py) checks that the file stayed ASCII, CRLF, and in that shape.

The cost is remote code execution with no integrity check, on a machine whose user can already read anything the script writes, and a second Windows path that is easy to confuse with the product.

## Docker in the Windows distribution is started by systemd

Decided 2026-11-12, for v0.2.1. The first install on a real Windows 11 machine stopped at step 4. `/etc/wsl.conf` had `[boot] command = "service docker start"`, and the install ran the same command after apt. Ubuntu 24.04's `docker.io` ships a systemd unit and no init script, and the distribution had no systemd, so nothing could start `dockerd`. The error blamed apt and printed the first line of its output, which was the part that had worked.

`/etc/wsl.conf` now says `[boot] systemd = true`, the install runs `systemctl enable --now docker` and waits for `docker info`, and the serve script uses `systemctl start docker`. That is how the Linux server starts Docker too. The service itself is still run by the Windows supervisor, not by a unit: the supervisor is what restarts it and holds the log. A failed step now prints the command and its last 20 lines.

The cost: systemd in WSL needs WSL 0.67.6 or newer (`wsl --update`), and it is a few more processes in the VM. The install says so when `/run/systemd/system` is missing.

## The logon task is registered for one user, without an administrator

Decided 2026-11-12, for v0.2.2. On a real Windows 11 account that is not an administrator, step 7 failed with "Access is denied". `schtasks /Create /SC ONLOGON` makes a trigger for any user's logon, and that needs an administrator even with `/RL LIMITED`.

The task is now registered with `Register-ScheduledTask` and a logon trigger tied to the installing user, which a standard user may create for themselves. The same call carries the settings that used to be a second, soft step (no battery rule, no 72 hour limit, ignore a second instance). `schtasks /Run` still starts it. `shutup-and-run wsl task register|remove` runs just that step, so CI and a Windows Sandbox can test it without WSL.

`install.ps1` ended a task that might not exist with `schtasks ... 2>$null`. Under `$ErrorActionPreference = "Stop"`, Windows PowerShell 5.1 turns that stderr into an error, so an update on a machine without the task stopped before it replaced the `.exe`. It goes through `cmd /c` now.

The cost: the install depends on the ScheduledTasks PowerShell module, which Windows 11 has. CI's runner is an administrator, so the non-administrator path is only tested by hand, in `tests/windows-sandbox`, which cannot run WSL2.

## The Windows worker shows no window

Decided 2026-11-12, for v0.2.3. On a real PC the logon task opened a Windows Terminal window titled `wsl.exe`, and closing it killed the worker. The `.exe` is a console program. The task started it with a console, `--background` detached from that console, and the `wsl.exe` child, a console program left with none, was given a new one.

Three things changed. The task's action is `conhost.exe --headless <exe> wsl serve --background`, which the owner tested: the process has a console and nobody sees it. Every child process is created with `CREATE_NO_WINDOW`; their output was already piped. And `--background` no longer detaches, because the console is now what delivers the sign-out and shutdown events: on those the supervisor stops the service and gives the containers SIGTERM and three seconds, inside the five Windows allows.

The `.exe` stays one console program. A GUI-subsystem build, with `AttachConsole` for the command line, was weighed and left out: PowerShell and cmd do not wait for a GUI program and do not set `$LASTEXITCODE` for it, which would break `install.ps1`, the doctor's exit code and CI. A second, GUI binary would be one more release asset and checksum for the same result. There is still no tray icon.

The cost: `conhost --headless` is not a documented switch, though Windows Terminal and others rely on it. The session-end handler cannot be tested in CI, and five seconds is what Windows gives, so a job that needs longer to finish cleanly still does not get it.

## The Windows .exe links the C runtime statically

Decided 2026-11-12, for v0.2.3. In Windows Sandbox the `.exe` exited with `0xC0000135` and printed nothing: it imported `vcruntime140.dll`, which comes with the Visual C++ Redistributable and is not on a clean Windows 11. The owner's PC and the CI runner both have it, which hid this.

`.cargo/config.toml` sets `+crt-static` for `x86_64-pc-windows-msvc`. CI reads the built file and fails if it mentions `vcruntime140` or `msvcp140`, and checks that its PE subsystem is console. `install.ps1` now runs `--help` after installing and reports an exit code in hexadecimal with a hint.

The cost is a slightly larger `.exe`.

## A local status page, off by default

Decided 2026-11-12, for v0.2.3. The person at a lent PC, and support asking them for a screenshot, need to see whether the worker is running and whether the PC may be turned off. The owner asked for a page at `http://127.0.0.1:8787/`.

`status_page = true` adds `GET /`: state, slots, the last job (image, status, reason, exit code, age), the machine's name and the version, with one button that pauses or resumes taking work, which is the drain flag. No token, env or log is on it. The Windows install turns it on; a Linux server leaves it off.

It is the one thing on the port without the bearer token, so it is fenced: the socket peer must be loopback, the `Host` header must name loopback, and the button's POST must carry the page's own `Origin`. Otherwise it is a 404 or a 403, and it never counts as an authentication failure. `/favicon.ico` is answered too, because a browser asks for it by itself and three unanswered asks would ban the machine's own address.

The cost: "every route requires the token" now has an exception, and "no web UI" has one page. Anyone who can open a browser on that machine can pause the worker, which the person lending the PC could already do. Do not turn it on behind a reverse proxy on the same host that forwards `Host: localhost`.

## A status file beside the page

Decided 2026-11-12. Scripts on the machine need the same answer as the status page without opening HTTP. The page and `status.json` are built from one snapshot: slots, whether new work is refused, the running job ids, and the last job.

On Linux the service writes it next to the history file (`/var/lib/shutup-and-run/status.json` by default) at least every 30 seconds and within a second of a change, as a temp file renamed over the previous one. A graceful stop writes `stopped` last. The directory stays root-only, like the history file.

On Windows the distribution does not mount the Windows disk, so the service cannot write `C:\ProgramData`. The supervisor copies each status line from the service into `C:\ProgramData\shutup-and-run\status.json`. When the service or the distribution is gone, the supervisor writes `failed` and the reason itself. `wsl stop` and the end of the Windows session write `stopped` after that, so the file does not stay on `failed`.

The cost: a reader that looks only at `state` will trust a dead process until it also checks `updated_at`. Older than 60 seconds means dead. The Windows file lags the service by whatever it takes the log line to arrive, and the supervisor log gains one JSON line per rewrite.

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

## A re-install on Windows replaces the running service, and proves it

Decided 2026-11-12. Two defects from re-installing on real PCs.

`wsl install` on a machine that already had a worker could end with the old service still running on the old config. Ending the scheduled task did not always end the supervisor (one started by hand in a terminal is not the task's), the supervisor restarted the old service in the middle of apt, and that service was still there after the new files were written. The new supervisor then found the lock taken and left. The doctor's `service` line was soft, so the install said `done`.

And the install makes a new API token every time. The doctor read it from the new env file and asked the old service for `/health` with it, up to 20 times. Three refusals ban an address for 48 hours, the address was `127.0.0.1`, and the ban is in SQLite, so it outlived the restart that would have fixed everything else.

The install now takes the supervisor's own lock after ending the task and holds it until step 7. Halfway through the wait it runs `taskkill` on every other `shutup-and-run.exe`; if the lock still cannot be taken it stops with an error before step 3. Before step 6 writes the config and env, the service inside the distribution gets SIGTERM and the install waits until no such process is left. After step 7 the service must answer the new token within 60 seconds, or the install fails: the token is new with each install, so a service that accepts it was started after the files were written. The doctor asks until something answers and stops at the first `401` or `403`, with a line that says which it was and what to do. And step 6 runs `shutup-and-run bans lift 127.0.0.1` inside the distribution, for a machine an earlier install had already locked out.

The ban rule itself did not change. Loopback is not exempt, the count and the 48 hours are the same for every address, and the lift is the documented `bans lift`, run as root inside a distribution whose listener is on localhost. A test starts the real router with the default limits and checks that a doctor with the wrong token is one failure, that the right token still works after it, and that three wrong ones still ban.

The cost: a doctor run against a service with an older token is still one counted failure each time, so three such runs in a row, with no success between them, ban the address. The doctor now says to restart the service, and the next install lifts the ban. A re-install ends the jobs that were running, as before: step 3 restarts the distribution. `taskkill` by image name also ends a `shutup-and-run.exe` doing something else, a doctor in another terminal for instance. None of the `wsl.exe`, `schtasks.exe` and `taskkill.exe` steps can run in CI.

## The contract is a document, schemas, and tests that hold them to the code

Decided 2026-11-12. An orchestrator had to be written by reading `src/api.rs` and `src/poll.rs`.

[INTEGRATION-CONTRACT.md](INTEGRATION-CONTRACT.md) is the specification, and `docs/schemas` holds JSON Schema Draft 2020-12 for the job, the poll request, the poll answer, the manifest and the job result. They describe what the runner did already. Nothing in the API changed: no `schema_version`, no register call, no new field. `src/contract.rs` sends every example file under `docs/schemas/examples` through the real router, poll parser and callback, checks what the runner emits against the schemas, and for a list of invalid bodies checks that the schema and the runner both refuse them.

Limits found on the way are listed in the document and were left as they are: a polled task is never acknowledged, a busy worker is silent, a lost callback cannot be asked for again in pull mode, and a draining machine answers `503` before it looks up a `job_key`. Each would be a contract change with its own record here.

The cost: `jsonschema` is a development dependency, about forty crates that the release binary does not link. A change to an answer or to a validation rule now has to change a schema, an example and the document in the same patch, which is the point.

## One config file, an agent CLI, and a page that edits

Until 0.2.4 the rule was "no web UI beyond one status page", and a Windows worker's config changed only by installing again. That stopped fitting how the product is used. The machines are clients' PCs, set up and repaired remotely, and now mostly by an AI agent. The poll URL changes often, because it carries a per-user filter. An agent that has to re-run an installer, or edit a TOML file inside a WSL distribution, or ask a person, is slow and makes mistakes. And a person who knows the project had no way to look at or change a setting without a terminal.

So the premise changed, narrowly:

- **One file.** `config.json` holds every setting of a machine, both tokens included, mode 600. With it there is no environment on top and no second file. `docs/schemas/worker-config.schema.json` documents it, and a test keeps the schema and the code's key list equal. A `config.toml` machine keeps working as before and is not converted; `poll_token` stays out of TOML, which is a file people commit, while `config.json` is machine state the tool writes.
- **Never down for bad config.** Every write validates the whole config first with the service's own rules, so nothing invalid is saved. A file damaged another way (a power cut, a hand edit) falls back to the copy made by the last save, then to built-in defaults that idle, and the page, `status.json` and the doctor say so.
- **Apply without a service manager.** `config apply`, and Save on the page, make the service stop as it does on SIGTERM and then exec itself. The pid stays, so systemd and the Windows supervisor see nothing, and containers are left alone as in any restart.
- **A CLI for agents.** `config show|get|set|unset|validate|apply|reset|keys`, `status`, `doctor`, `pause`, `resume`: no questions, `--json`, exit codes 0/1/2/3. On Windows the `.exe` forwards them into the distribution, so there is one implementation. The doctor reads files and the job history and opens one TCP connection to the poll URL's host; it sends no token anywhere, so it cannot ban the machine or enroll it. Each failed line has a `fix`. `doctor --report` prints a report with secrets masked and a prefilled new-issue URL, so no GitHub login is needed on the machine.
- **The page edits.** Still loopback only, still one page family under `/` and `/local/`. Everyday keys are open: `poll_url`, `poll_interval_secs`, `max_slots`, `worker_name`, and pause. Dangerous keys ask for HTTP Basic credentials (`admin`/`admin` until `config set admin_password`): `allowlist` and `poll_any_image` decide which images run; `default_timeout_secs` and `max_timeout_secs` bound how long a job holds the PC; `token` opens the API; `poll_token` is the machine's identity at the queue; and the admin login itself. `listen`, TLS, paths and the rest are CLI only: a wrong value there takes the page itself away. A form reads only its own group's keys, so the open form cannot carry a dangerous one. The login is a BIOS password, not security: the PC's user can read the file. The page is in English or Portuguese, never both; a link switches, a ten-year cookie remembers, and the browser's language is the default.
- **A pause does not outlive a restart where the page is on.** A pause forgotten before a reboot or a re-install left a machine lending nothing. With `status_page` the flag is cleared at start. Without it (a Linux server) the drain flag persists, as the Drain record says.
- **Factory reset.** A client's PC loses power, disk and network at any time. `config reset` rewrites the config (tokens and identity kept when readable). `wsl reset` deletes and rebuilds the distribution; the config returns from a copy the supervisor keeps in `%ProgramData%`, so the machine keeps its poll token and needs no new approval. `--factory` forgets that too. A re-install keeps the config and changes only the options given.

What it costs. The tokens are now in a file a re-install does not rotate, on a machine whose user could always read them. The page is more code on a port, guarded by loopback, Host and Origin checks, and its "login" must never be mistaken for a control. A changed port needs the desktop shortcut rewritten, which `config apply` does on Windows. Linux gets the CLI and the page only with `config.json`; `install.sh` still writes `config.toml`, and there is no converter. Lease, heartbeat and idempotency are not part of this.

## A shared PC: limits on every job, and no job into a full machine

A Windows worker is a client's everyday computer, and a job used to run there with no limit unless the task named one. One browser job could take all the memory of the WSL2 VM, and the VM half the PC's RAM. The rule now is that the person at the PC comes first and throughput second.

There are three layers, and each is something that already existed. The WSL2 VM is bounded by `.wslconfig` (40% of the RAM, half the processors). Each container is bounded by Docker's own `NanoCpus`, `Memory` and `MemorySwap`, which the runtime already set when a task asked: the `shared-pc` profile gives a default and a ceiling (`max_job_cpus` 1, `max_job_memory` 2g). And a job is not admitted while `MemAvailable` or the free disk is under a floor (`min_free_memory` 2g, `min_free_disk` 5g), checked inside the same lock that counts slots.

- **One figure is both the default and the maximum.** A job that names nothing gets the machine's limit, and a job may ask for less. Two keys per resource (default and maximum) would be a second thing to get wrong for no case we have.
- **A request above the limit is refused, not cut down.** A browser given half the memory it asked for fails later and in a stranger way. A `400` with the limit in it tells the sender now.
- **The profile is the default on Windows only, by `worker_os`.** A worker installed before this gets the limits when its binary is replaced, with no config change. A Linux machine is the owner's server and keeps running as before unless `resource_profile = shared-pc` is set. The cost: a Windows worker whose tasks asked for more than 1 CPU or 2g starts refusing them after the upgrade, until the tasks or the keys change.
- **Memory defaults shrink on a small machine**: half of `MemTotal` under 4 GiB. An 8 GiB PC gives WSL2 about 3.2 GiB, and a 2 GiB job plus a 2 GiB floor would never start there.
- **A pull worker that is short does not poll.** The poll body stays at its eight keys and the protocol has no "busy" answer. Not asking is what a full worker already did. It cannot claim a task and then drop it, except in the one round trip between the check before the poll and the check at the start.
- **`.wslconfig` is the user's file.** The install writes it only when it does not exist and the PC has no other distribution. Otherwise it reports, and `wsl limits --apply` is the explicit step: it replaces two keys, keeps every other line and leaves a `.wslconfig.bak`. Nothing runs `wsl --shutdown`, which would stop the user's other distributions; the doctor says `not in effect yet` until WSL restarts by itself.

What it costs, and what it is not. The free-memory figure is the VM's, not Windows': the PC can be short while the VM looks fine. It is read once, at admission, not while the job runs. Disk and network use are not limited. There is no idle detection, no monitor, no resizing, and no job is killed for pressure; each of those is a daemon and a policy this project does not want yet. The four size keys are strings (`"2g"`, `"1"`) so the CLI and the file read the same, which makes `max_job_cpus` a quoted number in TOML.

## Certificate passthrough: the owner's key, to every job or to none

Some of the work these machines do is signing with the client's own A1 certificate, which lives on the client's PC and must not live on the orchestrator. The request was explicit: when the owner enables it, every container gets the certificate and the password, with no per-image list.

- **It is a machine setting, never a request field.** A request still cannot set a mount. `StartSpec.binds` is filled only by `certificate::stage`, from two fixed files to two fixed paths, read-only. The orchestrator cannot turn it on, and never receives the certificate or the password.
- **No per-image list.** A list would look like a control and be one only as strong as the orchestrator that picks the image, which on a Windows worker is already trusted with `poll_any_image`. Saying "every job" is the honest description, and the owner confirms exactly that sentence.
- **An imported copy, not a mount of the original.** The WSL2 distribution mounts no Windows drive, by an earlier decision that keeps containers away from `C:`. So the supervisor or the browser reads the file once and the service keeps a copy. The requirement to mount the original read-only is met in spirit: the original is never written, and the container's view is read-only. It costs a second copy of the key on disk.
- **The password is a 600 file beside the config, not in the Windows credential store.** The service that needs it at every job start runs as root inside WSL, where DPAPI does not reach, and the supervisor would have to hand it over at every start and keep it in memory. The PC's user is root in that distribution either way, so the store would add a moving part and no protection against the one person who could read it. It is kept out of `config.json` so that `config show`, the page, the backup copy on the Windows side and every report cannot carry it.
- **Per-job copies under `/run`.** A container that does not run as root has to read the files, so they are mode 444 inside a directory only root can enter, one directory per job, deleted at the job's end. Binding the stored 600 files directly would fail for exactly the browser images these jobs use.
- **openssl, not a crate.** Parsing PKCS#12 and X.509 in the binary would add a cryptography dependency tree to check a file a few times a year. The `openssl` program is already on every machine the service runs on (the Windows install now names it). The demo signs with it inside `python:3.12-slim` and verifies with it outside the container.
- **The demo's claims are small on purpose.** `demo sign` produces `adbe.pkcs7.detached` and verifies the digest and the signer's key. It does not build PAdES, does not judge the chain, and does not mention ICP-Brasil as satisfied.

## First-run commands reuse the worker's own path

`demo`, `job validate` and `config check` exist so that a person or an agent can prove a machine works before an orchestrator is wired to it. Each could have been a shortcut around the worker. None is.

- `job validate` calls `launcher::check_request`, the function `launch_from` now also calls. There is one copy of the rules. A test sends the same bodies to both and requires the same verdict.
- `demo` builds a `Launcher` on the machine's Docker with the machine's settings and a history file in a temporary folder. The allowlist, the limits, the slots and the memory check apply. An image that is not allowed fails the demo with the command that allows it. The worker's own history and config are not opened for writing.
- `config check` is the doctor's config and Docker checks plus four that need no running service. It writes nothing; storage is asked of the kernel with `access`.
- Diagnostic codes are a field added to each check. `name`, `status`, `detail`, `fix` and the exit codes are as they were.

The cost: a demo container carries the worker's label, so a running service counts it against `max_slots` for the seconds it lives, and a service with pruning on could remove it first.

## The poll interval has a floor of 30 seconds

`poll_interval_secs` could be set to 1. The poll URL is one server for every worker, most of them clients' PCs configured by someone else, and a few machines at 2 seconds are a flood the server did not ask for. From 0.2.8 the minimum is 30 seconds and so is the default (it was 15). It is one constant, `config::MIN_POLL_INTERVAL_SECS`.

- **Loading never fails on it.** A config written by an older release with a lower value still starts the worker, which polls every 30 seconds and says so: a `POLL_INTERVAL_RAISED` warning in `doctor` and `config check`, a `warning` in `config validate`, and `effective` in `config show`. Refusing to start would turn an upgrade into an outage on a PC nobody is sitting at.
- **Writing a lower value is refused**, exit 3, by `config set`, the page and the install option, and the file is left as it was.
- **The service does not rewrite the file.** `config.json` is written only by `config::save_json`, on a person's or an agent's command, and a service that edits its own config at start is a second writer with no one to report to. The migration happens at the first write instead: a save that finds the old low value still in the file raises it to 30 in that same write, and so does a re-install. Without that, an old install could not change any other key until someone fixed this one.
- **It costs throughput.** A worker takes one task per poll, so a machine with four slots fills them in two minutes, not eight seconds. That is the trade the owner asked for. Rate limiting stays the orchestrator's job; this only removes the easy way to cause the problem.

The tests paid for it too: the smoke test's poll check and the mock cycle now wait for real 30 second polls, about two more minutes of CI.

Two fixes from the first run of 0.2.7 on a real PC ride along. An install whose poll URL does not answer at that minute ends with 0 and a note, because everything the install does on the PC worked and the URL is another machine. And the desktop folder is read from PowerShell as base64 of UTF-8: printed as text it came through the console code page, and `Área de Trabalho` became a folder that does not exist.

## Hints, a jobs command, a poll right after a job, and a masked query (0.2.9)

The worker goes to the PCs of the clients of a petition-filing service. Whoever installs and supports it is an AI that may have a shell or may only tell a person what to type, and the container posts its result to the service itself, so the worker's side of "did it run?" is all there is to ask the machine.

- **A `hint` beside every code.** Every failure of the CLI and every check that is not ok carries `hint`: one short next step with the command, in `language` (`en` or `pt`, a config key). `fix` stays: it is the exact command for this machine, in English. The catalogue is one table in `src/hints.rs`; a test finds every code-shaped string in the sources and fails when one has no hint, and another keeps `docs/FOR-AGENTS.md` equal to it. A failure with no code of its own gets one from its exit code (`USAGE`, `INVALID_VALUE`, `FAILED`).
- **`jobs`** reads the SQLite history that was already there. It prints when, how long, the exit code and one sentence of why, and never the env, command, callback, key or logs. The history gained a ceiling of 500 finished rows beside `history_days`.
- **A poll right after a job ends**, when a slot is free, so a queue drains without 30 seconds of idle between tasks. It is one ordinary poll: "nothing" is followed by the interval, a failed poll (429 included) keeps its backoff, and a job that never started does not trigger it, so a task whose image cannot be pulled is not asked for in a loop. One second at least separates two polls.
- **The query of the poll URL is masked.** It pairs the machine with a person (a tax id, an e-mail, a key). 0.2.5 said it was not a secret and showed it; it is still not what approves a machine, but it identifies someone, so `config show`, `config get`, the page, the doctor, the report and the poll error print `***` for its values. `--reveal` shows it. The page's form shows the masked URL and keeps the real one when it is sent back unchanged.
- **`config check` asks the poll URL once**, with no token and `free_slots: 0`, so that a 404 is not called reachable. A request that could be answered with a task would lose that task, which is why it carries no token.

The cost: `config get poll_url` no longer prints the usable URL without `--reveal`, which a script written for 0.2.5 may depend on. A poll server now sees one request with no token per `config check`, and more polls from a busy machine than two a minute.

## One next step per code, and a release without a hand-pushed tag (0.2.10)

- **`fix` and `hint` were two sentences for one thing.** 0.2.9 added `hint` beside `fix`: `fix` was written where the check was, in English, with the machine's own detail; `hint` came from the catalogue, in the configured language, without it. Now there is one: the catalogue's sentence, and a check hands it the one detail it needs (the address to unban, the image to allow, the slot count that fits), which fills the `{}`. The JSON still has `fix`, with the same text as `hint`, so a reader of 0.2.8 keeps working; the text output has only `hint:`. The cost: a few `fix` texts were longer than the hint that replaced them.
- **One language setting.** The page followed a cookie and the browser, the CLI followed `language`. The page now opens in `language` unless the person pressed its button, and `wsl install --language pt` sets the key.
- **`last_poll_at` and `last_poll_status`** in `status.json`. "Is it asking for work?" could only be guessed from `poll_error` being null. Only a request that was sent counts.
- **The doctor asks the poll URL itself when no service is running** to say how its polls go, with the probe of `config check` (no token, no free slot).
- **Unit tests run as a Linux server on every platform.** With no `worker_os`, a test's machine is `linux` (`cfg!(test)` in `config.rs`). Twice a fixture that named no OS took the Windows defaults on the Windows runner and failed there only.
- **The release workflow runs when `Cargo.toml` on `main` has a version with no release**, creates the tag and publishes. Tags were pushed by hand, from the owner's machine, after every merge. The cost: merging a version bump publishes, so the bump is the decision to release.

## Idle wait, a shutdown question, interrupted jobs and callbacks that are kept (0.2.11)

The worker is on shared Windows PCs with 8 to 12 GiB, running one container of about 1.3 GiB that files documents with a browser. Four things the owner asked for, each on something that was already there.

- **`min_idle_secs`, off by default.** The record for 0.2.6 said "no idle detection". It was the daemon that was refused, and there is still none: the Windows supervisor, which was already running in the person's session, reads `GetLastInputInfo` and writes one line every ten seconds to the stdin of the service it started, which was `NUL` until now. The service keeps the last line and compares two numbers at admission, beside the memory and disk reads. No file crosses the WSL boundary and the service never calls Windows. A feed older than 45 seconds is forgotten and the wait is dropped: a supervisor that hangs must not park the worker. A running job is never stopped when the person comes back. The cost: "idle" is keyboard and mouse only, so a film counts as idle; a push to such a machine can now be answered `503` `user active`; and on Linux the key is accepted and does nothing.
- **Windows asks before it ends a session with a job running.** The supervisor creates a top-level window that is never shown and answers `WM_QUERYENDSESSION` with no, and a reason, while the last status line of the service says a job holds a slot. Windows then shows its own screen, and the person cancels or goes on. The answer reads one atomic flag: nothing in a window message calls WSL or Docker. The jobs are stopped only at `WM_ENDSESSION` with the session really ending, so a cancelled shutdown costs nothing. Loading user32 means Windows no longer sends this process the console's logoff and shutdown events, which is how the supervisor heard them before; the console handler stays for a closed console. The cost: "the worker never shows a window" now reads "never shows one on screen"; a forced shutdown, a power cut and "shut down anyway" still end the job; and the status line is about a second old, so a job accepted in that instant is not protected.
- **`interrupted` is a reason.** After a restart the service already closed the rows whose containers had stopped and sent their callbacks, as `exited` with a `null` or non-zero code, which an orchestrator could not tell from a job that failed by itself. Now a job whose container is gone, or that was found stopped with a non-zero code when the service started, ends as `interrupted`. A non-zero exit the service saw happen is `exited`, as before, and so is an exit `0` found after a restart. Nothing is started again: the orchestrator decides. The cost: a job that failed by itself in the seconds the service was restarting (a config save, an upgrade) is `interrupted` too, with its own exit code; and a caller that matched `reason == "exited"` with a `null` code to mean "gone" has to match the new value.
- **A callback nobody answered is kept.** The three quick tries were all there was, and a PC that comes back from a shutdown often has no network yet, which is exactly when an interruption is reported. The job's row gained one column, `callback_state` (`pending`, `delivered`, `failed`); the maintenance loop that already runs sends each pending one again once a minute, for one hour after the job ended, then gives up. No second table, no queue, and the body is the stored job, so a retry cannot say something else. The cost: a receiver can get the same body more than twice, up to an hour late, where before it got it within twenty seconds or never.
- **"Computer resources" on the local page** shows what was read and nothing else: the PC's processors and memory from the supervisor's feed, the VM's from `/proc/meminfo`, the limits from the config, slots, idle time, whether a job could start, interrupted jobs and callbacks. The CPU and memory a running container uses are not there: Docker's stats call takes about a second per container, and the page would need a sampler to show it.

## Smoke test against a running instance

`scripts/accept.sh` starts its own instance. `scripts/smoke.sh` tests one that is already installed, through the API only, with the URL and token from the environment. It is safe on a live machine: its jobs echo and sleep under a short `timeout_secs`, it undrains what it drained, and it sends wrong tokens from `127.0.0.2`, never from the caller. What it cannot do from where it runs, it skips and says why.

CI runs it twice: after `accept.sh` on the same instance, and against a service that `install.sh` installed from a local copy of the release assets. The release workflow runs it a third time after the real one-line install.

The cost is that the instance must allowlist an image for the test. The owner chose two: a small Python image built from `examples/smoke-image` and published as `ghcr.io/nailtongomes/shutup-and-run-smoke`, which is the default, and Docker's `hello-world`, which has no shell and so runs a reduced set. A worker image of your own works too if it has `sh` and `sleep`. There is no cancel call in the API, so cleanup is waiting for the test's jobs to end.

## The name

The owner named it `shutup-and-run`. It is the whole product: send a job, get a container, or get a refusal. Earlier nautical names (`berth`, `kedge`, `cleat`, and others) were already used by other Docker tools.

## The doctor says whose fault it is (0.2.12)

**Context.** On a client's PC the worker ran `hello-world` and served its page while the configured poll URL answered 404. The doctor said `"ok": false` and "something is wrong", and an agent reading that went looking for a fault on a machine that had none.

**Decision.** `doctor --json` and `config check --json` gain three fields computed from the checks they already had: `worker_ready`, `poll_endpoint_ok` (true, false or null) and `poll_endpoint` (`ok`, `error`, `disabled`, `not_checked`). `ok`, the exit codes and the check codes do not change: a poll URL that answers 404 to the service is still exit 1. The text output, when `poll` is the only failed check, ends by saying the worker is ready and the fault is at the poll URL. `tests/poll-mock/cycle.sh ... one` is the flow an integrator needs to see once: one task, the callback with its `job_key`, and a second offer that starts nothing.

**Cost.** `worker_ready` is "no check but `poll` failed", so a paused worker, or one short of memory, is `true` with a warning beside it. The word for the poll URL is in two fields, a boolean for a quick `if` and a string for why it is null. The one-task run takes a minute and a half of CI, because the 30 second poll interval is not shortened for tests.

## The owner's computer comes before the job: assessment, consent, incidents (0.2.13)

**Context.** The shared-PC profile already capped every container and made a new job wait for free memory and disk. Four things were missing. The free-memory read was the WSL2 VM's, so a Windows PC out of memory looked fine from inside. A reading that failed counted as fine. Nothing looked at the processor. And a 2-processor, 5 GB PC started jobs exactly like a 16-core one, without its owner ever being told what that costs. A container killed for memory was "exit 137", the same as a `docker kill`, and the next task could do it again at once.

**Decision.**

- `src/safety.rs` assesses the machine at every admission under `shared-pc`, from reads that were already there (`/proc/meminfo`, one `statvfs`) plus `/proc/loadavg` and, on Windows, three more fields on the supervisor's line: the PC's CPU use (`GetSystemTimes`, the difference between two lines) and the free space of the drive that holds the WSL2 disk (`GetDiskFreeSpaceExW`), beside the memory it already sent. The answer is healthy, warning or blocked. Critical lines latch until a recovery line, so admission does not flap.
- Blocked is mandatory. The thresholds are constants, not config keys: on a client's PC the operator who can edit the config is not the person the limit protects.
- A missing reading is never healthy. One the worker takes itself (memory, disk, load) blocks; the Windows side going quiet is a warning, because the VM's own readings still stand and a supervisor that hangs must not park the machine with no way out for its owner.
- A warning needs the owner's consent, once. It is stored in the history's `meta` table with the notice version, the codes it covered, the time and the channel, and it is asked again when a warning appears that was not covered or the notice changes. It is given on the local page or at a terminal, by a person; `--json`, a missing terminal, the API, the poll URL and the config cannot give it, and answer `AUTHORIZATION_REQUIRED` instead of waiting. The page is not behind the admin login: that password is the operator's.
- `State.OOMKilled` is read when a job ends. The job's `reason` stays `exited`; the row, `jobs`, the JobResult and the callback gain `oom_killed` only when true. Three in thirty minutes stop new jobs for fifteen: a pause with an end, since the worker cannot know which task is at fault and must not lock a machine for good.
- CPU throttling is read once per running job every 30 seconds (one-shot `docker stats`) and recorded when the job ends if it was held back in half its periods. It changes nothing else: throttling is the limit working.
- Incidents (OOM, throttling, blocked, recovered, authorization given or withdrawn) go to an `incidents` table capped at 200 rows, and the last five to `status.json` and the page. Nothing is sent anywhere.
- A job's own memory limit, when it is above `min_free_memory`, is what has to be free for it to start.
- Profile `none` is not assessed. A Linux server its owner dedicated to jobs behaves as before.

**Cost.** A Windows PC with under 8 GiB or two processors stops taking work after the update until its owner authorizes on the page; that is the point, and it is also a fleet that goes quiet until people click. A PC that crosses a warning line for a minute, with an owner who never authorized, reports `authorization_required` for that minute. A determined program on the machine can imitate the page or a terminal; the record says which channel was used, and nothing more is claimed. The Windows readings are compiled and unit tested for their arithmetic and have not run on a Windows PC. The thresholds are chosen, not measured. The one-task-in-flight CPU sample is a small loop where there was none. Throttling of a job shorter than 30 seconds is never seen. Host readings are ten seconds old at worst, and a job is not stopped when the PC gets busy after it started.

## Managed containers: the worker starts its own work, in a mode of its own (0.2.14)

**Context.** A recurring RPA (a Selenium worker every fifteen minutes on a client's PC) needed Kestra or n8n and a poll URL only to say "run this again". The rule until now was "no scheduler, and no automatic restart of a container that exits", and the owner asked for both, small.

**Decision.**

- A managed container is a job request (image, command, env, cpu, memory, shm, timeout) plus a policy: `once`, `interval`, `always`. It has no field a job does not have. `src/managed.rs` decides only what to start and when; a run goes through `Launcher::launch_managed`, the same admission as a push (`check_request`, the allowlist, the slots, a pause, memory and disk, idle time, the owner's safety, the certificate), and is a row in `jobs` with a new `managed` column. Managed runs share `max_slots` with nothing else to share them with, because of the next point.
- The machine works one way at a time. `mode` is a row in the history's `meta` table, like the pause, not a config key: it must change without a restart, from the page or the CLI, and only while no job row is unfinished. That condition and the write are one SQL statement, and so are "this row is a managed run" and "the mode is managed" in `insert_running`, so the CLI in another process cannot slip between a check and a write. A push in managed mode is `409`, not `503`: it does not clear by itself.
- The mode is explicit, not derived from "some definition is enabled". A derived mode would change when the last container is disabled, which is exactly when one may still be running.
- Definitions and their state live in the history file, table `managed`. What a person wants (enabled, start, stop) is written there by the page and the CLI; a pass every 10 seconds, woken at once by the page and by a job's end, reads it and acts. So the CLI needs no HTTP, and a restart loses nothing. Which run belongs to a definition is the job row itself, so a worker killed between starting a container and noting it still finds it, and never starts a second one.
- JSON, not YAML: requests and `config.json` are JSON and there is no YAML parser in the binary. The page has a form.
- Secrets are `secret_env`: values in their own column of the history file (mode 600, where callback URLs already are), never in the stored definition, and no code path reads that column except the one that builds the container's env. `null` in a file means "the value this machine has".
- `interval` counts from start to start, never overlaps, runs once after any number of missed intervals, and does not retry a failure early. `always` waits 30 s, doubling to 15 min while runs last under a minute, whatever their exit code. It never gives up: a PC that was offline for a day should resume without a person. A start the machine refused is a wait of 30 s, not a failure.
- Stop is a person's word and holds until a person starts it again. Disable starts nothing new and kills nothing, like every other protection here: a running job is not interrupted by a setting.
- Start, stop, enable and disable are open on the local page, like the pause. What a container is, removing it and the mode are behind the admin login. There is no API route for any of it: the bearer token is the orchestrator's, and this is the feature for machines without one.

**Cost.** The runner now has a scheduler, a small one: no cron expressions, no dependencies, no catch-up. A machine cannot serve a queue and run managed containers at once. The CLI's start and stop take up to 10 seconds to happen and say so. An `always` container that can never work is retried every 15 minutes forever and shows its count of failures. Secrets are plain text in a root-only file, as the tokens in `config.json` are; on a Windows PC its user can read them, as the rule about Windows machines already says. `demo` runs on a history of its own and so still runs in managed mode. On Windows `wsl reset` deletes the history, and with it the definitions, secrets and mode; only `config.json` has a copy on the host. Each pass inspects each running managed container once (one local Docker call per container per 10 s).

## An update replaces both halves, or neither (Windows, 0.2.15)

**Context.** On a client's PC `install.ps1` over a running worker failed at `Move-Item`: `schtasks /End` showed the task as ready while `wsl serve --background` still ran under its console host and held the `.exe`. The script left the download and `SHA256SUMS` in the folder. Once the `.exe` was swapped by hand, the Linux binary in the distribution was still the old release until `wsl install` ran again.

**Decision.**

- `install.ps1` over an installed `.exe` stops the worker with `wsl stop` (the jobs get SIGTERM, as a stop always gave them), then ends a process still running that one path, and only that path. It renames the old `.exe` instead of overwriting it, which Windows allows even while it runs, so a process that refuses to die does not stop the update.
- The Linux half is `wsl update`, not `wsl install`: only the binary changes, so no apt, no `wsl.conf`, no config written. It keeps the old binary until the new service has written a status with the new version, and puts it back otherwise. The script keeps the old `.exe` until `wsl update` succeeds, and puts it back otherwise. Download, `SHA256SUMS` and `.old` are removed on every path.
- The doctor fails on a `.exe` and a distribution of different versions (`VERSION_MISMATCH`). They are one release; the host feed and the commands the `.exe` forwards assume so.
- With no WSL or no distribution, `wsl update` has nothing to update and exits 0, so a PC with only the `.exe` updates too.

**Cost.** An update ends running jobs, as a re-install did. The rollback of the `.exe` and of the binary are two steps in two places; a power cut between them leaves a `VERSION_MISMATCH`, which the doctor names and `wsl update` fixes. CI installs the latest release in WSL2 on the Windows runner, updates it with this build and rolls back from an `.exe` that does not start. Until a release differs from the build, the two versions there are the same, so the swap is real but the numbers do not change; a PC with jobs, pause and authorization is still a hand test.

## A job's output is kept on the machine, unmasked, for 7 days (0.2.16)

**Context.** The container is removed when a job ends, and with it what the job printed. A managed job on a Windows PC ended with exit 0 and left nothing to read: `docker ps -a` was empty, and `wsl logs` has only the worker's own lines. The history row kept the last `log_lines` lines for `GET /v1/jobs/{id}?logs=true`, which neither the owner nor an agent at the PC uses. The rule until now was that `jobs` and the local page never show a job's output; the owner asked for it.

**Decision.**

- Just before the container is removed, the last 256 KiB of stdout and stderr go to `logs/<job id>.log` next to the history: mode 600, the folder 700, written through a temporary file. The cut drops the partial first line. One place, `Launcher::complete`, in the branch that closed the row, so every mode and every ending goes through it. A job that never started a container has no file. A failure to read or write is a warning; the job and the worker go on.
- Files older than 7 days go at each purge pass. Size and age are constants, not settings.
- `jobs logs ID|--last [--tail N] [--json]` reads them with no service running, like `jobs`. `jobs --json` gives each job's `log` (path, size, command). The doctor warns about a last job that did not end well, with the exact command. The local page lists them and serves one as text, to open or download; the id must be a worker id, so no other file can be named.
- Nothing is masked: masking that is not complete is worse than none, and the owner asked for the output as it was.

**Cost.** Up to 256 KiB per job for 7 days on disk: with `max_slots` 1 and a job a minute, about 2.5 GiB at most. A job that prints a secret leaves it in a file readable by root, and on a client's PC by whoever uses it, as the rule about Windows machines already assumes; the local page shows it to anyone at the PC. Output is read once at the end, all of it streamed from Docker and only the end held: a job that printed gigabytes costs that read. There is no live view. stdout and stderr are in the order Docker read them, which is not always the order the job wrote them.

## The local page: one stylesheet, the state first (0.2.17)

**Context.** The page worked and was hard to read: a person sent to it, by the owner or by an AI, could not tell in a few seconds whether the worker was fine, busy, waiting for them or broken, nor what to do next.

**Decision.**

- The top of every status page is the state in one word, coloured by what it asks of the person (ready, working, waiting for you, blocked, error), one sentence under it, and the machine and its last poll. Anything that needs a person comes next, one box each, with its one action: authorize, resume, give a managed container its secret, fix the config, look at the poll URL. Then the facts, the last jobs, the resources.
- One stylesheet in the binary (`PAGE_CSS`), no font, no framework, no request anywhere: the page works offline. Light and dark follow the system. Surfaces are told apart by a step in tone; lines only inside tables and fields. Text and status colours keep 4.5:1 or more on their grounds in both themes.
- Times are UTC in the markup and become local time through eight lines of inline script; without it the page still reads.
- The settings are grouped by subject, each field with a short label and one line of help. Fields behind the admin login are marked as protected. The list of jobs is the history's last 100 jobs with their logs, plus any log whose job the history no longer has.
- Nothing else changed: the routes, the forms and their field names, the rules of the page, the config and the contract.

**Cost.** About 43 KB more in the binary. The page is longer on a phone, where tables become stacked rows. The screenshots in `docs/screenshots/local-page/` show it at 0.2.16 data and will age; the README's own come from `scripts/screenshots.sh`, which makes them again.

## The first host-feed line waits a second for the CPU reading (0.2.18)

**Context.** The PC's CPU use is the difference between two readings of the totals `GetSystemTimes` keeps. The supervisor took both readings back to back for its first line, so that line never had a CPU figure. The safety assessment reads a missing figure as "Windows did not report CPU use" (`SAFETY_HOST_UNKNOWN`), a warning, and every install, update and restart asked the owner for an authorization for about ten seconds, until the next line. The doctor that closes an install showed it on every Windows CI run.

**Decision.** The first line is sent one second after the first reading (`CPU_FIRST_SAMPLE`), with the CPU use of that second. Every later line is the same as before, ten seconds apart. The rule that a missing reading warns, never passes, is unchanged.

**Cost.** The service gets its first line from Windows one second later.


## When the PC lends itself, what a job used, and the owner's consent from the contract (0.2.19)

**Context.** The safety assessment (0.2.13) answers whether the PC has room and whether a risk was agreed to. Three more things decide whether a customer's PC should lend itself at all, and the contracts under which PCs are lent already name them: the hours agreed with the owner, a laptop away from the mains, and a person in a meeting or a presentation, where a spike hurts most. The operator also wants to know what a job used, to size the next ones. And on a small PC every install ended waiting for someone to authorize on the page, though the owner had agreed in the contract before anything was installed.

**Decision.**

- **Three waits, in `src/lending.rs`.** Outside `lend_hours`, on the battery below 60%, and while the person presents or has a program in full screen, no new job starts, the worker does not poll, and a push gets `503` `not lending now` with the code (`OUTSIDE_LEND_HOURS`, `BATTERY_LOW`, `USER_PRESENTING`). A running job goes on, as with every other admission check. On the battery at 60% or more a job starts; the page and the doctor say so (`ON_BATTERY`). The battery and the full-screen state come on the host feed, from `GetSystemPowerStatus` and `SHQueryUserNotificationState`; a reading that is missing or stale does not hold, like `min_idle_secs`, because these are about the person's comfort and a hung supervisor must not park the worker. 60% is a constant, like the safety thresholds.
- **`lend_hours` is a condition, not a scheduler.** One string, `[days] HH[:MM]-HH[:MM]` windows split by `;`. A window past midnight belongs to the day it starts. The clock is the machine's local time from the C library (`TZ`, else `/etc/localtime`, which WSL sets from Windows). The key is on the open form: narrowing it protects the owner, like `min_idle_secs`.
- **CPU time and memory peak per job.** The 30-second reading that already took each running job's CPU throttling now also keeps its CPU time and its memory (`usage` minus `inactive_file`, as `docker stats` shows it); at the end the last CPU time and the highest memory go into the job's row (`cpu_ms`, `memory_peak`). `jobs`, `jobs --json` and the page show them. They do not go into the callback: the contract is unchanged.
- **`--owner-consent`.** `shutup-and-run wsl install --owner-consent`, and `shutup-and-run safety authorize --owner-consent` on a machine already installed, record the owner's consent to every warning of the current notice (`safety::WARNING_CODES`) with the channel `contract`, without asking. It is a flag on a command run at the machine by whoever installs it, because that is where the contract is put into effect; there is still no config key, env var, API field or route that grants consent. A block is still a block, a new `NOTICE_VERSION` asks again, and the owner can withdraw it on the page.

**Cost.** A machine whose WSL2 clock is in the wrong zone lends at the wrong hours; the page shows the time next to the window so it is seen. A meeting in a window, not in full screen, is not noticed. The usage figures miss the last seconds of a job and peaks shorter than 30 seconds, and a job under 30 seconds may have none. The consent from the contract is only as good as the contract: the worker cannot tell whether the owner read it, and the page is the place where the owner can still say no.

## lend_hours on the Windows clock, a pause for a while, a suggested capacity, three releases kept (0.2.20)

**Context.** `lend_hours` was read on the clock inside WSL2. WSL sets that zone from Windows when the distribution starts, so a zone changed later in Windows, or a distribution that did not get one, moves every window by hours without a word. A pause lasted until someone resumed or the PC restarted, and the person who pauses for a meeting wants to forget it. Each job records what it used (0.2.19), and turning that into "how many fit" was left to arithmetic. The releases piled up: every patch version was kept, with no one installing an old one.

**Decision.**

- The supervisor sends the Windows offset from UTC on the host feed (`GetTimeZoneInformation`, daylight saving included), and `lend_hours` is read on it; without that reading, on the clock inside WSL2. The page, `doctor` and `status.json` show the time with its zone. A `CLOCK_ZONE_MISMATCH` warning was the first idea; following the Windows clock removes the mismatch instead of reporting it.
- `pause --for 30m|2h|1d` (a minute to a week) and the page's **1 hour**, **3 hours** and **rest of the day** (to midnight on that same clock) store the second the pause ends in the same `meta` row. It is over when that second passes, with nothing to clear, and it outlasts a restart, which still ends a pause with no end.
- `jobs --summary` and the doctor's `ROOM_FOR_MORE_SLOTS` suggest twice the highest memory peak of the last 20 jobs, rounded up to 256 MiB and never above the limit in force, and the slots that fit in this worker's memory and processors at that size. Only from five jobs measured, only when that is more slots than now, and never applied by itself.
- The `release` workflow keeps the three newest releases and deletes the others with their tags, after each release and when it is run by hand.

**Cost.** A PC whose Windows clock is wrong lends at the wrong hours, as the person sees them. A suggestion from 20 jobs is only as good as the next jobs are alike; a short peak between two readings is missed, which the factor of two is there for. An old version can no longer be installed from GitHub, and a supervisor older than three releases cannot fetch its own Linux binary for a repair: it has to be updated first.

## Memory: what to close, how much, and what WSL2 gives back (0.2.20)

**Context.** On a client's PC the usual reason no job starts is memory, and "low on memory" told nobody what to do. A Windows PC's memory goes to the person's programs, and to the WSL2 VM, which keeps as cache what a job read and wrote until WSL restarts, so `vmmemWSL` looks like it holds the memory after the job is gone.

**Decision.**

- While the PC's memory is tight (under 20% available, or under 1.5 GiB), the supervisor adds the five programs holding the most memory to its line on the service's stdin. They are added up per program; each process counts as the smaller of its working set and its private bytes, which is close to the memory only it holds. Windows' own processes, the antivirus, WSL and the worker are never listed. The list goes to the local page and the local doctor; it never goes to `status.json` (which the service also logs), `doctor --report`, a callback or the poll body.
- The amount to free is the gap to the line at which a new job can start: a block's recovery line (15%, at least 1.5 GiB on the PC), the 20% warning line while the owner has not agreed to it, or the free memory the VM needs for a job.
- The service keeps the lowest memory of each hour of the week (168 rows, a row older than two hours replaced on the next pass of that hour) and the seconds new jobs waited for memory per day. An hour under 20% is short. When short hours fall inside `lend_hours` (or any hour, without it), the doctor suggests the `lend_hours` that remains without them, written by the same parser's rules. It is a suggestion: `lend_hours` may be in the contract.
- When the last job on a Windows PC ends, the service writes `1` to `/proc/sys/vm/drop_caches` (and asks for compaction), so the VM hands the memory back to Windows at once. `wsl limits --apply` adds `autoMemoryReclaim=dropcache` under `[experimental]` when no value is set, for the rest of the VM's idle time; an owner's own value, `disabled` included, is kept. `dropcache` rather than `gradual`: it is the simpler of the two (drop the cache once the VM is idle) and matches what the service does after a job.

**Cost.** The program list is a guess at what closing a program frees: shared memory is not counted, and a process of another user or of the system cannot be read. The hour map shows the last seven days, not a typical week. Dropping the cache makes the next job read its image from disk again. `autoMemoryReclaim` is an experimental WSL key; a WSL that moves or drops it ignores the line.

## The WSL2 clock follows Windows, and what Windows testing found (0.2.21)

**Context.** A hand test on a Windows notebook found the clock inside WSL2 35 seconds ahead of Windows, which is a known WSL2 drift after the PC sleeps; times on the page, `lend_hours` and the end of a pause were off by that much. The page wrote its times in UTC until JavaScript changed them, so a script or an agent reading the page saw UTC under a footer that says the PC's time. The doctor said `ok, authorized` and a minute later `AUTHORIZATION_REQUIRED`, both true: another warning had come up, and nothing said which was already agreed to. A day of memory readings was enough for a `lend_hours` suggestion, since an hour never read counted as one with memory to spare.

**Decision.**

- The supervisor's line carries the Windows time (`unix_ms`). The service, root in its distribution, sets the VM's clock to it with `clock_settime` when they are 5 seconds or more apart, as `hwclock -s` would; the clock belongs to the WSL2 VM and is shared by its distributions, and Windows' time is the one the person sees. When it cannot, `status.json` has `lending.clock_skew_secs` and the doctor `CLOCK_SKEW`.
- The page writes a time on the PC's clock with its zone (`2026-11-18 15:30 UTC-03:00`) from the offset on the feed; the browser still turns it into its own local time.
- A wait for authorization names the warnings already covered.
- `MEMORY_SHORT_HOURS` names the short hours from the first reading and suggests a `lend_hours` only once 72 hours of the week have one (`memory::MIN_SAMPLED_HOURS`).
- `pause --for day` and `pause --until HH:MM` end on the PC's clock, read from `lending.utc_offset_secs` in `status.json`, since the CLI inside WSL2 does not get the feed.
- A running job's readings go to its row as they come, so its CPU time and memory show while it runs (`so far`).

**Cost.** The service changes a clock other distributions on the PC also read; it only ever moves it toward Windows'. A Windows clock that is wrong is now wrong inside WSL2 too. Three days of readings is a short week.

## The worker cleans up after itself; the WSL2 disk is compacted by a person (0.2.22)

**Context.** Images were removed only with `prune_interval_secs`, off by default, and then every image of every finished job, so the next job pulled it again. On a client's PC the images of jobs that stopped coming stayed for good, and so did the old copy an image pulled again under the same tag leaves behind. On Windows the WSL2 disk file grows with them and never shrinks: what is deleted inside is free for WSL2, not for Windows. The owner wants a worker that does not pile up leftovers on someone else's PC.

**Decision.**

- The history keeps each image's last use (`images`: the start of its last job; filled from the jobs on the first start of 0.2.22). Once a day the service removes the images unused for 30 days that no container uses, and forgets them. 30 days is a constant, always on, like the 7 days of job logs. On a Windows PC, where the distribution and its Docker are the worker's own, it also removes the untagged images and runs `fstrim /`; on Linux the daemon is the owner's, and so are its untagged images. `status.json` (`cleanup`), the page (**Cleanup**) and `doctor` (`cleanup`) say when it last ran and about how much it freed: free disk after minus before, which other writes can blur.
- The WSL2 disk file is not made sparse. A sparse disk would hand freed space back to Windows by itself, but recent WSL versions refuse it without `--allow-unsafe`, over a risk of corrupting the disk, and the worker does not take that risk on a client's PC. `wsl doctor` shows the file's size next to what is in use and says `WSL_DISK_TO_COMPACT` when the file is twice that and 5 GiB more; compacting needs `wsl --shutdown` and an administrator's `diskpart`, so a person does it (docs/WINDOWS.md).
- `status` prints the rest of `status.json` that has something to say. `jobs --summary` adds one line per image, with the `memory` to ask for it from three jobs measured.
- `tests/windows-round/check.ps1` runs the part of a Windows hand test no person is needed for and writes a Markdown table; CI only checks that it parses.

**Cost.** An image used once a month is pulled again each time. An image pulled before 0.2.22 whose jobs were already purged from the history is not known, and stays. Until someone compacts it, the disk file on Windows keeps its largest size.

## Not on a metered network; network per job; a running job's output (0.2.23)

**Context.** A notebook on a phone's hotspot pulled images of hundreds of MB and ran jobs on the person's data plan. Nobody could tell how much data a job of an image moves. And what a job printed was only readable after it ended, so a job that seemed stuck could not be looked into.

**Decision.**

- A fourth wait in `src/lending.rs`: `METERED_NETWORK`. The supervisor reads `GetNetworkConnectivityHint` and sends `metered` on the host feed: a fixed or variable cost, roaming, or over the data limit. Like the others it stops admission and the poll, never a running job, and an unknown cost does not hold. Unlike the battery, it can be lifted: `lend_on_metered`, on the open form, because the data is the person's and some have a plan that does not care. Off by default.
- The 30-second `docker stats` reading also gives the bytes received and sent on the container's networks; the most a reading saw is `network_bytes` in the row, in `jobs`, on the page and per image in `jobs --summary`. The pull is the daemon's and is not counted; a job shorter than 30 seconds has no number.
- `jobs logs ID` and the page read a running job's output from Docker at that moment (`live: true`); the file is still written only when the job ends. Same content, same readers, nothing kept.

**Cost.** A Wi-Fi that Windows does not know is a phone's hotspot counts as unmetered unless the person marks it. A job that only waits on a metered network waits until the PC changes network. The network number misses up to 30 seconds of traffic at the end of a job.

## The pull's own limit, Docker's log capped, disk per job, digests not pulled again, when lending resumes (0.2.24)

**Context.** The pull had the job's `timeout_secs` as its limit. A short job of a big image on a slow line ended `start_failed` ("image pull timed out") before it ran, and failed the same way every time (CI's smoke test hit it with a 2-second job); a 6-hour job let a stuck pull hold its slot for 6 hours. Docker kept everything a container printed, so a chatty job or a managed `always` container could fill the WSL2 disk, which does not give space back to Windows by itself. A job that wrote 30 GB inside its container went unseen: `min_free_disk` is read only at admission. An image named by its digest was still asked of the registry on every job. And outside `lend_hours` the page said the PC waited, not until when.

**Decision.**

- The pull has a fixed limit of 30 minutes (`launcher::PULL_LIMIT_SECS`), apart from `timeout_secs`, which still counts from the container's start. A constant, not a setting.
- Every container is created with the `json-file` log driver, `max-size` 5m and `max-file` 2: at most 10 MB of output per container, read by `docker logs` and by the job's log file as before. It also overrides another driver an owner set on a Linux daemon, for the worker's containers only.
- Before a finished container is removed, the worker asks Docker its `SizeRw` (what it wrote to its own layer, bounded at 20 seconds) and keeps it as `disk_bytes`: in `jobs`, on the page and per image in `jobs --summary`. A number, not a limit.
- An image named `repo@sha256:...` that is already on the machine starts without asking the registry. A tag is still pulled every time.
- Outside `lend_hours`, the wait says when the next window opens: `; next window wed 19:00` in its detail, the time on the page's state line, and `lending.resumes_at` in `status.json`. The other waits have no known end and say nothing new.

**Cost.** A pull that really needs more than 30 minutes now fails where a long job used to let it finish. A job that prints more than 10 MB keeps only the last part in Docker (the log file kept 256 KiB anyway). Sizing the layer walks it, which on a job that wrote many files takes a while at its end. A digest is trusted to be what is on the machine.

## SIGTERM before SIGKILL, process limit and init, no leftover volumes or journal, spread-out retries (0.2.24)

**Context.** A timeout or a stop was SIGKILL at once, so a job could not write its result or close its browser, and a Chrome profile was left half written. Nothing limited a container's processes: a job that forked without end slowed the whole PC down within its CPU and memory limits, and the zombies a browser leaves piled up. A container was removed without its anonymous volumes, so every job of an image with a `VOLUME` left one on the WSL2 disk for good. The distribution's systemd journal could grow to 4 GB. And after an outage of the poll URL every PC retried on the same doubling schedule, so a fleet came back in waves on the same second.

**Decision.**

- A timeout and a stop send SIGTERM (or the image's `STOPSIGNAL`) through `docker stop`, and SIGKILL 10 seconds later (`bollard_rt::KILL_GRACE_SECS`). The row keeps `exit_code` 137 for `timeout_kill` and `launcher_kill` whatever the job answered, so a caller that reads 137 as killed does not change.
- Every container gets `PidsLimit` 4096 and Docker's `init` as its first process, under any profile: a constant, not a setting, and not a request field. `docker.io` on Ubuntu ships `docker-init`.
- A container is removed with `v`, so its anonymous volumes go with it. Named volumes cannot exist: a request has no mounts.
- `wsl install` and `wsl update` write `/etc/systemd/journald.conf.d/shutup-and-run.conf` (`SystemMaxUse=100M`) in the distribution; a failure is a warning.
- After a failed poll each machine waits the doubled time plus up to a quarter more at random. A success goes back to the plain interval from where it landed, so the fleet stays spread. Never under the 30-second minimum.

**Cost.** A job past its time holds its slot up to 10 seconds longer, and a stop on the page answers up to 10 seconds later. A job that really needs more than 4096 processes fails. The row's 137 hides that the job ended cleanly on SIGTERM; its log says so. A retry after an outage can come up to a quarter later.

## Download progress, Docker's data-root, a capped credential helper, an update notice, the .wslconfig on uninstall (0.2.25)

**Context.** A job whose image was downloading looked the same as one running: nothing said how far the pull was, and a 2 GB image on a slow line looked stuck. The free-disk reads assumed `/var/lib/docker`, so a machine whose Docker keeps its images on another disk was measured on the wrong one. A credential helper that hung (a keyring waiting for a prompt nobody sees) held the pull until its 30-minute limit. A PC on an old release had no way to hear of a new one but the operator. And `wsl uninstall` left the `.wslconfig` the install had written, so WSL2 stayed capped after the worker was gone.

**Decision.**

- While a pull lasts, Docker's per-layer messages are summed (bytes so far, size of the layers named so far) and written to the job's row at most every 3 seconds (`pull_done`, `pull_total`), cleared when the pull ends however it ended. `jobs`, the page and `status` show `downloading the image: X of Y`; `status.json` has `pulling`, which does not by itself make the file rewritten, so it is at most 30 seconds old. Not in the callback or the poll body.
- The disk checks read `data-root` (or the old `graph`) from `/etc/docker/daemon.json`, then `/var/lib/docker`. A `--data-root` on dockerd's command line is not looked up (a shortcut).
- A credential helper has 5 seconds; then it is killed and the pull goes on anonymously, as on any helper failure.
- `wsl doctor` asks GitHub where `releases/latest` points (a HEAD, no redirect followed, 5 seconds) and warns `UPDATE_AVAILABLE` when that is newer than the `.exe`. No answer, no line. Only `doctor` asks; nothing updates by itself.
- The install records the sha256 of the `.wslconfig` it writes. `wsl uninstall` removes that file only when it still has that hash and no other WSL distribution is left; any other file stays.

**Cost.** One more write to the history every few seconds while an image downloads. The second number can grow during the pull, since Docker names layers as it goes. The doctor makes one request to github.com. A `.wslconfig` that `wsl limits --apply` rewrote after the install no longer matches and stays.

## Internet sharing: a reverse connection pool in the binary (alpha, 0.2.26)

**Context.** Many clients' PCs are too weak for containers or cannot run WSL2, but the operator's automations still need to reach courts and sites from the client's residential address, not the datacenter's. The PC is behind NAT or CGNAT, so the connection has to start from it. Chrome takes no SOCKS5 login and no proxy login on its command line. Security was explicitly not a goal of the first version: the client accepts the risks in the contract.

**Decision.**

- A mode of its own, `internet on|off|status|credentials|serve`, apart from the worker: its own file `internet.json` beside the supervisor's files (Windows) or the history (Linux), its own process run by a logon task or a systemd unit, and a read-only page on 127.0.0.1:8788. It must run on a PC with no worker, no WSL and no Docker, and while the worker lends nothing.
- The PC keeps 4 idle TCP connections to the operator's relay (`shutup-and-run relay`, the same binary). One connection per stream, no multiplexing: the relay sends `C host:port`, the PC dials the site and both copy bytes. Pings every 20 s; 60 s of silence and the connection is dialed again, with a backoff from 1 to 30 s.
- The relay is an HTTP proxy (`CONNECT` and `http://`) whose login names the PC; `--open PORT=ID:SECRET` gives Chrome a port with no login. It keeps nothing on disk.
- The PC refuses destinations on its own network, on the address it dials.
- New streams wait like new jobs for the worker's pause, hours, battery, presenting and a metered network; never for memory or the safety levels, which this mode does not load. An open stream is never cut.
- Bytes are counted on the site's side of each stream, kept across restarts in the status file.
- OpenSSH's `ssh -R`, chisel, frp, gost and rathole were weighed in docs/INTERNET.md: a second program, a key per PC, no byte count, and Defender's HackTool alerts cost more than about 1,600 lines of Rust with no new crate.

**Cost.** Plain TCP between the PC and the relay: the password and the names of the sites can be read on the way; HTTPS content cannot. Any PC can connect to the relay, and after a relay restart the first to connect sets an id's password. One TCP connection per stream means one round trip to the relay before each stream, and four idle connections per PC on the relay. UDP, QUIC and WebRTC do not go through. A second page and a second file to keep in step with the worker's, and `doctor` on a Windows PC without WSL still fails on its first line (`internet status` is the check there).

## Internet sharing: a dead link skipped, IPv4 first, a token, a limit a day, one-line install (0.2.27)

**Context.** In the first days of the alpha: right after a PC woke or changed its Wi-Fi, the relay handed a request to a connection that was already dead and answered `502` after 15 seconds. A home with broken IPv6 spent up to 10 seconds per stream on it, and a site could see an IPv6 address other than the one a test showed. Four idle connections were too few for one Chrome page. An id lost its password the moment its PC slept past a minute. A crash of the process waited for the next logon. After a sleep, streams whose far end was gone stayed open for good. And a client had to download the `.exe` by hand.

**Decision.**

- A claimed connection answers `A` at once; without it in 3 s the relay tries the next one, up to 3. The relay tries first the connection heard from in the last 15 s. The greeting is now `SAR2`; a 0.2.26 PC is told to update.
- Pings every 10 s, a link silent for 30 s (35 s on the relay) is dropped.
- IPv4 first, 3 s per address, IPv6 only when no IPv4 answers.
- 16 idle connections per PC.
- An id keeps its password 10 minutes after its last connection.
- `relay --token-file`: only PCs that send that token connect (`internet on --token`).
- A stream with no byte either way for 10 minutes is closed, on the PC and on the relay.
- `--max-day`: past it, new streams wait until midnight on the PC's clock.
- The logon tasks (the worker's and this one) restart a process that ended with an error a minute later, up to 999 times.
- `install.ps1` with `SHUTUP_AND_RUN_RELAY` runs `internet on`, and restarts internet sharing after an update. `wsl uninstall` removes it too.
- `GET /status` on the proxy port, with a PC's login, says whether it is connected; once a minute the relay logs each PC whose numbers changed.
- A task goes out through a PC by its `env` (`HTTPS_PROXY`); the request has no proxy field.

**Cost.** One more round trip before each stream. A 0.2.26 PC stops connecting until it is updated. A site reached only over IPv6 waits for the IPv4 attempts first, 3 s each. A WebSocket or a long poll silent for 10 minutes is closed. An id whose PC changed its password waits 10 minutes. A task that keeps failing at once restarts every minute instead of stopping.

## Internet sharing beside the worker: one command for an agent, a login from the operator, a pause and a stop on the page (0.2.27)

**Context.** The PCs are clients the operator trusts, who trust the operator; their own AI agent sets things up. The agent had to combine three documents, run `on`, poll `status` until connected, and send the generated password back to the operator's backend. The install line lent either the internet or containers, never both. A PC that only lent its internet had no pause, and the person at it could stop it only from a terminal.

**Decision.**

- `fit --json`: `both` or `internet`, from what the doctor and the safety levels already read: WSL2 on Windows (`WSL_UNAVAILABLE`), and a PC above the small-machine line (`SAFETY_SMALL_MACHINE`, which would need the owner's authorization for jobs). No new code.
- `internet on --login ID:PASSWORD`: the operator's backend chooses the login, so nothing comes back from the PC; the backend checks the PC with the relay's `GET /status`. `--wait SECONDS` returns once connected, or exits 1 with the code and the hint (`INTERNET_REFUSED`, `INTERNET_WAITING`, `INTERNET_RELAY_UNREACHABLE`). It takes only a status written after it began, and for a new login a connection made after it began.
- The install lines (`install.ps1`, `install.sh`) do both: `SHUTUP_AND_RUN_RELAY` turns on the internet, and `SHUTUP_AND_RUN_POLL_URL` the worker too, on Windows only where `fit` says containers. Not connected within 60 s is not a failed install. `install.sh` gets `SHUTUP_AND_RUN_INTERNET_ONLY=1` (no Docker).
- `status` shows the internet line and an `internet` section; with no worker it says `"worker": "not installed"` and takes the internet's exit code.
- Its own pause (`internet pause --for`, `internet resume`, `paused_until` in `internet.json`), and on the page two buttons: pause 1 hour (or resume) and stop sharing, behind the same loopback and same-origin check as the worker's page. The stop deletes the task or unit without ending the process, which sees the file and exits cleanly; the systemd unit is now `Restart=on-failure`, so a clean exit stays stopped.
- Nothing was cut: `--open` ports, the page, plain TCP and a relay without a token stay as options, with their risks in docs/INTERNET.md.

**Cost.** The page is no longer read-only: anyone at the PC can stop the sharing, which is the point. The backend holds every PC's password (`--login`). An older binary refuses an `internet.json` with `paused_until` (unknown fields stay refused), so going back a version after a pause needs `internet on` again. `fit` on Linux does not look for Docker: the install puts it there.

## The relay in one line, its proxy on Docker's bridge

**Context.** An operator had to read `examples/relay`, install Docker and write a token by hand to have a relay. And the compose file published the proxy only on its own network `egress`, which the jobs of a shutup-and-run worker on the same server never join: a request cannot choose a network, so those jobs could not reach the proxy at all.

**Decision.** `install.sh` with `SHUTUP_AND_RUN_RELAY_SERVER=1` installs the binary, makes `/etc/shutup-and-run/relay_token`, and writes the systemd unit `shutup-and-run-relay`: PCs on `0.0.0.0:7000`, the proxy on Docker's bridge address (read from `docker0` or `docker network inspect`; loopback until Docker exists; `SHUTUP_AND_RUN_RELAY_PROXY` for a VPN address). It prints what to give the clients, the token only with `SHUTUP_AND_RUN_SHOW_TOKEN=1`. The plain install line restarts the relay on the new binary, and `--uninstall` removes it. The compose file also publishes the proxy on `172.17.0.1:8080`. docs/SERVIDOR.pt-BR.md walks an operator through it.

**Cost.** Every container on that server's default Docker network reaches the proxy, not only the worker's jobs; the PC's login still guards it. A relay installed before Docker sits on loopback until its line runs again. The unit runs as root, with `ProtectSystem=strict`; the relay writes nothing.

## The poll URL says where the relay is; the relay as an image (0.2.28)

**Context.** Every client had to be told the relay's address, token and login, and an operator with a server already running Docker had to clone the repository and build an image to run a relay.

**Decision.**

- `internet on --discover URL` (the install lines: `SHUTUP_AND_RUN_LEND_INTERNET=1` with `SHUTUP_AND_RUN_POLL_URL`) sends one `POST` to the operator's URL, the poll URL serving, with `"lend": "internet"` and the poll token when set, and takes `relay`, `token` and `login` from a `200` answer. It is the person's own command, so it is the same consent as `--relay`; the URL only says where. It is asked once per `on`, not on a schedule, and nothing else from the poll touches internet sharing. Two schemas, `internet-discovery-request` and `-response`, held by `src/contract.rs`.
- The release workflow builds `ghcr.io/nailtongomes/shutup-and-run-relay` from the release's own musl binary (checked against `SHA256SUMS`) and pushes it as the version and `latest`: a relay pulled is the same release as the PCs. The relay takes its token from `SHUTUP_AND_RUN_RELAY_TOKEN` when there is no `--token-file`, so a `docker run` or a compose service is one line of env. `examples/relay/compose.yml` uses the image, fails without a token, and is a service to paste into a compose file that exists.
- The READMEs open with the ways to lend a PC, one row each.

**Cost.** A PC told by the URL keeps that relay until the next `internet on --discover`: moving the relay means running the install line again on each PC. The poll URL learns which PCs lend their internet. The image is built only after a release (or "Run workflow" on main for the newest one), not on every push. A relay with no token in its env accepts any PC, as before.

## The operator's list of PCs, room for 10 consumers, the worker image per release, n8n as the hub

**Context.** The operator's backend could ask the relay about one PC at a time, with that PC's login. Ten browsers at once through one PC went near its 256 streams. The worker image had no version tag since v0.2.9. And a server with a relay still needed a backend to answer the discovery and to send a job through a chosen PC.

**Decision.**

- `GET /pcs` on the relay's proxy port, with the relay's token as a bearer, lists every PC the relay knows: id, connected, idle, open, seen, and bytes since the relay started. No password, no address. A relay without a token has no list (`404`): the token is the operator's, the PCs' logins are not. It is on the proxy port, which only the operator's containers reach.
- A PC keeps 32 idle connections and up to 512 streams. 10 Chromium at once held up to 192 streams, three quarters of the old 256; the old numbers were as fast, so this is margin, not speed (`tests/internet/local.sh` with `SHUTUP_AND_RUN_LOAD=1`, numbers in docs/INTERNET.md). Constants, like the rest.
- The release workflow's `images` job pushes the worker image as `v<version>` and `latest` beside the relay's, from the same checked binary.
- `examples/n8n/worker-orchestrator.json` answers the discovery (a login per PC, made by a Crypto node and kept) and queues a job `via_internet`, which puts that PC's proxy in the task's `env`. The queue stays in the orchestrator, as before; the relay and the worker get no queue.
- CI runs internet sharing on the Windows runner: `install.ps1` with a relay, `curl.exe` through the PC, `/status` and `/pcs`, the page's pause, resume and stop.

**Cost.** Anyone with the relay's token reads which PCs are there and how much they carried; the PCs hold that token too, but reach only port 7000, not the proxy. 32 idle connections are 32 sockets on the PC and the relay per PC, and a ping each every 10 s. The n8n flow answers any discovery that reaches it, without approval, up to 200 PCs: a stranger with the URL can lend you a connection you never use. The Windows CI step proves the path on a runner, not on a home PC behind NAT.


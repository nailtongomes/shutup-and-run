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

v0.1.0's `SHA256SUMS` named the file `dist/shutup-and-run-x86_64-unknown-linux-musl`, because `sha256sum` was run on that path. The installer matches the base name, so a sums file in that shape still matches, and a different hash still refuses. Later tags hash from inside `dist/` and record the base name only. The v0.1.0 tag and its GitHub release were removed. Install from `releases/latest`.

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

## Smoke test against a running instance

`scripts/accept.sh` starts its own instance. `scripts/smoke.sh` tests one that is already installed, through the API only, with the URL and token from the environment. It is safe on a live machine: its jobs echo and sleep under a short `timeout_secs`, it undrains what it drained, and it sends wrong tokens from `127.0.0.2`, never from the caller. What it cannot do from where it runs, it skips and says why.

CI runs it twice: after `accept.sh` on the same instance, and against a service that `install.sh` installed from a local copy of the release assets. The release workflow runs it a third time after the real one-line install.

The cost is that the instance must allowlist an image for the test. The owner chose two: a small Python image built from `examples/smoke-image` and published as `ghcr.io/nailtongomes/shutup-and-run-smoke`, which is the default, and Docker's `hello-world`, which has no shell and so runs a reduced set. A worker image of your own works too if it has `sh` and `sleep`. There is no cancel call in the API, so cleanup is waiting for the test's jobs to end.

## The name

The owner named it `shutup-and-run`. It is the whole product: send a job, get a container, or get a refusal. Earlier nautical names (`berth`, `kedge`, `cleat`, and others) were already used by other Docker tools.

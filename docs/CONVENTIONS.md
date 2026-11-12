# Conventions

shutup-and-run does not know what a container does. It starts the image you named. This file is the agreement you accept by running it. The process does not police the items under "You do this". It always applies the items under "The process always does this", and a request cannot turn them off.

Read this with [SECURITY.md](../SECURITY.md). The record of why it stays this way is [DECISIONS.md](DECISIONS.md).

## You do this

**One machine, one caller.** The token is a root password for the machine. There is one token and no second tenant. Ideally the host runs this process and its jobs and nothing else.

**A machine someone lends you.** A client may offer a machine so your jobs can use its idle capacity. That is allowed, and it is a risk both sides take on purpose:

- The lender accepts that the token holder can run any allowlisted image on their machine, with Docker's default privileges and outbound network access. They are trusting the caller's images.
- The lender stays in control. They install the service, write the allowlist, set `max_slots`, can read `docker ps` and the service log, and can stop the service and kill every job without asking anyone.
- The caller sends `cpu` and `memory` with every job, uses a separate token for each lent machine, and does not print secrets, because the last log lines are stored on the lender's disk.
- Other workloads on that machine are not protected from a job by anything except those limits. If that is not acceptable, lend a separate machine or a VM.

The commands for both sides are in [RECIPES.md](RECIPES.md).

**A Windows machine someone lends.** The same agreement, with two differences the lender should hear before saying yes. The worker only polls your orchestrator, and it runs whatever image that orchestrator names: there is no allowlist on it (`poll_any_image`), so the lender is trusting your orchestrator, not a list on their disk. And everything runs inside a WSL2 VM that the lender can cap (`--wsl-memory`, `--wsl-processors`) and remove in one command (`shutup-and-run wsl uninstall --purge`). The containers cannot see the Windows drives. [WINDOWS.md](WINDOWS.md) is what the lender reads.

**Your Linux machines are trusted; a client's Windows PC is not.** On Linux the token holder and the machine owner are you. On a client's PC the person at the keyboard can read the poll token and the `env` of every task that runs there. Give each such machine its own token, have the poll URL approve a machine before it answers with a task, send it only secrets scoped to that task, and treat what it reports as a claim.

**Images you own.** Every allowlist entry is a repository you control, under a registry namespace you control. Do not allowlist a public library image or someone else's repository. An entry is one repository (any tag, or exactly one tag or digest if you pin it). It is not a wildcard. `ghcr.io/you/worker` does not allow `ghcr.io/you/worker-evil` or `ghcr.io/you/other`. Add each repository yourself.

**Secrets only in the task environment.** Pass secrets in the `env` object of the POST body or of the polled task. Do not bake them into the image. This process has no secret store. SQLite stores callback URLs and the last log lines, and those lines can contain whatever the container printed. The file is mode 600. Treat the disk that way. Tokens are not written to the log.

**Network.** Bind `listen` to an address you mean to expose. The installer's generated config uses `0.0.0.0`, which is wider than the example file. Firewall the port. Leave `trust_x_forwarded_for` off unless a proxy you control is the only path in. If you set `poll_url`, that endpoint is one you run. The process will POST to it.

**Tokens.** `SHUTUP_AND_RUN_TOKEN` authenticates callers. `SHUTUP_AND_RUN_POLL_TOKEN` is what this process sends when it polls, and it is read from the environment only. A `poll_token` key in the TOML file is an unknown field and the process will not start. Keep both in `/etc/shutup-and-run/env` (mode 600). The systemd unit loads that file. Rotate one by editing the file and `sudo systemctl restart shutup-and-run`. The installer does not rotate a token that already exists. Do not commit either token, and do not pass them on a command line that lands in shell history.

**Certificates.** Pin the fingerprint the installer prints, or replace the self-signed certificate. Skipping TLS verification is a choice you make in the caller. It is not a default of this process.

**The release binary.** `scripts/install.sh` downloads the GitHub release asset and `SHA256SUMS` from that same release, and stops if the hash does not match. `SHUTUP_AND_RUN_BIN` installs a binary you already have and skips that check.

**Callbacks.** `callback_url` is called from this host, three times, with no redirects. Point it only at an endpoint you trust. Anyone who holds the token can set any http or https URL the host can reach, including link-local addresses.

**Jobs can use the machine.** A job container is not given a host mount, a published port, or privileged mode. It still runs as whatever user the image selects, with Docker's default capability set, on the default bridge, and it can open outbound connections. If you omit `memory`, Docker does not cap the container. Set `memory` and `cpu` on the task when a runaway matters. The host Docker socket is not mounted into the job. You mount it only into this process, and only if you run this process as a container.

**Pull is optional.** Leave `poll_url` unset and nothing calls out. When it is set, the poll body names this machine (`worker_name`, `worker_os`) and its free slots. A `manifest_url` in the answer is a URL your orchestrator chose; the poll token goes with that request only when the manifest is on the same origin as `poll_url`.

**`poll_any_image` is for a machine with one source of work.** It makes a polled task's image whatever the poll answer says. Turn it on only where the orchestrator behind `poll_url` is the only caller, as on the Windows worker. A server that also accepts pushes keeps its allowlist, and the flag never loosens a push.

## The process always does this

A request cannot override these. Unknown JSON fields are rejected. The container host config is built in code from the image, env, command, cpu, memory, shm, and a deadline. Nothing else is copied from the request.

- Privileged mode is not set. Docker's default is off. The request has no field for it.
- No volume, bind, device, or extra host is set.
- No port is published.
- No capability is added. None are dropped either. Docker's default set is what the container gets.
- No PID, IPC, UTS, or network mode is set. The container joins the default bridge.
- No restart policy. A container that exits stays stopped.
- When `memory` is set, `MemorySwap` is set to the same value, so there is no extra swap.
- The deadline is a label. The reaper sends SIGKILL. There is no SIGTERM grace.
- The image name must pass `validate_image_name` (lowercase, no spaces, no `..`) and match an allowlist entry. An entry that names a tag or a digest matches only that exact string. Nothing after the entry may contain a `/`.
- `callback_url`, and `poll_url` when you set it, must be `http` or `https` with a host. Redirects are not followed. `poll_url` must not carry a username or password.
- Every HTTP route on this process requires the bearer token. Failed attempts are counted per peer address and then banned. The ban survives a restart.
- TLS is on unless `http = true`. Plaintext on `0.0.0.0` logs a warning.
- The TLS listener is HTTP/1.1 only. A connection that sends no request within 10 seconds is closed.
- The poll token is never read from the TOML file. The poll response is capped at 64 KiB, the same limit as a job POST.
- Job environment and the bearer tokens are not written to the log. A startup line for pull mode records the poll host, not the URL.

## What this will not grow into

No image signature check, no vulnerability scan, no user namespace, no read-only root, no dropped capabilities, no seccomp profile of our own, no per-tenant tokens, and no secret broker. Put those in the image, or in front of this process, if you need them.

## If you cannot follow one of these

Do not run the binary on that machine. There is no flag that makes a shared host safe. A lent machine is safe only to the extent both people have read this file and agreed.

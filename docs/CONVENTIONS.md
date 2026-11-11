# Conventions

shutup-and-run does not know what a container does. It starts the image you named. This file is the agreement you accept by running it. The process does not police the items under "You do this". It always applies the items under "The process always does this", and a request cannot turn them off.

Read this with [SECURITY.md](../SECURITY.md). The record of why it stays this way is [DECISIONS.md](DECISIONS.md).

## You do this

**One machine, one owner.** The host runs this process and its jobs, not other people's workloads. The token is a root password. There is one token and no second tenant.

**Images you own.** Every allowlist entry is a repository you control, under a registry namespace you control. Do not allowlist a public library image or someone else's repository. An entry is one repository (any tag, or one tag if you pin it). It is not a wildcard. `ghcr.io/you/worker` does not allow `ghcr.io/you/worker-evil` or `ghcr.io/you/other`. Add each repository yourself.

**Secrets only in the task environment.** Pass secrets in the `env` object of the POST body or of the polled task. Do not bake them into the image. This process has no secret store. SQLite stores callback URLs and the last log lines, and those lines can contain whatever the container printed. The file is mode 600. Treat the disk that way. Tokens are not written to the log.

**Network.** Bind `listen` to an address you mean to expose. The installer's generated config uses `0.0.0.0`, which is wider than the example file. Firewall the port. Leave `trust_x_forwarded_for` off unless a proxy you control is the only path in. If you set `poll_url`, that endpoint is one you run. The process will POST to it.

**Tokens.** `SHUTUP_AND_RUN_TOKEN` authenticates callers. `SHUTUP_AND_RUN_POLL_TOKEN` is what this process sends when it polls, and it is read from the environment only. A `poll_token` key in the TOML file is an unknown field and the process will not start. Keep both in `/etc/shutup-and-run/env` (mode 600). The systemd unit loads that file. Rotate one by editing the file and `sudo systemctl restart shutup-and-run`. The installer does not rotate a token that already exists. Do not commit either token, and do not pass them on a command line that lands in shell history.

**Certificates.** Pin the fingerprint the installer prints, or replace the self-signed certificate. Skipping TLS verification is a choice you make in the caller. It is not a default of this process.

**The release binary.** `scripts/install.sh` downloads the GitHub release asset and `SHA256SUMS` from that same release, and stops if the hash does not match. `SHUTUP_AND_RUN_BIN` installs a binary you already have and skips that check.

**Callbacks.** `callback_url` is called from this host, three times, with no redirects. Point it only at an endpoint you trust. Anyone who holds the token can set any http or https URL the host can reach, including link-local addresses.

**Jobs can use the machine.** A job container is not given a host mount, a published port, or privileged mode. It still runs as whatever user the image selects, with Docker's default capability set, on the default bridge, and it can open outbound connections. If you omit `memory`, Docker does not cap the container. Set `memory` and `cpu` on the task when a runaway matters. The host Docker socket is not mounted into the job. You mount it only into this process, and only if you run this process as a container.

**Pull is optional.** Leave `poll_url` unset and nothing calls out.

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
- The image name must pass `validate_image_name` (lowercase, no spaces, no `..`) and match an allowlist entry.
- `callback_url`, and `poll_url` when you set it, must be `http` or `https` with a host. Redirects are not followed. `poll_url` must not carry a username or password.
- Every HTTP route on this process requires the bearer token. Failed attempts are counted per peer address and then banned. The ban survives a restart.
- TLS is on unless `http = true`. Plaintext on `0.0.0.0` logs a warning.
- The poll token is never read from the TOML file. The poll response is capped at 64 KiB, the same limit as a job POST.
- Job environment and the bearer tokens are not written to the log. A startup line for pull mode records the poll host, not the URL.

## What this will not grow into

No image signature check, no vulnerability scan, no user namespace, no read-only root, no dropped capabilities, no seccomp profile of our own, no per-tenant tokens, and no secret broker. Put those in the image, or in front of this process, if you need them.

## If you cannot follow one of these

Do not run the binary on that machine. There is no flag that makes a shared host safe.

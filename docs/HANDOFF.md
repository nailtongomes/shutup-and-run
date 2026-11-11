# Handoff

Notes for someone reading this repository for the first time, including another model asked to review it. The product decisions are in [DECISIONS.md](DECISIONS.md). The operator agreement is in [CONVENTIONS.md](CONVENTIONS.md). Behavior for callers is in [README.md](../README.md) and [SECURITY.md](../SECURITY.md).

## What this is

shutup-and-run is one static Rust binary. It sits on one machine that has Docker. A caller POSTs a job. If a slot is free, the process starts an allowlisted container and returns `202` immediately. If not, it returns `429`. The caller owns the list of machines and tries the next one. There is no queue, no scheduler, no UI, and no second tenant.

A slot is a running container labeled `shutup-and-run=1`. The count is a Docker list, so a restart does not forget work that is still running. Finished jobs, the last log lines, the drain flag, and auth bans live in one SQLite file.

When `poll_url` is set, the same process also asks that URL for one task on an interval, and only if a slot is free and the process is not draining. The task is the same JSON as `POST /v1/jobs` and goes through the same `launch` path. When `poll_url` is unset, nothing calls out.

## What this is not

- Not a queue, a scheduler, or a multi-host control plane. A full machine is `429`. A draining machine is `503`.
- Not a sandbox. It does not drop capabilities, set a user namespace, or verify image signatures. See [CONVENTIONS.md](CONVENTIONS.md).
- Not multi-tenant. One bearer token is the whole ACL.
- Not a Windows or macOS program. That idea is only [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md).
- Not a place to put client names, real hostnames, or tokens. Examples use placeholders.

## Architecture

`serve` (the default command) loads config, connects to a local Docker unix socket, opens SQLite, and serves HTTPS (or plain HTTP when `http = true`). The HTTPS listener is HTTP/1.1 only and closes a connection that sends no request within 10 seconds. Every route goes through middleware that bans the peer after repeated bad tokens and then checks `Authorization: Bearer`.

`POST /v1/jobs` parses a body with `deny_unknown_fields` and calls `Launcher::launch`. Launch checks the image name, the allowlist, timeout, env, command, `job_key`, and `callback_url`. Then it holds one process mutex while it checks drain, the key, and the slot count, writes the row, and answers `202`. A spawned task pulls the image and creates the container, named `shutup-and-run-<job id>`. A pull or start that fails closes the row with `start_failed`. The ids of jobs between `202` and a running container are in `Launcher::starting`; they count as used slots. The host config sets cpu, memory, memory swap equal to memory, and shm. It does not set mounts, ports, or privileged.

A background task reaps containers past the deadline label with SIGKILL, purges old rows, and optionally prunes. Docker events and a reconcile pass on startup close rows when a container exits. An optional `callback_url` is POSTed three times with no redirects and does not hold a slot.

`shutup-and-run bans` and `shutup-and-run bans lift <ip>` read the same SQLite file over SSH. They do not open the HTTP port. They load only `history_path`, not the full settings, because the token is in the systemd env file and an SSH shell does not have it.

Pull mode is `spawn_poller` in `src/poll.rs`. It is not started when `poll_url` is unset. Errors back off. They do not end the process.

## File map

| Path | Role |
| --- | --- |
| `src/main.rs` | CLI, Docker connect, HTTP or rustls bind, starts background tasks. |
| `src/lib.rs` | Module list and label constants. |
| `src/config.rs` | TOML plus env. Unknown TOML fields refuse startup. `PollToken` redacts Debug. |
| `src/api.rs` | Routes, auth middleware, client IP, shared job JSON parse (`launch_request_from_slice`). |
| `src/auth.rs` | SHA-256 of the token, compared with `subtle`. Bearer parsing. |
| `src/launcher.rs` | Slot mutex, `launch` (answers, then pulls and starts in a task), reaper, purge, prune, reconcile, metrics text. |
| `src/poll.rs` | Optional outbound poll. Same launch path. |
| `src/allowlist.rs` | Image name check and repository match (`repo` allows `repo:tag` and `repo@digest`, not `repo-evil` and not `repo:5000/x`; a pinned entry is exact). |
| `src/runtime.rs` | `Runtime` trait and `StartSpec`. |
| `src/bollard_rt.rs` | Docker create/start/kill/logs/prune. Host config is built here. |
| `src/registry.rs` | Pull credentials from the machine's Docker config. The request cannot supply them. |
| `src/store.rs` | SQLite: `jobs`, `meta` (drain), `auth_failures`, `bans`. Mode `600` on unix. |
| `src/callback.rs` | URL check and the three-try POST. |
| `src/bytesize.rs` | `512m`, `1g`, or a raw byte count. |
| `src/cert.rs` | SHA-256 fingerprint of the PEM, same shape as `openssl x509 -fingerprint -sha256`. |
| `src/clock.rs` | Clock, plus a fake clock for tests. |
| `src/mock.rs` | In-memory runtime for tests. |
| `scripts/install.sh` | Ubuntu install: Docker, token, self-signed cert, systemd, optional ufw. |
| `scripts/accept.sh` | End-to-end test against a real Docker daemon. Starts its own instance. `SHUTUP_AND_RUN_SMOKE=1` then runs `smoke.sh` on it. |
| `scripts/smoke.sh` | Twelve scenarios against an instance that is already running, through the API only. |
| `examples/smoke-image/` | The Python image `smoke.sh` runs by default. `image.yml` publishes it. |
| `docs/RECIPES.md` | Copy-and-paste answers: firewall, pinning, NAT, a client's machine, stop everything. |
| `docs/REVIEW-2026-11-11.md` | The last run of the review prompt below, with issue text for what was deferred. |
| `deploy/` | Example config and the systemd unit. |
| `.github/workflows/ci.yml` | fmt, clippy, test, shellcheck, accept.sh with smoke.sh, musl static check, scratch image, install.sh from a local release with smoke.sh. |
| `.github/workflows/image.yml` | GHCR image on `main` (`:edge`) and on `v*` tags, and the smoke image. |
| `.github/workflows/release.yml` | `v*` tag attaches the musl binary and `SHA256SUMS`, then runs the one-line install and smoke.sh on a fresh runner. |

## Config

TOML from `SHUTUP_AND_RUN_CONFIG`, or `/etc/shutup-and-run/config.toml` when that is unset. Environment overrides the file. The full table is in the README. Required: token (at least 16 characters, no whitespace), `max_slots` (1 to 256), a non-empty allowlist. HTTPS is the default and then needs `tls_cert` and `tls_key`. `http = true` is plaintext.

Pull, all optional:

| toml | environment | notes |
| --- | --- | --- |
| `poll_url` | `SHUTUP_AND_RUN_POLL_URL` | Unset: no outbound poll. `http` or `https`, no userinfo. |
| `poll_interval_secs` | `SHUTUP_AND_RUN_POLL_INTERVAL_SECS` | Default 15. Range 1 to 3600. Ignored for behavior when `poll_url` is unset, but an out-of-range value still fails startup. |
| (not a toml key) | `SHUTUP_AND_RUN_POLL_TOKEN` | Sent as `Authorization: Bearer` only when `poll_url` is set. A `poll_token` key in the file is an unknown field and startup fails. When `poll_url` is unset the variable is ignored. |

The poll request, empty response, and task response are specified in the README under "Pull". Examples: `examples/kestra-poll.yml`, `examples/n8n-poll.json`. The push-side examples remain `examples/kestra-dispatch.yml` and `examples/n8n-dispatch.json`.

## Build, test, run accept.sh

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Release binary: `cargo build --release --locked`.

Static binary, which is what the release and the GHCR image ship:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools build-essential
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl
```

SQLite is bundled, so the musl build needs a C compiler. `rust-toolchain.toml` tracks stable. `Cargo.lock` is committed. CI uses `--locked`.

Acceptance, on Ubuntu with Docker and no other container labeled `shutup-and-run=1`:

```bash
sudo bash scripts/accept.sh
sudo bash scripts/accept.sh /path/to/shutup-and-run-x86_64-unknown-linux-musl
```

It installs a throwaway `shutup-and-run-accept.service` when systemd is present, and does not touch a production `shutup-and-run.service`. The expected lines are in the README. It does not set `poll_url` until the optional smoke pass, which adds it and restarts.

Smoke, against any running instance (the variables are in the header of the script and in the README):

```bash
SMOKE_URL=https://203.0.113.10:8787 SMOKE_TOKEN=... bash scripts/smoke.sh
sudo SMOKE_LOCAL=1 bash scripts/smoke.sh
```

There is no systemd in some sandboxes. `accept.sh` then runs the binary in the background, and `SHUTUP_AND_RUN_SMOKE=1` still runs all twelve scenarios.

Run the service: `shutup-and-run` or `shutup-and-run serve`. Config errors print to stderr and exit 1. `RUST_LOG` defaults to `info`.

## CI, GHCR, release

`ci.yml` runs on every push and pull request:

- `check`: `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked`, `shellcheck scripts/*.sh`.
- `accept`: release binary, then `sudo SHUTUP_AND_RUN_SMOKE=1 bash scripts/accept.sh` against the runner's Docker, which also runs `smoke.sh` with every scenario on. Timeout 30 minutes.
- `musl`: static `x86_64-unknown-linux-musl` binary, then `file` and `ldd` must show it is static. The same job builds the scratch image and runs it, serves the binary and `SHA256SUMS` on localhost as a stand-in for a release, checks that `install.sh` refuses a wrong checksum, installs from it, runs `smoke.sh` against the installed HTTPS service, and uninstalls.

`image.yml` runs on `main` and on `v*` tags. It builds the musl binary, wraps it in `Dockerfile` (`FROM scratch`), and pushes `ghcr.io/nailtongomes/shutup-and-run:edge` or `:vX.Y.Z`. Permissions: `contents: read`, `packages: write`. linux/amd64 only. The first push can create a private package; someone has to make it public if anonymous pulls should work.

`release.yml` runs on `v*` tags. Permissions: `contents: write`. It attaches `shutup-and-run-x86_64-unknown-linux-musl` and `SHA256SUMS` to the GitHub release. `scripts/install.sh` downloads that asset from `releases/latest` and refuses to install it when the hash does not match `SHA256SUMS`. `SHUTUP_AND_RUN_BIN` skips the download. A second job, `install`, waits for the assets, runs the README one-liner on a fresh runner, checks that the installed binary matches `SHA256SUMS`, and runs `smoke.sh`.

To cut the first release: merge to `main`, then `git tag v0.1.0 && git push origin v0.1.0`. Tag from `main`, because the one-liner fetches `install.sh` from `main`. `Cargo.toml` says `0.1.0`.

The workflows do not set `id-token` or a cloud deploy role. They use `GITHUB_TOKEN`.

## Decisions

Each heading in [DECISIONS.md](DECISIONS.md) is the context, the choice, and the cost. The ones that bound a review:

- [Push, no queue](DECISIONS.md#push-no-queue). `429` and `503` are the failover signals.
- [Slots come from Docker labels](DECISIONS.md#slots-come-from-docker-labels). One process mutex. Two processes can overshoot.
- [Idempotency is `job_key`, on one machine](DECISIONS.md#idempotency-is-job_key-on-one-machine). A failed pull or start keeps the key.
- [202 before the pull](DECISIONS.md#202-before-the-pull) and [No cancel call](DECISIONS.md#no-cancel-call).
- [A machine someone else lends](DECISIONS.md#a-machine-someone-else-lends).
- [Every route requires the bearer token](DECISIONS.md#every-route-requires-the-bearer-token) and [Three failures, then 48 hours](DECISIONS.md#three-failures-then-48-hours).
- [Public IP and TLS in the binary](DECISIONS.md#public-ip-and-tls-in-the-binary). Plain HTTP is opt-in.
- [No volumes, published ports, or privileged](DECISIONS.md#no-volumes-published-ports-or-privileged).
- [Pull, optional](DECISIONS.md#pull-optional). Unset means the old behavior.
- [Security is a convention](DECISIONS.md#security-is-a-convention). Do not add a sandbox in a drive-by review.
- [Windows and macOS: the same product](DECISIONS.md#windows-and-macos-the-same-product). Decided, not built.

## Known risks

These are accepted, not a bug list. A review should confirm they are still true in the code.

- The Docker socket is root on the host. The token is that power, limited by the allowlist and by the host config the code actually sets (`src/bollard_rt.rs`).
- Allowlist matching is a string match on the repository, not a signature. An entry pinned to a tag or digest is exact (`src/allowlist.rs`).
- Job containers get Docker's default capabilities, user, and bridge network. The code does not drop capabilities or set `privileged: false` explicitly; it leaves those fields at the API default and does not take them from the request.
- `memory` is optional. An omitted limit does not cap the container.
- `callback_url` is any http(s) URL the host can reach. No private-address block (`src/callback.rs`).
- `poll_url` is an outbound POST to an operator-chosen URL. Unset token means no Authorization header. A redirect is not followed.
- A task can be lost if the server deletes it when it offers it and the launcher then refuses (slot taken, not allowlisted). An accepted task keeps its `job_key` even if the image fails to pull. The README tells the server to offer the same key until the callback.
- A job answered with `202` and still pulling its image exists only in memory. A graceful stop waits 30 seconds for it. A crash, or a longer pull, ends it: the row is closed at the next start and the callback says so.
- `202` does not mean the image exists. A wrong tag is a `start_failed` job, not an error on the POST.
- `trust_x_forwarded_for` trusts the first header value. The default is off (`src/api.rs`).
- Bans key off the peer IP. A shared NAT can lock out everyone behind it. The ban is an empty `403` even with the right token.
- SQLite uses `Mutex::lock().expect`. A poisoned lock panics the calling task (`src/store.rs`).
- `scripts/install.sh` listens on `0.0.0.0` and generates a self-signed certificate. The downloaded binary is checked against `SHA256SUMS`. A binary passed in `SHUTUP_AND_RUN_BIN` is not.
- Log lines kept in SQLite can contain secrets the container printed.
- Two shutup-and-run processes on one daemon can exceed `max_slots`.
- Stopping the service does not kill containers. A timeout is SIGKILL only. While the service is stopped nothing enforces a timeout.
- A client that keeps reconnecting can still hold connections on the TLS listener. It sends no token, so it is not banned. The defense is a firewall rule ([RECIPES.md](RECIPES.md)).
- Four medium and six low findings in [REVIEW-2026-11-11.md](REVIEW-2026-11-11.md) are open. The allowlist ones are fixed.
- There is no cancel call, by decision. Ending a job early needs a shell on the machine.

## Open TODOs

- The Windows and macOS client in [ROADMAP-windows-macos.md](ROADMAP-windows-macos.md). No code. The open questions there are undecided. The integrity check (sha256 or signature) is required before that client runs a downloaded script.
- GHCR: if a package was created private, the owner still has to switch it to public for anonymous pulls. That applies to `shutup-and-run` and to `shutup-and-run-smoke`. It is a registry setting, not a code change.

## Answered by the owner on 2026-11-11

1. Allowlist match: fixed.
2. Slow answers while pulling: `202` now comes before the pull. See DECISIONS.
3. Cancel: no.
4. Installer without an allowlist: it installs and warns.
5. Smoke image: `hello-world`, or the Python image in `examples/smoke-image`.
6. Windows and macOS: the same binary, with a script mode on Windows only. See DECISIONS and the roadmap.
7. Lent machines: CONVENTIONS now covers them.

## Open questions for the owner

1. Script mode on Windows. The roadmap requires a sha256 or a signature before a downloaded script runs. Where does the hash come from: the task JSON, a file beside the script, or a signature with a key you hold? Nothing should be built before this is answered.
2. macOS. Your answer mentioned running a script on Windows or macOS and then said script mode is Windows only. It is recorded as Windows only, with containers on macOS. Say so if macOS should run scripts too.
3. The script-mode allowlist. Containers are limited to allowlisted images. What limits scripts: a list of URL prefixes in the config?
4. The smoke image is pushed to GHCR by `image.yml` on `main`. The first push may create a private package. Make `shutup-and-run-smoke` public, or machines without a login will only run it from a local build.
5. The four medium findings still open in the review (connection cap, log buffer, event reconnect, CI token). Fix them next?

Do not add a queue, a web UI, a second database, cross-machine coordination, or automatic restarts of failed containers. [CONTRIBUTING.md](../CONTRIBUTING.md) says so.

## Review prompt

Paste the block below. It is the assignment. Do not add findings from this handoff that you have not opened in the tree.

```text
You are reviewing the public repository shutup-and-run, a single Rust binary that starts allowlisted Docker containers on one machine. Read README.md, SECURITY.md, docs/CONVENTIONS.md, docs/DECISIONS.md, and the code under src/, scripts/install.sh, and .github/workflows/. Do not propose new features. docs/ROADMAP-windows-macos.md is documentation of a future client; do not treat missing Windows or macOS code as a defect.

Audit only what you can point at. If you cannot cite a file and line, do not report it. Do not invent a finding to fill the table. An empty table is a valid result. The "Known risks" section of docs/HANDOFF.md is the owner's accepted list: confirm each item against the code or drop it. Do not copy it in as a new finding unless the code is worse than that section says.

Look at these areas:

- Auth and bans: bearer check, constant-time compare, which routes are covered, ban persistence, empty 403, X-Forwarded-For, whether a banned IP is checked before the token.
- Allowlist bypass: repository-neighbor names, tag versus digest, uppercase, odd punctuation, config parsing of the allowlist.
- Privilege flags: container create in src/bollard_rt.rs. Can a JSON field set privileged, binds, devices, pid, network mode, capabilities, or ports? What do Docker defaults leave on?
- TLS: plaintext opt-in, certificate load, fingerprint, what install.sh generates.
- Callback SSRF: scheme check, redirects, what addresses a token holder can make the host call. Same question for poll_url, which is operator config.
- install.sh: permissions on the token and key, bind address, whether the downloaded binary is checked, what --uninstall deletes.
- Resource exhaustion: body size, slot ceiling, unbounded memory when the task omits memory, log retention, poll response size, Docker log growth (the README already names daemon log rotation).
- CI permissions: contents and packages scopes on ci.yml, image.yml, and release.yml. Say if a job's token can do more than that job needs.
- Panics: unwrap, expect, and lock poisoning on the request path and in the poll and reaper tasks. Tests do not count.

Output a Markdown table with these columns only:

| Severity | Location | Finding | Minimal fix |

Severity is one of critical, high, medium, low. Location is file:line (a range is fine) for a line you opened. Finding is one or two sentences. Minimal fix is the smallest change that addresses it, or "accept and document" when the code matches an explicit decision in docs/DECISIONS.md or docs/CONVENTIONS.md. No redesigns. No patches for style.
```

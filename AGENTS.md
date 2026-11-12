# Agents

Notes for an AI coding agent in this repository. Decisions live in [docs/DECISIONS.md](docs/DECISIONS.md). What the operator agrees to lives in [docs/CONVENTIONS.md](docs/CONVENTIONS.md). Behavior for callers is in [README.md](README.md).

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
shellcheck scripts/*.sh
```

`scripts/install.ps1` and `scripts/shutup-and-run.bat` have no linter in CI. The `.bat` is ASCII with CRLF line ends (`.gitattributes`). Keep it short and keep its checksum logic the same shape as `install.sh`.

The Windows `.exe` (`src/wsl.rs`, the WSL2 supervisor) is built on `windows-latest` in CI with the MSVC toolchain. From Linux it cross-compiles for a check, with `rustup target add x86_64-pc-windows-gnu` and `apt-get install gcc-mingw-w64-x86-64`:

```bash
cargo clippy --all-targets --locked --target x86_64-pc-windows-gnu -- -D warnings
cargo build --release --locked --target x86_64-pc-windows-gnu
```

The parts of `src/wsl.rs` that shell out to `wsl.exe` and `schtasks.exe` can only run on Windows 11 with WSL2. Their pure parts (flag parsing, `wsl --list` decoding, the rendered config, checksum matching) are unit tested everywhere. `tests/poll-mock/poll_mock.py` is a stand-in poll URL for tests, and `tests/poll-mock/cycle.sh` runs it against a worker in CI. `tests/windows-sandbox/run.ps1` is a hand test in Windows Sandbox, as a user who is not an administrator, of `install.ps1` and the logon task; it cannot run WSL2. `shutup-and-run wsl doctor` on a machine without WSL must stop at its first line and exit 1; CI checks that on a Windows runner.

`docs/INTEGRATION-CONTRACT.md` and `docs/schemas/*.schema.json` are the public contract. `src/contract.rs` holds them to the code: it sends every file in `docs/schemas/examples` through the real router, poll parser and callback, and checks what the runner emits against the schemas. A change to a request field, an answer body, a status code or a poll answer form changes the schema, an example and the document in the same patch. `jsonschema` is a development dependency only. `examples/n8n/*.json` were checked by hand against a local n8n; CI does not run n8n.

The release binary is static musl. SQLite is bundled, so the link needs a C compiler and `musl-gcc`:

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install -y musl-tools build-essential
CC_x86_64_unknown_linux_musl=musl-gcc cargo build --release --locked --target x86_64-unknown-linux-musl
```

The GitHub-hosted image already has shellcheck. CI installs `musl-tools` with `scripts/ci-apt.sh`. Each `apt-get` is capped. If it fails, the script leaves `azure.archive.ubuntu.com` and retries against `archive.ubuntu.com`, then `mirrors.kernel.org`.

## Accept and smoke

Needs Docker, and no other container labeled `shutup-and-run=1`.

```bash
cargo build --release --locked
sudo bash scripts/accept.sh target/release/shutup-and-run
```

`SHUTUP_AND_RUN_SMOKE=1` runs all of `scripts/smoke.sh` on that instance. `accept.sh` starts its own process. It does not touch a production `shutup-and-run.service`. Without systemd it runs the binary in the background.

Against an instance that is already up:

```bash
SMOKE_URL=https://127.0.0.1:8787 SMOKE_TOKEN=... bash scripts/smoke.sh
sudo SMOKE_LOCAL=1 bash scripts/smoke.sh
```

The scratch image is the Dockerfile with the musl binary copied to `./shutup-and-run` first. `scripts/size.sh` measures the launcher's RAM and CPU. It is not part of CI. The numbers and the reservation to make for the process are in [docs/SIZING.md](docs/SIZING.md).

## Keep it this shape

- One machine, one bearer token. An allowlist entry is one repository. `repo` allows `repo:tag` and `repo@digest`. It does not allow `repo-evil`, a wildcard, or someone else's image. Do not loosen that match. The one way around the list is `poll_any_image`, which applies to a polled task only, never to a push, and needs `poll_url`. Do not widen it.
- A request sets image, env, command, timeout, cpu, memory, and shm. It does not set mounts, ports, privileged, capabilities, or a network mode. Do not add those fields. A manifest is that same request, fetched once from `manifest_url`; it cannot point at another manifest.
- The poll body is who is asking and what it could lend: `worker`, `os`, `arch`, `version`, `max_slots`, `free_slots`, `cpus`, `memory_bytes`. Do not put the machine's config, tokens, job history, or a self-reported address in it.
- A Linux machine is the owner's and trusted. A Windows machine is a client's PC and is not: assume its user can read the poll token and every task's env. The poll URL decides which machines get work.
- No `schema_version` and no register call. A request is not versioned; the poll body's `version` is how a server tells releases apart. Enrollment is the first poll with a token the server has not seen.
- Unknown JSON fields and unknown TOML fields stay rejected. `poll_token` is not a TOML key. `SHUTUP_AND_RUN_POLL_TOKEN` is environment only.
- On Windows a job is a container inside the `shutup-and-run` WSL2 distribution. The `.exe` is a supervisor; it does not talk to Docker and does not run scripts. No Docker Desktop path, no script mode.
- Every route needs the bearer token, except the local status page (`status_page = true`: `GET /`, `/favicon.ico`, `POST /local/pause|resume`), which answers only a loopback peer naming a loopback host. Do not add anything to that page that is not already visible to the person at the machine, and nothing else without the token.
- The Windows `.exe` is one console-subsystem program with a static C runtime. The logon task runs it under `conhost.exe --headless`, and every child process is created with `CREATE_NO_WINDOW`. The worker must never show a window.
- No queue, no scheduler, no web UI beyond that one status page, no second database, no cross-machine coordination, and no automatic restart of a container that exits.
- No secrets, tokens, or credentials in the repo, in examples, or in logs. Do not print `SHUTUP_AND_RUN_TOKEN`.
- Prefer a smaller patch. A shortcut in code is marked `// shortcut: <limit>, <when to upgrade>`.
- A choice a later reader would second-guess goes in `docs/DECISIONS.md`: context, the decision, and what it costs.
- A release is a `v*` tag pushed from `main`. Do not create or push a tag unless the owner asked. `install.sh` on `main` is what that tag's one-line install fetches.

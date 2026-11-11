# Agents

Notes for an AI coding agent in this repository. Decisions live in [docs/DECISIONS.md](docs/DECISIONS.md). What the operator agrees to lives in [docs/CONVENTIONS.md](docs/CONVENTIONS.md). Behavior for callers is in [README.md](README.md).

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
shellcheck scripts/*.sh
```

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

- One machine, one bearer token. An allowlist entry is one repository. `repo` allows `repo:tag` and `repo@digest`. It does not allow `repo-evil`, a wildcard, or someone else's image. Do not loosen that match.
- A request sets image, env, command, timeout, cpu, memory, and shm. It does not set mounts, ports, privileged, capabilities, or a network mode. Do not add those fields.
- Unknown JSON fields and unknown TOML fields stay rejected. `poll_token` is not a TOML key. `SHUTUP_AND_RUN_POLL_TOKEN` is environment only.
- No queue, no scheduler, no web UI, no second database, no cross-machine coordination, and no automatic restart of a container that exits.
- No secrets, tokens, or credentials in the repo, in examples, or in logs. Do not print `SHUTUP_AND_RUN_TOKEN`.
- Prefer a smaller patch. A shortcut in code is marked `// shortcut: <limit>, <when to upgrade>`.
- A choice a later reader would second-guess goes in `docs/DECISIONS.md`: context, the decision, and what it costs.
- A release is a `v*` tag pushed from `main`. Do not create or push a tag unless the owner asked. `install.sh` on `main` is what that tag's one-line install fetches.

# Contributing

shutup-and-run stays a single static binary: one machine, a slot limit, no queue, no scheduler, and no UI.

- Run `cargo test` and `cargo fmt` before sending a change. Agents: read [AGENTS.md](AGENTS.md) first.
- Prefer a smaller patch. If a behavior needs a shortcut, mark it in the code as `// shortcut: <limit>, <when to upgrade>`.
- Do not add a web UI, a database besides the one SQLite history file, cross-machine coordination, or automatic restarts of failed containers.
- Keep examples generic. No private hostnames, addresses, or image names.
- The HTTP API is root-equivalent. See `SECURITY.md`.

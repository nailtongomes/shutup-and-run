# Security

shutup-and-run talks to the Docker socket. A valid token can start any allowlisted image with any environment variables. Treat the token as a root password. There is one token and one caller. When the machine belongs to someone else, [docs/CONVENTIONS.md](docs/CONVENTIONS.md) says what each side accepts.

What the operator must do, and which defaults a request cannot turn off, is [docs/CONVENTIONS.md](docs/CONVENTIONS.md). This file is how the process behaves. That file is what you accept by running it. The process does not add checks for rules a person can follow.

## Every route needs the token

`/health` and `/metrics` included. A wrong or missing token is `401`. Nothing is served anonymously.

## Public address, native TLS

The intended install listens on a public IP and port and terminates HTTPS in the binary (rustls). The installer writes a self-signed certificate and prints its SHA-256 fingerprint. Pin that fingerprint, or point clients at the cert file. Kestra can set `options.ssl.insecureTrustAllCertificates: true`. n8n can set `allowUnauthorizedCerts: true`. Check the fingerprint out of band before you do either.

The HTTPS listener speaks HTTP/1.1 only. A connection that does not send a request within 10 seconds is closed, and so is a kept-alive connection that does not send its next request headers within 10 seconds. That bounds how long a silent client can hold a file descriptor. It does not stop a client that keeps reconnecting, and such a client is never banned because it never sends a token. If that matters, allow only your orchestrator's address in the firewall ([docs/RECIPES.md](docs/RECIPES.md)).

Plain HTTP exists only when `http = true`. That is for localhost or a network you already trust. The process warns if plaintext is bound to every interface, because the token would travel in clear text.

## Bans

Three failed authentications from the same TCP peer (defaults, both configurable) ban that IP for 48 hours. The ban is a row in SQLite, so it survives a restart. A banned IP gets `403` with an empty body, even with the right token. The peer address is the real socket address. `X-Forwarded-For` is ignored unless `trust_x_forwarded_for = true`, which you should set only behind a proxy you control.

From the machine, over SSH:

```bash
sudo shutup-and-run bans
sudo shutup-and-run bans lift 203.0.113.5
```

These read only `history_path` from the config, so they work in a shell that does not have the token.

Bans are logged.

A correct token clears the failure counter for that IP, so two typos do not linger until the third.

## Files

The token is never hard-coded. Comparison is a SHA-256 digest checked with `subtle`. The installer stores it in `/etc/shutup-and-run/env`, mode `600`, and does not print it unless `SHUTUP_AND_RUN_SHOW_TOKEN=1`. The SQLite file is mode `600`. It holds job keys, callback URLs, log lines, and bans. Log lines can contain secrets the container printed.

## Callbacks

`callback_url` is called from this machine when a job ends, with three tries and no redirects. A token holder can point it at any http(s) URL the host can reach, including link-local addresses.

## Registry and containers

Image pulls use the machine's `docker login`. The request cannot supply registry credentials, volumes, published ports, or privileged mode.

## Pull

`poll_url` makes this process call out. You choose that URL. The bearer token is `SHUTUP_AND_RUN_POLL_TOKEN` in the environment, not in the TOML file. When the variable is unset the request has no Authorization header. Redirects are not followed. The contract is in the README.

The poll body carries `worker_name`, `worker_os` and the free slot count, nothing else. A `manifest_url` in the answer is fetched once, with the same size cap and no redirects, and the poll token is sent with it only when the manifest is on the same scheme, host and port as `poll_url`.

`poll_any_image = true` lets a polled task name any image. It never applies to a pushed job, it still validates the image name, and it needs `poll_url`. The Windows worker is installed with it on, because the orchestrator is that machine's only source of work.

## Windows

The `.exe` is a supervisor for a WSL2 distribution it creates. The service, Docker Engine and the containers live inside that distribution. It mounts no Windows drive and cannot launch Windows programs. Both tokens are written only to `/etc/shutup-and-run/env` inside it, mode 600, and the API listens on `127.0.0.1` inside the distribution only. Downloads (the rootfs, the Linux binary, the `.exe` itself through `install.ps1`) are checked against the publisher's `SHA256SUMS` before use. The `.exe` is not code signed. `wsl stop` terminates the distribution, which kills every container in it.

## Reporting

Open a private security report with the repository host if you can. Otherwise open an issue without exploit detail and ask for a private channel.

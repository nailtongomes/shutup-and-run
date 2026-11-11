# Security

shutup-and-run talks to the Docker socket. A valid token can start any allowlisted image with any environment variables. Treat the token as a root password. There is one owner.

What the operator must do, and which defaults a request cannot turn off, is [docs/CONVENTIONS.md](docs/CONVENTIONS.md). This file is how the process behaves. That file is what you accept by running it. The process does not add checks for rules a person can follow.

## Every route needs the token

`/health` and `/metrics` included. A wrong or missing token is `401`. Nothing is served anonymously.

## Public address, native TLS

The intended install listens on a public IP and port and terminates HTTPS in the binary (rustls). The installer writes a self-signed certificate and prints its SHA-256 fingerprint. Pin that fingerprint, or point clients at the cert file. Kestra can set `options.ssl.insecureTrustAllCertificates: true`. n8n can set `allowUnauthorizedCerts: true`. Check the fingerprint out of band before you do either.

Plain HTTP exists only when `http = true`. That is for localhost or a network you already trust. The process warns if plaintext is bound to every interface, because the token would travel in clear text.

## Bans

Three failed authentications from the same TCP peer (defaults, both configurable) ban that IP for 48 hours. The ban is a row in SQLite, so it survives a restart. A banned IP gets `403` with an empty body, even with the right token. The peer address is the real socket address. `X-Forwarded-For` is ignored unless `trust_x_forwarded_for = true`, which you should set only behind a proxy you control.

From the machine, over SSH:

```bash
shutup-and-run bans
shutup-and-run bans lift 203.0.113.5
```

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

## Reporting

Open a private security report with the repository host if you can. Otherwise open an issue without exploit detail and ask for a private channel.

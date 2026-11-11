# Security

Fairlead talks to the Docker socket. A valid token can start any allowlisted image with any environment variables. Treat the token like a root password.

## Bind a private address

Set `listen` to a private interface or a Tailscale address. The default is `127.0.0.1:8787`. Do not use `0.0.0.0` or `[::]`. The process logs a warning if you do, and it still starts: the config is yours to get right.

`/health` and `/metrics` do not require the token. They only expose slot counts and counters. They are still not for a public interface.

## Token and files

The token is never hard-coded. Comparison is a SHA-256 digest checked with `subtle` (constant time). Put it in `FAIRLEAD_TOKEN` or in the config file, mode `600`. The SQLite file is created mode `600`. It holds image names, job keys, exit codes, callback URLs, and the last log lines, which may contain secrets the container printed.

## Callbacks

`callback_url` is called from this machine when a job ends, with a small retry and no redirects. A token holder can point it at any http(s) URL the host can reach, including link-local and cloud metadata addresses. Only give the token to callers you trust with that.

## Registry

Image pulls use the machine's existing `docker login`. The request body cannot supply registry credentials.

## Reporting

Open a private security report with the repository host if you can. Otherwise open an issue without exploit detail and ask for a private channel.

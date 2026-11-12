# Windows and macOS client

This describes how shutup-and-run runs on a person's computer.

## Built on 2026-11-12: Windows, as containers inside WSL2

Windows 11 is done and documented in [WINDOWS.md](WINDOWS.md). It is the same Rust binary: `shutup-and-run.exe wsl install` imports a WSL2 distribution, puts Docker Engine and the Linux service inside it, and the service polls in the same contract as a Linux machine. There is no script mode in the binary. A PC that cannot run WSL2 can use [`scripts/windows/sar-launcher.bat`](../scripts/windows/sar-launcher.bat) instead. That file is the bootstrap described below. The binary does not download or run it, and it still has no integrity check, so it is not the deferred script mode. The decisions are in [DECISIONS.md](DECISIONS.md#windows-is-the-linux-service-inside-wsl2).

That answers, for Windows: 1 (a binary), 2 (WSL is required; there is no script path), 3 (poll only), 4 (the bearer poll token, plus `worker_name` in the body), 10 (the same `job_key` rule as Linux, inside the distribution), 12 (not signed; `SHA256SUMS` is the integrity check), 13 (this repository), 17 (the poll URL is the only base; a `manifest_url` is one hop from it, with the token only on the same origin), 18 (a task is a container, so Python is whatever the image has).

Script mode, the rest of this file, is deferred. It is not deleted because the integrity requirement below is the reason it is not built, and the next person should see that. The remaining open questions are the script ones (5 to 9, 11, 16) and macOS (14, 15).

## Decided on 2026-11-11

- It is the same product and the same Rust binary, not a separate client. It accepts pushed jobs or polls for them, like the Linux service.
- On macOS a job is a container.
- On Windows a job may be a script instead of a container. The binary downloads the script from a URL and runs it with no WSL and no Docker around it. The owner of the machine accepts that loss of isolation in order to run the automation. Script mode is for Windows only.

That answers questions 1 (a binary), 3 (both), and 13 (this repository) in the list at the end, and part of 2 (the script path does not require WSL). The rest of this file is the earlier description. Where it disagrees with the three points above, the points above win. The other open questions are still open, and the integrity requirement below still stands.

## What it would do

A small program the person installs. It waits for work, or polls for work, and then either starts a container or, on Windows, runs a script fetched from a URL. Installing it is the person agreeing that the program may execute Python scripts on that machine.

Ideal isolation on Windows is WSL, so the container path matches the Linux service. The simplest path does not require Docker. It is [`scripts/windows/sar-launcher.bat`](../scripts/windows/sar-launcher.bat), whose behavior is listed below. That file is the behavior to keep, plus an integrity check it does not have.

## The bootstrap to mirror

Configuration variables live at the top of the script.

Each task takes a lock file. If a live process holds the lock, the script exits 2. If the lock is older than an orphan TTL, it is treated as abandoned and taken.

Python is detected as `py -3`, then `python`.

Helper files are downloaded from a base URL. A helper that fails to download is non-fatal. The main script is fatal when it cannot be downloaded and there is no local copy.

Downloads use a cache TTL. Retries use backoff. Each download is written to a `.tmp` file beside the destination, validated, then moved into place. Validation today is: the file is non-empty, and a `.py` file must not be HTML (a proxy error or a login page saved as the script).

When the machine is offline, the last local copy is used.

The child process receives the run mode and the task id as environment variables.

The process exit code is propagated: 0 ok, 1 error, 2 lock held.

## Integrity is required

That bootstrap does not check that the downloaded bytes are the bytes that were published. The client must not ship that way. Every downloaded script needs a sha256, or a signature, checked before the file replaces the local copy and before it runs. A mismatch is fatal for the main script. How the hash is published, and whether a previously verified local copy may be used while offline, are open questions. The requirement is not: a script that fails the check must not run.

## macOS

A launchd agent would start the same kind of small binary at login and keep it running. Containers would go through Docker Desktop or colima. There is no `.bat` to mirror. Fetching a Python script from a URL and running it is the Windows fallback, not the macOS plan, unless an open question below is answered that way.

## Open questions

1. Is the Windows deliverable a small binary, or only a script?
2. Is WSL required, with the script path only when WSL and Docker are absent, or is the script path the default?
3. Does the client poll, listen, or both? The Linux service can poll. Whether the client uses that same contract is undecided.
4. How does the client authenticate? A header token, a key in the URL, or something else?
5. Where does the sha256 live: a sibling file, a manifest, or a signature? Who holds the signing key?
6. Are helper files in the integrity check, or only the main script?
7. What are the helper files for? The bootstrap downloads some. Their roles are not specified here.
8. Lock-file directory, orphan TTL, cache TTL, retry count, and backoff schedule.
9. The environment variable names for run mode and task id.
10. How a task is acknowledged so two polls do not run it twice. The Linux service uses `job_key` on one machine. A client that can be offline needs its own rule.
11. What happens to a task when the script exits 2 because the lock is held? Retry, skip, or leave it?
12. Code signing for a Windows binary, and notarization for a macOS binary.
13. Does the client live in this repository or a separate one?
14. On macOS, is launchd enough, or is a GUI expected?
15. Docker Desktop or colima as the documented macOS default.
16. When the hash cannot be downloaded, is the offline fallback forbidden, or may a copy that was checked earlier still run?
17. Should the client refuse any base URL host other than the one in its config?
18. Is Python required on Windows, or can a task be a binary with no Python?

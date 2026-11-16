# Poll mock

A small stand-in for the poll URL, to prove pull mode with real work and to find how much a machine takes. One Python file, standard library only, runs on Windows or Linux. Test only: plain HTTP, and it serves any token unless you pass `--require-approval`.

## The contract it speaks

It is the worker's existing poll contract. Nothing was added to the worker for this.

| | |
| --- | --- |
| `POST /poll` | The worker asks, with `Authorization: Bearer <token>` and `{"worker","os","arch","version","max_slots","free_slots","cpus","memory_bytes"}`. The answer is `{"task": {...}}` or `204`. One task per poll. |
| the task | The same JSON as `POST /v1/jobs`: `image`, `env`, `command`, `timeout_secs`, `cpu`, `memory`, `shm`, `job_key`, `callback_url`. |
| `POST /callback` | The worker reports the end of a job: `id`, `job_key`, `image`, `status`, `exit_code`, `reason` (`exited`, `timeout_kill`, `start_failed`, `interrupted`), `started_at`, `ended_at`, `duration_secs`. |

**What the contract cannot return.** The callback carries the exit code and the reason. It does not carry the job's output or any file, and a task cannot mount a volume, by design. The last log lines exist only on the worker, behind its own token (`GET /v1/jobs/{id}?logs=true`).

So a result that is a file is sent **by the container itself**, to a URL it is given in its `env`. That is how the product is meant to be used ("the container writes its own result wherever it already does"), and it needs no change. The mock gives every task `PROBE_UPLOAD_URL=http://<mock>/upload/<job_key>` and stores what arrives.

**Proposed, not implemented:** if you want a failed job's output at the orchestrator without asking the machine, the smallest addition would be an optional `logs` field in the callback body, holding the same clipped tail the worker already stores (`log_lines`). It would send container output off the machine, so it should be opt-in per task. Say so and it is a small PR.

## One task, from the poll to its callback

One command, on a Linux machine with Docker. It starts the mock and a worker of its own (a temporary config and history, no token of yours, nothing installed is touched), and removes both when it ends:

```
cargo build --release --locked
sudo bash tests/poll-mock/cycle.sh target/release/shutup-and-run one
```

The mock offers one `hello-world` task with `job_key` `task-1042-attempt-1`, the key an API would derive from its task 1042 ([contract, section 6](../../docs/INTEGRATION-CONTRACT.md#6-idempotency-and-delivery)). The run passes only when:

- the callback with that `job_key` arrives and says `status` `exited`, `exit_code` 0;
- the same task, offered a second time, starts nothing: no second job id calls back, and the worker logs `poll task already recorded`.

It takes about a minute and a half, because the worker polls every 30 seconds and that is not shortened for a test. The mock gives up after 240 seconds. It fails, with the reason on one `FAIL:` line, when:

| | |
| --- | --- |
| no callback came | `FAIL: no callback for job_key task-1042-attempt-1 within 240 s` |
| a callback came for another key | `FAIL: a callback arrived with job_key 'task-9999-attempt-1', and this run handed out task-1042-attempt-1` |
| the task ran twice | `FAIL: job_key task-1042-attempt-1 ran twice: callbacks from job <id> and job <id>` |

The script provokes the first two itself before the real run, so a mock that stopped noticing them fails too.

The same check against a worker that is already installed, by hand: `python3 tests/poll-mock/poll_mock.py --job-key task-1042-attempt-1 --deadline 240 --redeliver`, with the worker's `poll_url` pointing at the mock. A `job_key` is remembered by the worker, so a second run against the same worker needs a new key (`task-1042-attempt-2`): the old one would start nothing and call back nothing.

## Run it

On the machine that plays the orchestrator (your Windows host or a Linux box):

```
python tests/poll-mock/poll_mock.py --tasks 1
```

On Windows, allow Python through the firewall when asked (private network), and find the host's LAN address with `ipconfig`. Use that address, **not `127.0.0.1`**: the worker and its containers both have to reach it, and the mock puts the address the worker used into `callback_url` and `PROBE_UPLOAD_URL`.

Point the worker at it:

```powershell
shutup-and-run wsl install --poll-url http://192.168.0.50:8080/poll --max-slots 2 --poll-interval-secs 30
```

The mock prints `WORKER` (who asked, with its CPUs and RAM), `TASK`, `FILE`, then `ok` or `FAIL` per job and a `SUMMARY`, and exits. `C:\ProgramData\shutup-and-run\status.json` says `active` while a job runs and `idle` after; `http://127.0.0.1:8787/` shows the same.

Results are in `poll-mock-results/` (or `--out`): `summary.txt`, `events.jsonl` (every poll answer, callback and upload), and one `<job_key>.png` per upload.

## The heavy job: the Selenium probe

`ghcr.io/nailtongomes/shutup-and-run-selenium-probe` ([examples/selenium-probe](../../examples/selenium-probe)) is SeleniumBase with headless Chrome. It opens `https://github.com/nailtongomes/shutup-and-run`, takes a screenshot, posts it to `PROBE_UPLOAD_URL`, and exits 0 only if the page loaded and the upload worked. It holds no secret and no client data. The image is about 1.5 GB, so the first job on a machine is mostly the download.

```
python tests/poll-mock/poll_mock.py --image ghcr.io/nailtongomes/shutup-and-run-selenium-probe --timeout 180 --shm 1g
```

**The screenshot** is `poll-mock-results\<job_key>.png` on the machine running the mock. Open it.

The package is published by the `image` workflow on a push to `main`. The first time, GitHub may create it private; make it public once in the package's settings, or the worker cannot pull it.

## Load: find the machine's limit

Queue N equal tasks; a worker installed with `--max-slots K` runs K at a time.

```
python tests/poll-mock/poll_mock.py --tasks 20 --image ghcr.io/nailtongomes/shutup-and-run-selenium-probe --timeout 180 --shm 1g --memory 2g
```

```text
SUMMARY
  tasks            20  (ghcr.io/nailtongomes/shutup-and-run-selenium-probe)
  ok               19
  failed           1  {'timeout_kill': 1}
  at once (peak)   4
  wall time        212s  (5.7 tasks/min)
  job duration     avg 38.2s  min 21s  max 180s
  RAM per job      peak 912 MiB  avg 760 MiB  (x4 at once = about 3.6 GiB)
  CPU per job      avg 14.3 cpu-seconds
  files received   19  in ...
  worker           DESK-07  windows  v0.2.4  8 cpus  7.7 GiB  max_slots 4
```

(Those numbers are an illustration of the format, not a measurement.)

Raise `--max-slots` (run `wsl install` again with the new value) and repeat until the durations climb or jobs start failing. `RAM per job` and `CPU per job` are what each container's own cgroup recorded, reported by the probe with its upload; they are per job, not the whole machine, and only the probe image sends them. The `worker` line is what the machine said about itself in the poll.

`--hang 1` makes one task hang (`PROBE_HANG=1`): the worker kills it at `--timeout`, the summary counts it as `timeout_kill`, and the other tasks still run. `--env PROBE_FAIL=1` makes every task exit 3. `--expect-failures N` makes the mock exit 0 when exactly N failed. `--deadline SECS` makes it exit 1 when a callback is still missing after that long, or when one arrives for a `job_key` it did not hand out; without it the mock waits.

## Enrollment

`--require-approval` serves only a token whose SHA-256 (or its first 12 characters) is in `approved.txt`. An unknown one is printed once as `PENDING`, with the line that approves it, and gets `403`.

## In CI

`tests/poll-mock/cycle.sh` runs on Linux with Docker: the mock, a worker, three `alpine` tasks of which one hangs, and checks that `status.json` went `active` and back to `idle`, that the hung task was a `timeout_kill`, and that the worker lived. A second run builds the probe image and requires its screenshot, which CI keeps as the artifact `probe-screenshot`. A third is the one task above (`one`).

```
sudo bash tests/poll-mock/cycle.sh target/release/shutup-and-run
```

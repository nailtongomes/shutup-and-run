# Poll mock

A small stand-in for the poll URL, to prove pull mode with real work and to find how much a machine takes. One Python file, standard library only, runs on Windows or Linux. Test only: plain HTTP, and it serves any token unless you pass `--require-approval`.

## The contract it speaks

It is the worker's existing poll contract. Nothing was added to the worker for this.

| | |
| --- | --- |
| `POST /poll` | The worker asks, with `Authorization: Bearer <token>` and `{"worker","os","arch","version","max_slots","free_slots","cpus","memory_bytes"}`. The answer is `{"task": {...}}` or `204`. One task per poll. |
| the task | The same JSON as `POST /v1/jobs`: `image`, `env`, `command`, `timeout_secs`, `cpu`, `memory`, `shm`, `job_key`, `callback_url`. |
| `POST /callback` | The worker reports the end of a job: `id`, `job_key`, `image`, `status`, `exit_code`, `reason` (`exited`, `timeout_kill`, `start_failed`), `started_at`, `ended_at`, `duration_secs`. |

**What the contract cannot return.** The callback carries the exit code and the reason. It does not carry the job's output or any file, and a task cannot mount a volume, by design. The last log lines exist only on the worker, behind its own token (`GET /v1/jobs/{id}?logs=true`).

So a result that is a file is sent **by the container itself**, to a URL it is given in its `env`. That is how the product is meant to be used ("the container writes its own result wherever it already does"), and it needs no change. The mock gives every task `PROBE_UPLOAD_URL=http://<mock>/upload/<job_key>` and stores what arrives.

**Proposed, not implemented:** if you want a failed job's output at the orchestrator without asking the machine, the smallest addition would be an optional `logs` field in the callback body, holding the same clipped tail the worker already stores (`log_lines`). It would send container output off the machine, so it should be opt-in per task. Say so and it is a small PR.

## Run it

On the machine that plays the orchestrator (your Windows host or a Linux box):

```
python tests/poll-mock/poll_mock.py --tasks 1
```

On Windows, allow Python through the firewall when asked (private network), and find the host's LAN address with `ipconfig`. Use that address, **not `127.0.0.1`**: the worker and its containers both have to reach it, and the mock puts the address the worker used into `callback_url` and `PROBE_UPLOAD_URL`.

Point the worker at it:

```powershell
shutup-and-run wsl install --poll-url http://192.168.0.50:8080/poll --max-slots 2 --poll-interval-secs 2
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
  worker           DESK-07  windows  v0.2.3  8 cpus  7.7 GiB  max_slots 4
```

(Those numbers are an illustration of the format, not a measurement.)

Raise `--max-slots` (run `wsl install` again with the new value) and repeat until the durations climb or jobs start failing. `RAM per job` and `CPU per job` are what each container's own cgroup recorded, reported by the probe with its upload; they are per job, not the whole machine, and only the probe image sends them. The `worker` line is what the machine said about itself in the poll.

`--hang 1` makes one task hang (`PROBE_HANG=1`): the worker kills it at `--timeout`, the summary counts it as `timeout_kill`, and the other tasks still run. `--env PROBE_FAIL=1` makes every task exit 3. `--expect-failures N` makes the mock exit 0 when exactly N failed.

## Enrollment

`--require-approval` serves only a token whose SHA-256 (or its first 12 characters) is in `approved.txt`. An unknown one is printed once as `PENDING`, with the line that approves it, and gets `403`.

## In CI

`tests/poll-mock/cycle.sh` runs on Linux with Docker: the mock, a worker, three `alpine` tasks of which one hangs, and checks that `status.json` went `active` and back to `idle`, that the hung task was a `timeout_kill`, and that the worker lived. A second run builds the probe image and requires its screenshot, which CI keeps as the artifact `probe-screenshot`.

```
sudo bash tests/poll-mock/cycle.sh target/release/shutup-and-run
```

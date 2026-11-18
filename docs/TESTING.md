# Testing a release by hand

Português: [TESTING.pt-BR.md](TESTING.pt-BR.md).

One Linux VPS (yours, trusted) and one Windows 11 PC (treated as a client's). About 30 minutes. Each step has the command, then what to expect. If a step does not match, stop there and keep the output.

## 0. The release exists

The one-line installs download from the latest GitHub release.

- [ ] `https://github.com/nailtongomes/shutup-and-run/releases/latest` is the version in `Cargo.toml` (`v0.2.14` when this was written) or later and has three files: `shutup-and-run-x86_64-unknown-linux-musl`, `shutup-and-run-x86_64-pc-windows-msvc.exe`, `SHA256SUMS`.

If it is not there: the `release` workflow runs by itself when `main` gets a `Cargo.toml` version that has no release, creates the tag and publishes. Look at its last run under Actions; "Run workflow" on `main` retries one that failed. Nobody pushes a tag by hand.

## 1. Linux VPS: push

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo SHUTUP_AND_RUN_ALLOWLIST=ghcr.io/nailtongomes/shutup-and-run-smoke bash
```

- [ ] It prints the URL and the certificate fingerprint, not the token.

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/smoke.sh | sudo SMOKE_LOCAL=1 bash
```

- [ ] Lines are `PASS:` or `SKIP:` with a reason, none is `FAIL:`, and the last line is `RESULT: N passed, 0 failed, ...`. This covers `202`, `429` when full, the timeout kill, `job_key`, and drain.

One instance:

```bash
sudo SHUTUP_AND_RUN_CONFIG=/etc/shutup-and-run/config.toml SHUTUP_AND_RUN_TOKEN=x /usr/local/bin/shutup-and-run serve; echo "exit $?"
```

- [ ] `another shutup-and-run is already running (...jobs.lock is locked)` and `exit 1`. The service is untouched: `systemctl is-active shutup-and-run` says `active`.

The status file, on the VPS:

```sh
sudo sed -n 's/.*"state":"\([^"]*\)".*/\1/p' /var/lib/shutup-and-run/status.json
```

- [ ] Prints `idle` (or `active` while a smoke job is still running). `updated_at` in that file is within the last minute.

## 2. A poll URL to test with

On the VPS (or any machine the Windows PC can reach). It is a mock: plain HTTP, test only.

```bash
curl -fsSLO https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/tests/poll-mock/poll_mock.py
python3 poll_mock.py --require-approval --tasks 3 --keep-serving
```

- [ ] It prints `poll URL: http://<this machine's address>:8080/poll`. Open port 8080 in the firewall for the test. Leave this terminal visible.

## 3. Windows 11: install idle

Once, in PowerShell **as administrator**, then reboot:

```powershell
wsl --install --no-distribution
```

Then in a normal PowerShell, as the user who stays logged on:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
shutup-and-run wsl install
```

- [ ] SmartScreen may warn once ("More info", "Run anyway"). The `.exe` is not signed.
- [ ] Steps `[1/8]` to `[8/8]`, a few minutes (340 MiB download, then apt).
- [ ] The doctor table is all `ok`, including `container  hello-world ran: Hello from Docker!`.
- [ ] Last line: `done. No poll URL: the worker is installed and idle.`
- [ ] If step 7 prints `could not change the task settings`, note the message. The install still works.

No window, and the status page:

```powershell
Get-Process wsl, shutup-and-run, conhost, WindowsTerminal, OpenConsole -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Format-Table Name, Id, MainWindowTitle
```

- [ ] Only the terminal you are typing in is listed. No window titled `wsl.exe` appeared during the install, and none appears after signing out and in again or after `shutup-and-run wsl start`.
- [ ] `http://127.0.0.1:8787/` in a browser shows `RODANDO / RUNNING`, `0 / 1`, and the version. The button pauses (`PAUSADO / PAUSED`) and resumes.
- [ ] `(Get-Content C:\ProgramData\shutup-and-run\status.json -Raw | ConvertFrom-Json).state` prints `idle`. After `shutup-and-run wsl stop` it prints `stopped`.
- [ ] `shutup-and-run wsl doctor` right after the install says `ok   service`, not "not running".

Task settings and one instance:

```powershell
(Get-ScheduledTask shutup-and-run).Settings | Format-List ExecutionTimeLimit, DisallowStartIfOnBatteries, StopIfGoingOnBatteries, MultipleInstances
shutup-and-run wsl serve
```

- [ ] `ExecutionTimeLimit : PT0S`, both battery settings `False`, `MultipleInstances : IgnoreNew`.
- [ ] `wsl serve` prints `another shutup-and-run is already running` and returns at once.

## 4. Windows 11: enroll and run work

```powershell
shutup-and-run wsl install --poll-url http://YOUR_VPS_IP:8080/poll
```

- [ ] `made a poll token for this machine`, and at the end `This machine has a new token. It gets no work until the poll URL approves it.` The token is never shown.
- [ ] On the VPS, the mock prints one `PENDING <hash> ip=<the PC's public IP> body={"worker":"<PC name>","os":"windows",...}`.
- [ ] `shutup-and-run wsl logs` shows `poll returned 403 Forbidden: the poll URL has not approved this machine`.

Approve it, on the VPS, in the folder where the mock runs, with the line the mock printed:

```bash
echo <hash> >> approved.txt
```

- [ ] Within five minutes (the worker backs off while refused) the mock prints `TASK mock-...-1`, then `ok   1/3 ... reason=exited exit=0`, three times, a few seconds apart and not 30 (0.2.9: the worker polls again as soon as a job ends), then a `SUMMARY` with `ok 3`. After that it answers `204`, and the polls go back to one every 30 seconds.
- [ ] `shutup-and-run wsl logs` shows `poll started a job` three times.

Install again, with one option changed:

```powershell
shutup-and-run wsl install --poll-url http://YOUR_VPS_IP:8080/poll --max-slots 2
```

- [ ] `kept this machine's poll token`, and the mock prints no new `PENDING`: it is the same machine.
- [ ] The doctor's `service` line is `ok` and its JSON has `"slots_total":2`: the service that answers read the new config. The install ends with `done`, and `with 2 slot(s)`.
- [ ] `wsl -d shutup-and-run -u root -e /usr/local/bin/shutup-and-run bans` prints `no bans`.

Open a second PowerShell, run `shutup-and-run wsl stop` and then `shutup-and-run wsl serve`, and leave it. In the first one, install again with the same command.

- [ ] Either the install completes and the second terminal is back at its prompt, or the install stops before step 3 with `a shutup-and-run wsl serve is still running`. It never prints `done` with the old service still the one running. Note which you saw.

### 0.2.9: hints, jobs, pairing query

- [ ] `shutup-and-run jobs` lists the three jobs, newest first, each `ok`, `exit 0`, with a duration. `shutup-and-run jobs --limit 1 --json` has one entry with `id`, `image`, `started_at`, `ended_at`, `duration_secs`, `exit_code`, `status`, `failure`, and nothing else.
- [ ] `shutup-and-run pause`, then `shutup-and-run doctor --json`: the `paused` line has `"code":"WORKER_PAUSED"` and a `"hint"` that names `shutup-and-run resume`. `shutup-and-run config set language pt`, the doctor again: the hint is in Portuguese. `shutup-and-run resume`.
- [ ] `shutup-and-run config set max_slots 0 --json` exits 3 with `"code":"INVALID_VALUE"` and a `"hint"`.
- [ ] `shutup-and-run config set poll_url "http://YOUR_VPS_IP:8080/poll?cpf=00000000000&email=nobody@example.com" --apply`. The mock keeps answering. `shutup-and-run config show`, `shutup-and-run doctor --report` and the page's Settings show `?cpf=***&email=***`; none shows `00000000000`. `shutup-and-run config get poll_url --reveal` shows the whole URL. Saving the page's form untouched keeps the URL.
- [ ] `shutup-and-run config set poll_url "http://YOUR_VPS_IP:8080/nope" --json`, then `shutup-and-run config check --json`: the `poll` line is `POLL_HTTP_ERROR` with `HTTP 404`, not `POLL_OK`. Set the right URL back with `--apply`.
- [ ] 0.2.10: `shutup-and-run status --json` has `last_poll_at` (seconds ago, not minutes) and `last_poll_status` (`nothing` while the queue is empty). `shutup-and-run doctor --json`: every line that is not ok has one `hint`, and `fix` is the same text. After `config set language pt --apply`, the page at `http://127.0.0.1:8787/` opens in Portuguese in a browser that never pressed its language button.
- [ ] `shutup-and-run wsl install --poll-interval-secs 5` exits 3 (`$LASTEXITCODE`), says `at least 30`, and changes nothing.

The same enrollment, queue and callback against n8n, with no mock: [examples/n8n/README.md](../examples/n8n/README.md#7-smoke-test).

### Shared-PC limits

This part has only been run on Linux and in unit tests. On the Windows PC:

```powershell
shutup-and-run wsl limits
Get-Content $env:USERPROFILE\.wslconfig
shutup-and-run status --json
```

- [ ] `wsl limits` prints this PC's RAM and processors, and a budget of about 40% of the RAM and half the processors. Note whether the install wrote `.wslconfig`, or said why it did not (a file was there, or another distribution is).
- [ ] `status --json` has `"limits"` with `"profile":"shared-pc"`, `"max_job_cpus":1.0`, and `"waiting_for":null`.
- [ ] While a job from the mock runs: `wsl -d shutup-and-run -u root docker inspect -f '{{.HostConfig.NanoCpus}} {{.HostConfig.Memory}} {{.HostConfig.MemorySwap}}' $(wsl -d shutup-and-run -u root docker ps -q)` prints `1000000000` and the same number twice (2147483648 on a PC with 16 GiB).
- [ ] Start the mock with `--memory 8g`: no job starts, and `shutup-and-run wsl logs` has `poll task rejected`.
- [ ] `shutup-and-run config set min_free_memory 900g --apply`: `status` shows `waiting_for: low on memory`, the mock gets no more polls, and the doctor's `resources` line is a warning with a `hint:`. `shutup-and-run config unset min_free_memory --apply` and the polls come back.
- [ ] With a `.wslconfig` of your own that has other keys and `memory=64GB`: `wsl limits` exits 1; `wsl limits --apply` changes only `memory=` and `processors=`, and `.wslconfig.bak` is the old file. The doctor says `not in effect yet` until the PC restarts or you run `wsl --shutdown` yourself.

### 0.2.11: idle wait, shutdown question, interrupted jobs

None of this ran on a real Windows PC before the release: CI builds it and runs the unit tests, and cannot shut a session down.

- [ ] `http://127.0.0.1:8787/` has **Computer resources**: "This PC (Windows)" with the PC's processors and memory, "WSL2" with smaller figures, the job limit, "PC without use for" counting up while you do not touch the PC, and "Can a new job start? yes".
- [ ] `shutup-and-run config set min_idle_secs 120 --apply`. While you use the PC: `shutup-and-run status` has `waiting_for: the PC is in use`, the page says "waiting: the PC is in use", and the mock gets no polls. Leave the PC alone for two minutes: the polls come back and a task runs. Touch the mouse while it runs: the job goes on. `shutup-and-run config unset min_idle_secs --apply`.
- [ ] No window appeared at any point (the `Get-Process` line of [WINDOWS.md](WINDOWS.md#for-the-person-at-the-pc)).
- [ ] With no job running, Start > Shut down > Restart: the PC restarts with no question.
- [ ] With a five-minute job running (section 5), Start > Sign out: Windows shows `shutup-and-run` and "A job is running on this PC...". Press **Cancel**: `shutup-and-run wsl logs` has `the shutdown was cancelled; the jobs go on`, and the job ends well later (`ok` at the mock).
- [ ] The same, and press **Sign out anyway**. Sign in again: `shutup-and-run jobs` shows that job as `interrupted`, the mock printed a line for it with `reason=interrupted`, the page counts 1 interrupted job, and no container was started for it again (`wsl -d shutup-and-run -u root -- docker ps -a` is empty).
- [ ] With a job running and the mock stopped, restart the PC. Start the mock again within the hour: the callback arrives on a later try, and "Callback delivery" on the page goes back to 0 waiting.

### 0.2.13: resource safety and the owner's authorization

Automated on Linux: `cargo test` covers healthy admission, each critical reading, missing readings, the consent and its scope, the non-interactive answers, the breaker and its end, OOM against a plain kill, throttling and profile `none`, all with simulated readings. Against a real Docker, without stressing the machine:

```bash
cargo build --release --locked
sudo bash tests/safety/linux.sh target/release/shutup-and-run
```

It pins its own worker to two processors so that it sees a small computer, authorizes through a pseudo-terminal the way a person would, holds a job at a fifth of a processor, stops and restarts `dockerd` under a running job, and runs a job out of a 16 MiB limit three times. 34 checks; about a minute and a half. It is not in CI: it ends with the breaker open.

| Simulated on Linux | How | Observed |
| --- | --- | --- |
| Small machine | worker pinned to 2 of 4 processors | `authorization_required`, push `503`, no container |
| Non-interactive authorization | `--json`; the word piped in with no terminal | exit 1, `AUTHORIZATION_REQUIRED`, nothing changed |
| Person at a terminal | pseudo-terminal, wrong word, then the word | no; then recorded with channel `terminal`, and jobs start |
| Low memory, CPU saturation, disk exhaustion, stale or missing readings | simulated readings in unit tests | `503` `SAFETY_BLOCKED` with the code; nothing stored; clears past the recovery line |
| CPU throttling | real job at `cpu: 0.2` for 40 s | exit 0, one `cpu_throttled` incident (99% of its time) |
| Docker daemon failure | `dockerd` stopped under a running job, then started | push `502`, worker alive, `state: failed`; jobs accepted again with no restart; the lost job closed once |
| Plain kill | `docker kill` | exit 137, no `oom_killed` |
| Container OOM, repeated | real job, `memory: 16m`, `tail /dev/zero`, three times | exit 137, `oom_killed: true`, one incident each; then `SAFETY_OOM_BREAKER`, which an authorization does not lift |

Not simulated on Linux: a real shortage of host memory or disk (only the readings are faked), and anything on the Windows side.

#### A small Windows PC: 2 processors, 5 GB

A Windows 11 VM with 2 vCPU and 5 GB of RAM, nested virtualization on. Nothing below has been run on Windows: the supervisor's CPU and disk readings are new and were only compiled. **Do not take this configuration to be safe until the numbers below say so.** Keep Task Manager (Performance) open and write the figures down.

1. Before installing, with the VM idle for two minutes: memory **available** and CPU % in Task Manager.

   | | Available memory | CPU | Feels |
   | --- | --- | --- | --- |
   | Idle, before install | | | |
   | Installed, no job | | | |
   | One job running | | | |

2. Install (section 3), with the mock from section 2 serving three tasks.
   - [ ] The install ends with exit 0. The doctor has a `--  safety` line with `SAFETY_SMALL_MACHINE` and a `hint:` that says not to answer for the owner.
   - [ ] `http://127.0.0.1:8787/` says **WAITING FOR YOUR AUTHORIZATION** and "a small computer". The mock printed `WORKER` at most once and no `TASK`.
   - [ ] `shutup-and-run safety --json` has `"admission":"authorization_required"`; the reason's `detail` has this PC's real processors and memory.
   - [ ] `shutup-and-run safety authorize` exits 1 with `AUTHORIZATION_REQUIRED` and nothing changes. So does `shutup-and-run safety authorize --json`.
3. On the page, open **Safety of this computer**.
   - [ ] The risks, the notice (it says the safeguards do not remove the risk, and that authorizing does not change the provider's responsibilities), one box, one button. The link at the top right shows it in Portuguese only.
   - [ ] The button without the box does nothing (the browser asks for the box).
   - [ ] Tick the box, **Authorize**: "Authorized." The page goes back to **RUNNING**, the mock prints `TASK`, and the job ends `ok`. Fill in the table's last two rows while a job runs. Is the mouse smooth, does a browser tab open in a normal time?
   - [ ] The next two tasks start with no further question. `shutup-and-run safety --json` has `"authorization"` with `"channel":"page"`.
4. Memory pressure on the PC. In a PowerShell window (it takes about 1.5 GB and holds it):

   ```powershell
   $hold = @(); 1..15 | ForEach-Object { $b = New-Object byte[] 100MB; for ($i = 0; $i -lt $b.Length; $i += 4096) { $b[$i] = 1 }; $hold += ,$b }
   ```

   - [ ] When Task Manager shows under 1 GB available: within 15 seconds the page says **NEW JOBS BLOCKED FOR SAFETY**, memory critically low, "this PC: ... available". The mock gets no polls. A job that was already running goes on and ends.
   - [ ] The Authorize button is not offered for it, and `shutup-and-run safety authorize` says nothing is waiting.
   - [ ] Close that PowerShell window. Nothing changes until 1.5 GB are available again; then the page goes back by itself and the polls resume. "Last incidents" has `blocked` and `recovered`.
   - [ ] If Task Manager never showed 1 GB available even before the script, this PC is blocked most of the time: write that down. It is the answer to "is 5 GB enough", not a bug.
5. Processor saturation. `1..2 | ForEach-Object { Start-Job { while ($true) {} } }`.
   - [ ] Within about 20 seconds: blocked, "The processor is saturated", "this PC: processors 9x% busy". `Get-Job | Remove-Job -Force`: back by itself once it is under 85%.
6. A job that runs out of memory. Restart the mock with `--tasks 3 --image alpine --command "tail /dev/zero" --memory 64m --expect-failures 3`.
   - [ ] Each ends in seconds. `shutup-and-run jobs` says "ran out of memory ... (confirmed by Docker)" for each; Task Manager's available memory did not drop by more than the job's 64 MB.
   - [ ] After the third: blocked, "Jobs ran out of memory repeatedly". Fifteen minutes later it ends by itself.
7. On the page, **Withdraw the authorization**: **WAITING FOR YOUR AUTHORIZATION** again, and the mock gets no polls.
8. No window appeared at any point, and `shutup-and-run wsl logs` has no line with a token or a task's env.

What to report: the table, which of the boxes failed, and whether steps 4 and 5 blocked and recovered at the figures Task Manager showed. If the idle PC already sits under 1 GB available, or the VM feels slow with one job at the default limits, this PC should not lend its resources, whatever its owner authorizes.

### 0.2.15: update over a running worker

CI installs the latest release in WSL2 on the Windows runner and updates it, with no job, pause or authorization. This is the rest, on a PC that runs an older release with a job.

- [ ] With a five-minute job running, run the install line. It prints `stopping the worker`, then the `wsl update` steps, and ends with `updated to`. The job's callback arrives with how it ended (stopped, not `ok`).
- [ ] `shutup-and-run version --json` and `status.json` show the same new `version`; `state` is `idle`; the doctor's `version` line is ok.
- [ ] `C:\ProgramData\shutup-and-run` has no `*.tmp`, `SHA256SUMS` or `shutup-and-run.exe.old`.
- [ ] `shutup-and-run config show` is what it was before (poll URL, slots, worker name), and the mock sees the same token.
- [ ] A worker paused before the update is still paused; a PC whose owner had authorized is still authorized.
- [ ] Put the old binary back by hand (`wsl -d shutup-and-run -u root -- cp ...`): the doctor says `VERSION_MISMATCH` with the hint `shutup-and-run wsl update`, and that command clears it.

### 0.2.19: battery, full screen, lend hours, usage, consent from the contract

On a Windows laptop with the mock serving short jobs (`--image alpine --command "sh -c 'yes > /dev/null & sleep 90'"`).

- [ ] Plugged in: the page's **Power** line says plugged in. Unplug it above 60%: the **This PC is on its battery** box appears with the charge, and jobs still start. The doctor has a `lending` line with `ON_BATTERY`.
- [ ] Let it drain under 60% (or test at that point): the page says **WAITING**, the mock gets no polls, a job already running ends normally, and `shutup-and-run doctor` says `BATTERY_LOW`. Plug it in: polls resume within a minute.
- [ ] Start a PowerPoint slide show (or a video in full screen): **WAITING**, "presenting or has a program in full screen", no polls. Leave it: polls resume. A Teams call in a window is not a wait; write down whether one in full screen is.
- [ ] `shutup-and-run config set lend_hours "mon-fri 19-07" --apply` at a time outside it: **WAITING**, `OUTSIDE_LEND_HOURS`, and the **Lends itself** line shows the window and the time now, which is the PC's clock. `config unset lend_hours --apply`: polls resume.
- [ ] After a job of 90 seconds, `shutup-and-run jobs` has `used: cpu ... s, memory peak ...`, about one CPU-minute and a few MB; `jobs --json` has `cpu_ms` and `memory_peak_bytes`; the page's jobs table has the same in **CPU / memory**.
- [ ] On a small PC (or one where the page asks for an authorization), `shutup-and-run wsl uninstall`, then `shutup-and-run wsl install --poll-url URL --owner-consent`: it prints `recorded the owner's consent from the contract`, the page is **RUNNING** and never asks, and **Safety** shows the authorization on the `contract`. **Withdraw** on the page: it asks again.


What no CI run reaches on a real Windows PC since then (the Windows clock, a pause for a while, memory and the programs holding it, the WSL2 cache) is listed, in Portuguese, in [WINDOWS-PENDING.md](WINDOWS-PENDING.md).

### Certificate passthrough and the demos

Not run on Windows yet. On the Windows PC, with a test `.pfx` (never a real one for this) in a folder whose name has a space and an accent:

```powershell
shutup-and-run version --json
shutup-and-run config check
shutup-and-run demo
shutup-and-run demo sign
$env:SHUTUP_AND_RUN_CERT_PASSWORD = "the-test-password"
shutup-and-run certificate set --file "C:\Users\YOU\Área de Trabalho\test cert.pfx" --accept-risk --apply
shutup-and-run demo sign --configured
```

- [ ] `version --json` says `"platform":"windows"` and `"runtime":"wsl2"`.
- [ ] `demo` and `demo sign` end with `passed`, and `shutup-and-run-demo-signed.pdf` is in the current Windows folder and opens.
- [ ] Without `--accept-risk`, `certificate set` prints the warning and exits 2. With a wrong password it exits 3 with `CERTIFICATE_PASSWORD_WRONG`; with a `.txt` renamed to `.pfx`, `CERTIFICATE_INVALID`; with a path that does not exist, the path and exit 1.
- [ ] After `set`: `certificate status` says ON; `shutup-and-run config show` has `certificate_enabled` and no password; `shutup-and-run doctor --report` has `certificate passthrough: ON` and not the password.
- [ ] The local page, Advanced settings, Certificate: the same with the file chooser; saving without the box ticked is refused; the password field comes back empty.
- [ ] Queue two jobs from the mock (`--max-slots 2`) whose command is `sh -c 'ls -l $SHUTUP_AND_RUN_CERT_PATH; echo x >> $SHUTUP_AND_RUN_CERT_PATH || echo read-only'`: both list the file and print `read-only`.
- [ ] While a long job runs, `certificate disable --apply`: the job finishes; the next job has no `/run/shutup-and-run/certificate`.
- [ ] `wsl -d shutup-and-run -u root ls /run/shutup-and-run/jobs` is empty when no job runs, also after a job that timed out and after `wsl stop` / `wsl start`.
- [ ] `shutup-and-run wsl reset`: the install says passthrough was turned off; `certificate status` says off.

## 5. Windows 11: the shortcut, stop, shutdown

Put the shortcut on the desktop:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/shutup-and-run.bat -OutFile "$env:USERPROFILE\Desktop\shutup-and-run.bat"
```

Double-click it.

- [ ] `[RODANDO / RUNNING]  Tarefas agora / Jobs now: 0` and `PODE DESLIGAR O PC. / YOU CAN TURN THE PC OFF.`

To see the other answer, give the worker a long job. On the VPS, stop the mock (Ctrl+C) and start it with a task that sleeps five minutes:

```bash
python3 poll_mock.py --require-approval --image ghcr.io/nailtongomes/shutup-and-run-smoke --command "sleep 300" --timeout 600 --tasks 1 --keep-serving
```

When the mock prints `TASK mock-...-1`, wait half a minute for the image to download, then double-click the shortcut.

- [ ] `Jobs now: 1` and `NAO DESLIGUE AGORA. / DO NOT TURN OFF NOW.` Press `P`.
- [ ] `[PARADO / STOPPED]  PODE DESLIGAR O PC.` within about 15 seconds.
- [ ] Double-click again: it starts the service and says `RUNNING`. The mock then prints `FAIL 1/1` for the interrupted job, with `reason=interrupted`.

Start a long job again, then sign out of Windows while it runs, and sign in again.

- [ ] `shutup-and-run wsl logs` has `the session is ending; stopping the jobs`, and the mock printed a `FAIL` line for that job.

Reboot the PC and log on.

- [ ] Without touching anything, the shortcut says `RUNNING`.
- [ ] `wsl -d shutup-and-run -u root -- docker ps -a` lists no leftover container.

## 6. Remove

```powershell
shutup-and-run wsl uninstall --purge
```

```bash
curl -fsSL https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.sh | sudo bash -s -- --uninstall --purge
```

- [ ] `wsl -l -v` no longer lists `shutup-and-run`, and the scheduled task is gone. Close port 8080 again.

## If something fails

Send these:

```powershell
shutup-and-run wsl doctor
shutup-and-run wsl logs --lines 300
wsl --version
```

```bash
journalctl -u shutup-and-run -n 200 --no-pager
```

Neither log contains a token.

### 0.2.14: managed containers

Automated on Linux (`cargo test managed`, on the mock runtime and a fake clock): validation of a definition, persistence, the interval (start to start, no overlap, one run after missed intervals), the `always` backoff over a simulated hour, manual start and stop, disable, a restarted worker, a container that vanished, a failed start, Docker not answering, low memory, a pause, full slots, the safety block, the two modes and the busy refusal, the page and its logins, and that a secret is in nothing shown.

Against a real Docker, by hand, about four minutes:

```bash
cargo build --release --locked
sudo bash tests/managed/linux.sh target/release/shutup-and-run
```

It runs the same cases with real containers: a worker killed with SIGKILL, a container killed from outside, a container that exits 3 forever, and the secret looked for in every output, the log and `status.json`. Stopping and starting `dockerd` under a running `always` container was done once by hand: the worker stayed up and one new container followed.

Not exercised: anything on Windows. There the commands are forwarded into the WSL2 distribution like `config` and `jobs`; `managed apply FILE` and `managed secret` cross on stdin. On a Windows 11 PC, check by hand:

- [ ] `shutup-and-run managed apply C:\path\rpa-selenium.json`, then `managed secret rpa-selenium API_TOKEN` with the value piped in, then `managed mode on`: `managed list` shows it `running` and the page at `http://127.0.0.1:8787/local/managed` agrees.
- [ ] With it running, sign out of Windows: the shutdown screen names `shutup-and-run`, as for any job.
- [ ] `managed mode off` while it runs is refused with `MANAGED_BUSY`; after `managed stop`, it is accepted and the worker polls again.
- [ ] A disabled container does not start, not even with `managed start` (on purpose). For one run of a disabled one: `managed enable NAME`, `managed start NAME`, `managed disable NAME`; the run in progress finishes.

Managed mode is also the easiest way to test a Windows PC without a poll URL: the checks in [WINDOWS-PENDING.md](WINDOWS-PENDING.md) that need a job running take a `once` container started with `managed start`.

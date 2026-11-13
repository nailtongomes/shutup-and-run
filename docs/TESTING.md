# Testing a release by hand

Português: [TESTING.pt-BR.md](TESTING.pt-BR.md).

One Linux VPS (yours, trusted) and one Windows 11 PC (treated as a client's). About 30 minutes. Each step has the command, then what to expect. If a step does not match, stop there and keep the output.

## 0. The release exists

The one-line installs download from the latest GitHub release.

- [ ] `https://github.com/nailtongomes/shutup-and-run/releases/latest` is `v0.2.4` or later and has three files: `shutup-and-run-x86_64-unknown-linux-musl`, `shutup-and-run-x86_64-pc-windows-msvc.exe`, `SHA256SUMS`.

If it is not there, tag `main`: `git tag v0.2.4 && git push origin v0.2.4`, and wait for the `release` workflow.

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

- [ ] Within five minutes (the worker backs off while refused) the mock prints `TASK mock-...-1`, then `ok   1/3 ... reason=exited exit=0`, three times, 15 seconds apart, then a `SUMMARY` with `ok 3`. After that it answers `204`.
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
- [ ] `shutup-and-run config set min_free_memory 900g --apply`: `status` shows `waiting_for: low on memory`, the mock gets no more polls, and the doctor's `resources` line is a warning with a `fix:`. `shutup-and-run config unset min_free_memory --apply` and the polls come back.
- [ ] With a `.wslconfig` of your own that has other keys and `memory=64GB`: `wsl limits` exits 1; `wsl limits --apply` changes only `memory=` and `processors=`, and `.wslconfig.bak` is the old file. The doctor says `not in effect yet` until the PC restarts or you run `wsl --shutdown` yourself.

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
- [ ] Double-click again: it starts the service and says `RUNNING`. The mock then prints `FAIL 1/1` for the interrupted job, with a non-zero `exit`.

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

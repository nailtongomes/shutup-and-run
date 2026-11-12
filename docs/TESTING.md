# Testing v0.2.0 by hand

Português: [TESTING.pt-BR.md](TESTING.pt-BR.md).

One Linux VPS (yours, trusted) and one Windows 11 PC (treated as a client's). About 30 minutes. Each step has the command, then what to expect. If a step does not match, stop there and keep the output.

## 0. The release exists

The one-line installs download from the latest GitHub release.

- [ ] `https://github.com/nailtongomes/shutup-and-run/releases/latest` is `v0.2.0` and has three files: `shutup-and-run-x86_64-unknown-linux-musl`, `shutup-and-run-x86_64-pc-windows-msvc.exe`, `SHA256SUMS`.

If it is not there, tag `main`: `git tag v0.2.0 && git push origin v0.2.0`, and wait for the `release` workflow.

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

## 2. A poll URL to test with

On the VPS (or any machine the Windows PC can reach). It is a mock: plain HTTP, test only.

```bash
curl -fsSLO https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/examples/mock-poll.py
MOCK_PUBLIC_URL=http://YOUR_VPS_IP:8080 python3 mock-poll.py
```

- [ ] `mock poll URL on 0.0.0.0:8080`. Open port 8080 in the firewall for the test. Leave this terminal visible.

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

- [ ] Within five minutes (the worker backs off while refused) the mock prints `TASK 1/3`, then `CALLBACK ... "status":"exited","exit_code":0,"reason":"exited"`, three times, 15 seconds apart. Then nothing: the mock answers `204`.
- [ ] `shutup-and-run wsl logs` shows `poll started a job` three times.

Install again with the same command:

- [ ] `kept this machine's poll token`, and the mock prints no new `PENDING`: it is the same machine.

## 5. Windows 11: the shortcut, stop, shutdown

Put the shortcut on the desktop:

```powershell
irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/shutup-and-run.bat -OutFile "$env:USERPROFILE\Desktop\shutup-and-run.bat"
```

Double-click it.

- [ ] `[RODANDO / RUNNING]  Tarefas agora / Jobs now: 0` and `PODE DESLIGAR O PC. / YOU CAN TURN THE PC OFF.`

To see the other answer, give the worker a long job. On the VPS, stop the mock (Ctrl+C) and start it with a task that sleeps five minutes:

```bash
MOCK_IMAGE=ghcr.io/nailtongomes/shutup-and-run-smoke MOCK_COMMAND="sleep 300" MOCK_TIMEOUT=600 MOCK_TASKS=1 MOCK_PUBLIC_URL=http://YOUR_VPS_IP:8080 python3 mock-poll.py
```

When the mock prints `TASK 1/1`, wait half a minute for the image to download, then double-click the shortcut.

- [ ] `Jobs now: 1` and `NAO DESLIGUE AGORA. / DO NOT TURN OFF NOW.` Press `P`.
- [ ] `[PARADO / STOPPED]  PODE DESLIGAR O PC.` within about 15 seconds.
- [ ] Double-click again: it starts the service and says `RUNNING`. The mock then prints a `CALLBACK` for the interrupted job with `"status":"exited"` and a non-zero `exit_code`.

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

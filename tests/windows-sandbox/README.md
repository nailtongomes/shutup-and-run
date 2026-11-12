# Windows Sandbox test

A hand test for the two things that depend on the Windows user's rights and that CI cannot show, because the CI runner's user is an administrator:

1. `scripts/install.ps1`, run twice (new install, then update), leaves an `.exe` whose SHA-256 is the one in the release's `SHA256SUMS`.
2. The logon task is created by a user who is **not** an administrator, by the same code as step 7 of `wsl install` (`shutup-and-run wsl task register`), it exists, it runs, and `wsl uninstall` removes it.

```powershell
.\tests\windows-sandbox\run.ps1
```

It also reads the `.exe` itself: that it is a console program (so `install`, `doctor` and `logs` print to the terminal), that it does not import the Visual C++ runtime (a clean Windows, like the Sandbox, does not have it), and that the task opens no window.

It opens Windows Sandbox, waits, prints one `PASS` or `FAIL` line per item, and exits 1 if any failed. Results and logs are in `C:\sar-sandbox-results\` on the host: `summary.txt`, `results.txt`, and one `*.stdout.log` / `*.stderr.log` per command.

To test a build that is not released yet, give it the `.exe`: the `shutup-and-run-windows-exe` artifact of a CI run, or `target\release\shutup-and-run.exe`.

```powershell
.\tests\windows-sandbox\run.ps1 -Exe C:\Downloads\shutup-and-run.exe
```

`install.ps1` always downloads the latest release; `-Exe` replaces the binary only for the task items. `wsl task` exists from v0.2.2, so against an older release the task items fail unless `-Exe` is given.

## What it does not test

**Windows Sandbox cannot run WSL2.** It is itself a VM and has no nested virtualization. So this test never runs `wsl install`: no distribution, no Docker, no container, no poll. The full install, steps 1 to 8 and the smoke test, needs a real Windows 11 machine; [docs/TESTING.md](../../docs/TESTING.md) is that test.

## Needs

Windows 11 Pro, Enterprise or Education, with the feature on (as administrator, then reboot):

```powershell
Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All
```

And network access from the Sandbox to github.com.

## How it works

- `run.ps1` (host) fills the two paths in `sar.wsb`, opens it, and waits for `done.txt`.
- `in-sandbox.ps1` (Sandbox, its built-in administrator) creates the local user `sartest`, a member of `Users` only, and starts `as-user.ps1` as that user.
- `as-user.ps1` (Sandbox, `sartest`) runs the items and writes `results.txt`.

The test user is started with a secondary logon, not at the Sandbox's own desktop. A task that runs "only when the user is logged on" may be registered that way and still not start; if `task-ran` is the only `FAIL`, that is the likely reason, and a real machine is the check.

# Install the shutup-and-run worker on Windows 11 (WSL2).
#
# In PowerShell, as the user who will stay logged on:
#   $env:SHUTUP_AND_RUN_POLL_TOKEN = "<token>"   # optional: without it the machine makes its own and waits for approval
#   irm https://raw.githubusercontent.com/nailtongomes/shutup-and-run/main/scripts/install.ps1 | iex
#   shutup-and-run wsl install --poll-url https://orchestrator.example/poll
#
# Or in one go:
#   $env:SHUTUP_AND_RUN_POLL_URL = "https://orchestrator.example/poll"
#   irm .../install.ps1 | iex
#
# What it does: downloads the .exe of the latest release and SHA256SUMS,
# checks the hash, puts the .exe in %ProgramData%\shutup-and-run, adds that
# folder to the user's PATH, and, when SHUTUP_AND_RUN_POLL_URL is set, runs
# `shutup-and-run wsl install`. Everything else (the WSL distribution,
# Docker, the Linux binary, the logon task) is done by the .exe. Remove it
# all with `shutup-and-run wsl uninstall --purge`.
#
# Over an installed worker it is the update: it stops the worker
# (`shutup-and-run wsl stop`, then ends what is left of that one .exe), swaps
# the .exe, and runs `shutup-and-run wsl update`, which puts the Linux binary
# of the same version in the distribution and starts the worker again. The
# config, the poll token and the job history are kept. On any failure the
# previous .exe is put back and started, and no download is left behind.
#
# Internet sharing (no administrator, no WSL needed), alone or beside the
# worker:
#   $env:SHUTUP_AND_RUN_RELAY = "relay.example.com:7000"
#   $env:SHUTUP_AND_RUN_RELAY_TOKEN = "<token>"        # only when the relay asks for one
#   $env:SHUTUP_AND_RUN_INTERNET_LOGIN = "<id>:<password>"   # optional: what the operator chose
#   irm .../install.ps1 | iex
# runs `shutup-and-run internet on --relay ... --wait 60`. With
# SHUTUP_AND_RUN_POLL_URL as well it also installs the worker, when this PC
# has WSL2 (`shutup-and-run fit`); without WSL2 it lends the internet now
# and says what the worker needs. An update over it starts the task
# shutup-and-run-internet again.
#
# For the worker it needs WSL2 already enabled: `wsl --install --no-distribution` as
# administrator, then a reboot. This script does not need administrator.

$ErrorActionPreference = "Stop"

$Repo = "nailtongomes/shutup-and-run"
$Asset = "shutup-and-run-x86_64-pc-windows-msvc.exe"
$Url = if ($env:SHUTUP_AND_RUN_EXE_URL) { $env:SHUTUP_AND_RUN_EXE_URL } else { "https://github.com/$Repo/releases/latest/download/$Asset" }
$Dir = Join-Path $env:ProgramData "shutup-and-run"
$Exe = Join-Path $Dir "shutup-and-run.exe"
$Old = "$Exe.old"
$Failed = "$Exe.failed"
$Tmp = Join-Path $Dir "$Asset.tmp"
$Sums = Join-Path $Dir "SHA256SUMS"
$Help = "https://github.com/$Repo/blob/main/docs/WINDOWS.md#update-and-repair"

# An exit code as Windows writes it, with what the common ones mean. A program
# that cannot start prints nothing, so the code is all there is to go on.
function Describe-Exit([int]$Code) {
    $Hex = "0x{0:X8}" -f ([int64]$Code -band 0xFFFFFFFFL)
    $Hint = switch ($Hex) {
        "0xC0000135" { "a DLL it needs is missing on this PC. Send this line to support." }
        "0xC000007B" { "it is not a program for this kind of Windows (it needs 64-bit x86)." }
        "0xC0000005" { "it crashed while starting." }
        "0xC0000409" { "it crashed while starting." }
        default { "" }
    }
    if ($Hint) { return "$Code ($Hex): $Hint" }
    return "$Code ($Hex)"
}

# Native commands go through cmd: with $ErrorActionPreference = "Stop",
# Windows PowerShell 5.1 turns the stderr of a native command redirected with
# 2> into an error, and a task that does not exist would end this script.
function Test-Task([string]$Name = "shutup-and-run") {
    cmd.exe /c "schtasks.exe /Query /TN $Name >nul 2>&1"
    return ($LASTEXITCODE -eq 0)
}

# Internet sharing runs this same .exe; Stop-Worker ended it with the rest.
function Start-Internet {
    if ($Internet) { cmd.exe /c "schtasks.exe /Run /TN shutup-and-run-internet >nul 2>&1" }
}

# Stop the worker this folder runs: the clean way first (the task ends, each
# job gets SIGTERM and 10 seconds, the distribution stops), then whatever is
# still running from this very .exe. `schtasks /End` alone leaves the
# supervisor alive under its console host, holding the file. No other
# process is touched.
function Stop-Worker {
    if (Test-Path $Exe) { cmd.exe /c "`"$Exe`" wsl stop >nul 2>&1" }
    $Left = @(Get-Process -Name shutup-and-run -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Exe })
    if ($Left.Count -gt 0) {
        Write-Host "ending $($Left.Count) process(es) still running $Exe"
        $Left | Stop-Process -Force -ErrorAction SilentlyContinue
        $Left | Wait-Process -Timeout 30 -ErrorAction SilentlyContinue
    }
}

New-Item -ItemType Directory -Force -Path $Dir | Out-Null
$Had = Test-Path $Exe
$Task = $Had -and (Test-Task)
$Internet = $Had -and (Test-Task "shutup-and-run-internet")
$Stopped = $false
$Swapped = $false
try {
    Write-Host "downloading $Url"
    Invoke-WebRequest -Uri $Url -OutFile $Tmp -UseBasicParsing
    $SumsUrl = $Url.Substring(0, $Url.LastIndexOf("/")) + "/SHA256SUMS"
    Invoke-WebRequest -Uri $SumsUrl -OutFile $Sums -UseBasicParsing

    # "<hash>  <name>" or "<hash> *<name>". Match the base name, as install.sh does.
    $Expected = $null
    foreach ($Line in Get-Content $Sums) {
        $Parts = $Line -split "\s+", 2
        if ($Parts.Count -lt 2) { continue }
        $Name = $Parts[1].TrimStart("*")
        $Name = $Name.Substring($Name.LastIndexOfAny(@("/", "\")) + 1)
        if ($Name -eq $Asset) { $Expected = $Parts[0].ToLower(); break }
    }
    if (-not $Expected) { throw "SHA256SUMS does not list $Asset. Nothing was installed." }
    $Actual = (Get-FileHash -Algorithm SHA256 $Tmp).Hash.ToLower()
    if ($Actual -ne $Expected) {
        throw "checksum mismatch for $Asset (expected $Expected, got $Actual). Nothing was installed."
    }

    if ($Had) {
        Write-Host "stopping the worker"
        $Stopped = $true
        Stop-Worker
        # Windows renames a file a process still runs, and refuses to
        # overwrite it, so the old .exe moves aside instead of being replaced.
        Move-Item -Force $Exe $Old
    }
    Move-Item -Force $Tmp $Exe
    $Swapped = $true
    Write-Host "installed $Exe"

    # Does it start at all?
    cmd.exe /c "`"$Exe`" --help >nul 2>&1"
    if ($LASTEXITCODE -ne 0) {
        throw "$Exe did not start: exit code $(Describe-Exit $LASTEXITCODE)"
    }

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not ($UserPath -split ";" | Where-Object { $_ -eq $Dir })) {
        [Environment]::SetEnvironmentVariable("Path", "$UserPath;$Dir", "User")
        Write-Host "added $Dir to your PATH (open a new terminal to use it)"
    }
    if (-not ($env:Path -split ";" | Where-Object { $_ -eq $Dir })) { $env:Path = "$env:Path;$Dir" }

    $Relay = $env:SHUTUP_AND_RUN_RELAY
    if ($Relay) {
        $On = @("internet", "on", "--relay", $Relay, "--wait", "60")
        if ($env:SHUTUP_AND_RUN_RELAY_TOKEN) { $On += @("--token", $env:SHUTUP_AND_RUN_RELAY_TOKEN) }
        if ($env:SHUTUP_AND_RUN_INTERNET_LOGIN) { $On += @("--login", $env:SHUTUP_AND_RUN_INTERNET_LOGIN) }
        & $Exe @On
        # Exit 1 is on but not connected yet: the line above says why, and it
        # keeps trying by itself. Not a reason to undo the install.
        if ($LASTEXITCODE -eq 2) { throw "shutup-and-run internet on: exit code 2 (usage)" }
        if (-not $env:SHUTUP_AND_RUN_INTERNET_LOGIN) {
            Write-Host "give the operator what this prints: shutup-and-run internet credentials"
        }
    }
    # Containers too only where WSL2 is already there and the PC has room:
    # enabling WSL2 needs an administrator and a restart, which an install
    # that lends the internet does not wait for.
    $Worker = $env:SHUTUP_AND_RUN_POLL_URL
    if ($Relay -and $Worker) {
        $Fit = (& $Exe fit --json) | ConvertFrom-Json
        if (-not $Fit.containers) {
            $Worker = $null
            Write-Host "the worker waits: $($Fit.checks[0].detail)"
            Write-Host "hint: $($Fit.checks[0].hint) Then run this install again."
        }
    }
    if ($Worker) {
        & $Exe wsl install --poll-url $Worker
        if ($LASTEXITCODE -ne 0) { throw "shutup-and-run wsl install failed: exit code $(Describe-Exit $LASTEXITCODE)" }
    } elseif ($Had -and ($Task -or (-not $Internet -and -not $Relay))) {
        & $Exe wsl update
        # Exit 2 is the usage: a release older than `wsl update`. Its install
        # keeps the config and puts in the Linux binary of its own version.
        if ($LASTEXITCODE -eq 2) { & $Exe wsl install }
        if ($LASTEXITCODE -ne 0) { throw "shutup-and-run wsl update failed: exit code $(Describe-Exit $LASTEXITCODE)" }
    } elseif (-not $Relay) {
        Write-Host ""
        Write-Host "next (set SHUTUP_AND_RUN_POLL_TOKEN first if your orchestrator issued one):"
        Write-Host "  shutup-and-run wsl install --poll-url https://orchestrator.example/poll"
        Write-Host "or, to only check this machine (no polling, smoke test only):"
        Write-Host "  shutup-and-run wsl install"
    }
    Start-Internet
    # The old file can stay locked for a moment after its process ended.
    for ($i = 0; ($i -lt 10) -and (Test-Path $Old); $i++) {
        Remove-Item -Force -ErrorAction SilentlyContinue $Old
        if (Test-Path $Old) { Start-Sleep -Seconds 1 }
    }
} catch {
    $Why = $_
    if ($Stopped) {
        if ($Swapped -and (Test-Path $Old)) {
            Write-Host "putting back the previous $Exe"
            Stop-Worker
            # Renamed aside, like the old one was: the file of a process just
            # ended can stay locked for a moment.
            Move-Item -Force $Exe $Failed
            Move-Item -Force $Old $Exe
        } elseif (-not (Test-Path $Exe) -and (Test-Path $Old)) {
            Move-Item -Force $Old $Exe
        }
        if ($Task) {
            cmd.exe /c "schtasks.exe /Run /TN shutup-and-run >nul 2>&1"
            Write-Host "started the previous version again"
        }
        Start-Internet
    }
    Write-Host "hint: nothing of the new version was left behind; read the error below, then follow $Help"
    throw $Why
} finally {
    Remove-Item -Force -ErrorAction SilentlyContinue $Tmp, $Sums
    for ($i = 0; ($i -lt 10) -and (Test-Path $Failed); $i++) {
        Remove-Item -Force -ErrorAction SilentlyContinue $Failed
        if (Test-Path $Failed) { Start-Sleep -Seconds 1 }
    }
}

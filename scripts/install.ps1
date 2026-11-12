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
# Needs WSL2 already enabled: `wsl --install --no-distribution` as
# administrator, then a reboot. This script does not need administrator.

$ErrorActionPreference = "Stop"

$Repo = "nailtongomes/shutup-and-run"
$Asset = "shutup-and-run-x86_64-pc-windows-msvc.exe"
$Url = if ($env:SHUTUP_AND_RUN_EXE_URL) { $env:SHUTUP_AND_RUN_EXE_URL } else { "https://github.com/$Repo/releases/latest/download/$Asset" }
$Dir = Join-Path $env:ProgramData "shutup-and-run"
$Exe = Join-Path $Dir "shutup-and-run.exe"

New-Item -ItemType Directory -Force -Path $Dir | Out-Null

$Tmp = Join-Path $Dir "$Asset.tmp"
$Sums = Join-Path $Dir "SHA256SUMS"
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
if (-not $Expected) { throw "SHA256SUMS does not list $Asset" }
$Actual = (Get-FileHash -Algorithm SHA256 $Tmp).Hash.ToLower()
if ($Actual -ne $Expected) {
    Remove-Item -Force $Tmp
    throw "checksum mismatch for $Asset (expected $Expected, got $Actual). Nothing was installed."
}

# A running copy cannot be overwritten. Stop the task first, then move.
if (Test-Path $Exe) {
    # Through cmd: with $ErrorActionPreference = "Stop", Windows PowerShell 5.1
    # turns the stderr of a native command redirected with 2> into an error,
    # and a task that does not exist yet would end this script right here.
    cmd.exe /c "schtasks.exe /End /TN shutup-and-run >nul 2>&1"
    Start-Sleep -Seconds 1
}
Move-Item -Force $Tmp $Exe
Remove-Item -Force $Sums
Write-Host "installed $Exe"

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

# Does it start at all? Through cmd, for the same reason as above.
cmd.exe /c "`"$Exe`" --help >nul 2>&1"
if ($LASTEXITCODE -ne 0) {
    throw "$Exe did not start: exit code $(Describe-Exit $LASTEXITCODE)"
}

$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not ($UserPath -split ";" | Where-Object { $_ -eq $Dir })) {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$Dir", "User")
    Write-Host "added $Dir to your PATH (open a new terminal to use it)"
}
$env:Path = "$env:Path;$Dir"

if ($env:SHUTUP_AND_RUN_POLL_URL) {
    & $Exe wsl install --poll-url $env:SHUTUP_AND_RUN_POLL_URL
    if ($LASTEXITCODE -ne 0) { throw "shutup-and-run wsl install failed: exit code $(Describe-Exit $LASTEXITCODE)" }
} else {
    Write-Host ""
    Write-Host "next (set SHUTUP_AND_RUN_POLL_TOKEN first if your orchestrator issued one):"
    Write-Host "  shutup-and-run wsl install --poll-url https://orchestrator.example/poll"
    Write-Host "or, to only check this machine (no polling, smoke test only):"
    Write-Host "  shutup-and-run wsl install"
}

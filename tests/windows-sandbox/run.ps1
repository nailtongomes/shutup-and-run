# Run the Windows Sandbox test and print its summary. On the host:
#
#   .\tests\windows-sandbox\run.ps1
#   .\tests\windows-sandbox\run.ps1 -Exe C:\path\to\shutup-and-run.exe
#
# -Exe tests the logon task with a build that is not released yet (the CI
# artifact "shutup-and-run-windows-exe", or target\release after cargo build).
# Without it the test uses the .exe that install.ps1 downloads.
# Results and logs land in -ResultsDir. Exit code 1 when an item failed.
param(
    [string]$Exe = "",
    [string]$ResultsDir = "C:\sar-sandbox-results",
    [int]$TimeoutMinutes = 20
)

$ErrorActionPreference = "Stop"

$Sandbox = Join-Path $env:WINDIR "System32\WindowsSandbox.exe"
if (-not (Test-Path $Sandbox)) {
    Write-Host "Windows Sandbox is not enabled. As administrator, then reboot:"
    Write-Host "  Enable-WindowsOptionalFeature -Online -FeatureName Containers-DisposableClientVM -All"
    Write-Host "It needs Windows 11 Pro, Enterprise or Education."
    exit 2
}

$Here = Split-Path -Parent $MyInvocation.MyCommand.Path
$Repo = (Resolve-Path (Join-Path $Here "..\..")).Path

# A clean results folder, so an old summary is never read as this run's.
if (Test-Path $ResultsDir) { Remove-Item -Recurse -Force $ResultsDir }
New-Item -ItemType Directory -Force -Path (Join-Path $ResultsDir "input") | Out-Null
if ($Exe) {
    Copy-Item (Resolve-Path $Exe).Path (Join-Path $ResultsDir "input\shutup-and-run.exe")
}

$Wsb = Join-Path $env:TEMP "sar-sandbox.wsb"
$Template = Get-Content -Raw (Join-Path $Here "sar.wsb")
Set-Content -Encoding UTF8 -Path $Wsb -Value $Template.Replace("__REPO__", $Repo).Replace("__RESULTS__", $ResultsDir)

Write-Host "opening Windows Sandbox; results go to $ResultsDir"
Start-Process $Sandbox -ArgumentList "`"$Wsb`""

$Done = Join-Path $ResultsDir "done.txt"
$Deadline = (Get-Date).AddMinutes($TimeoutMinutes)
while (-not (Test-Path $Done)) {
    if ((Get-Date) -gt $Deadline) {
        Write-Host "no result after $TimeoutMinutes minutes. Look at the Sandbox window and at $ResultsDir."
        exit 3
    }
    Start-Sleep -Seconds 5
}

$Summary = Join-Path $ResultsDir "summary.txt"
Write-Host ""
Get-Content $Summary | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "logs: $ResultsDir"

# Close the Sandbox. Its process name changed between Windows builds.
Get-Process -Name "WindowsSandbox*" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue

if (Select-String -Path $Summary -Pattern "^FAIL" -Quiet) { exit 1 }
exit 0

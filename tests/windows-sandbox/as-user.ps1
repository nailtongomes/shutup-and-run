# Runs inside Windows Sandbox as the non-administrator test user.
# One line per item in out\results.txt: "PASS  name  detail" or "FAIL ...".
param([string]$Work = "C:\sar-test")

$ErrorActionPreference = "Continue"
$Out = Join-Path $Work "out"
$ResultFile = Join-Path $Out "results.txt"
$Repo = "nailtongomes/shutup-and-run"
$Asset = "shutup-and-run-x86_64-pc-windows-msvc.exe"
$Installed = Join-Path $env:ProgramData "shutup-and-run\shutup-and-run.exe"
$Task = "shutup-and-run"

function Item([bool]$Ok, [string]$Name, [string]$Detail) {
    $word = if ($Ok) { "PASS" } else { "FAIL" }
    Add-Content -Encoding ascii -Path $ResultFile -Value "$word  $Name  $Detail"
}

# A child process with its output in a log file. Redirected by the OS, not by
# PowerShell, so a line on stderr is a line in the log and not an error.
function Run([string]$Log, [string]$File, [string[]]$Arguments) {
    $p = Start-Process $File -ArgumentList $Arguments -Wait -PassThru -NoNewWindow `
        -RedirectStandardOutput (Join-Path $Out "$Log.stdout.log") `
        -RedirectStandardError (Join-Path $Out "$Log.stderr.log")
    return $p.ExitCode
}

# 0. The premise: this user is not an administrator.
$Me = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
$Admin = $Me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
Item (-not $Admin) "user-is-not-administrator" "$env:USERDOMAIN\$env:USERNAME"

# The hash the release says the .exe has.
$Expected = ""
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $Sums = (Invoke-WebRequest -UseBasicParsing "https://github.com/$Repo/releases/latest/download/SHA256SUMS").Content
    if ($Sums -is [byte[]]) { $Sums = [Text.Encoding]::ASCII.GetString($Sums) }
    foreach ($Line in ($Sums -split "`n")) {
        $Parts = $Line.Trim() -split "\s+", 2
        if ($Parts.Count -eq 2 -and $Parts[1].TrimStart("*").EndsWith($Asset)) { $Expected = $Parts[0].ToLower() }
    }
} catch {
    Add-Content -Path (Join-Path $Out "sums.log") -Value $_.Exception.Message
}
Item ($Expected -ne "") "release-sha256sums" "expected hash: $Expected"

# 1. install.ps1 twice: a new install, then an update with the .exe already
#    there and no task yet, which is where it used to abort.
$env:SHUTUP_AND_RUN_POLL_URL = ""
foreach ($Round in "install-fresh", "install-update") {
    $code = Run $Round "powershell.exe" @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $Work "install.ps1"))
    $got = ""
    if (Test-Path $Installed) { $got = (Get-FileHash -Algorithm SHA256 $Installed).Hash.ToLower() }
    $ok = ($code -eq 0) -and ($got -ne "") -and ($got -eq $Expected)
    Item $ok $Round "exit $code, installed hash: $got"
}

# 2. The logon task, by the same code as step 7 of `wsl install`.
#    -Exe on the host replaces the released .exe for this part.
$Exe = $Installed
$Given = Join-Path $Work "given.exe"
if (Test-Path $Given) { $Exe = $Given }

$code = Run "task-register" $Exe @("wsl", "task", "register")
Item ($code -eq 0) "task-register" "exit $code, with $Exe (a release before v0.2.2 has no 'wsl task': pass -Exe)"

$t = Get-ScheduledTask -TaskName $Task -ErrorAction SilentlyContinue
Item ($null -ne $t) "task-exists" "Get-ScheduledTask $Task"
if ($t) {
    $trigger = $t.Triggers[0]
    $logon = ($trigger.CimClass.CimClassName -eq "MSFT_TaskLogonTrigger") -and ($trigger.UserId -like "*$env:USERNAME")
    Item $logon "task-trigger-is-this-users-logon" "$($trigger.CimClass.CimClassName) for '$($trigger.UserId)'"
    $settings = ("$($t.Settings.ExecutionTimeLimit)" -eq "PT0S") -and (-not $t.Settings.DisallowStartIfOnBatteries) -and ("$($t.Settings.MultipleInstances)" -eq "IgnoreNew")
    Item $settings "task-settings" "limit $($t.Settings.ExecutionTimeLimit), no-battery $($t.Settings.DisallowStartIfOnBatteries), instances $($t.Settings.MultipleInstances)"
    Item ("$($t.Principal.RunLevel)" -eq "Limited") "task-is-not-elevated" "RunLevel $($t.Principal.RunLevel)"

    # It was started by `schtasks /Run`. Without WSL the supervisor stays up
    # and retries, so the process or a last run time is the evidence.
    Start-Sleep -Seconds 5
    $info = Get-ScheduledTaskInfo -TaskName $Task -ErrorAction SilentlyContinue
    $proc = Get-Process -Name "shutup-and-run" -ErrorAction SilentlyContinue
    $ran = ("$($t.State)" -eq "Running") -or ($null -ne $proc) -or ($info -and $info.LastRunTime -gt (Get-Date).AddMinutes(-10))
    Item ([bool]$ran) "task-ran" "state $((Get-ScheduledTask -TaskName $Task).State), last run $($info.LastRunTime), process $(if ($proc) { 'yes' } else { 'no' })"
}

# 3. The uninstall takes the task away. Without WSL it may exit non-zero
#    after that; what is checked is the task.
$code = Run "uninstall" $Exe @("wsl", "uninstall")
$left = Get-ScheduledTask -TaskName $Task -ErrorAction SilentlyContinue
Item ($null -eq $left) "task-gone-after-uninstall" "wsl uninstall exited $code"

exit 0

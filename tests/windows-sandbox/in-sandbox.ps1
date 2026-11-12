# Runs inside Windows Sandbox at logon, as its built-in administrator.
# It makes a local user who is NOT an administrator, runs as-user.ps1 as that
# user, and copies the result to the folder mapped from the host.
$ErrorActionPreference = "Stop"

$Results = "C:\sar-sandbox-results"
$Work = "C:\sar-test"
$Out = Join-Path $Work "out"

function Finish([string[]]$Lines) {
    Set-Content -Encoding ascii -Path (Join-Path $Results "summary.txt") -Value $Lines
    Set-Content -Encoding ascii -Path (Join-Path $Results "done.txt") -Value "done"
}

try {
    New-Item -ItemType Directory -Force -Path $Out | Out-Null
    Copy-Item "C:\sar-repo\scripts\install.ps1" $Work
    Copy-Item "C:\sar-repo\tests\windows-sandbox\as-user.ps1" $Work
    $Given = Join-Path $Results "input\shutup-and-run.exe"
    if (Test-Path $Given) { Copy-Item $Given (Join-Path $Work "given.exe") }

    # The password lives only in this throwaway VM and is never written out.
    $User = "sartest"
    $Plain = "Sar!" + [guid]::NewGuid().ToString("N")
    $Secure = ConvertTo-SecureString $Plain -AsPlainText -Force
    New-LocalUser -Name $User -Password $Secure -PasswordNeverExpires | Out-Null
    # "Users" only. Not "Administrators": that is the point of the test.
    Add-LocalGroupMember -SID "S-1-5-32-545" -Member $User
    cmd.exe /c "icacls $Work /grant ${User}:(OI)(CI)M /T >nul 2>&1"

    $Cred = New-Object System.Management.Automation.PSCredential("$env:COMPUTERNAME\$User", $Secure)
    $p = Start-Process powershell.exe -Credential $Cred -LoadUserProfile -WorkingDirectory $Work -Wait -PassThru `
        -ArgumentList "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", (Join-Path $Work "as-user.ps1"), "-Work", $Work `
        -RedirectStandardOutput (Join-Path $Out "as-user.stdout.log") `
        -RedirectStandardError (Join-Path $Out "as-user.stderr.log")

    Copy-Item (Join-Path $Out "*") $Results -Force
    $ResultFile = Join-Path $Out "results.txt"
    if (Test-Path $ResultFile) {
        $Lines = @(Get-Content $ResultFile)
        $Failed = @($Lines | Where-Object { $_ -like "FAIL*" }).Count
        $Lines += ""
        $Lines += "as-user.ps1 exited $($p.ExitCode); $Failed item(s) failed"
        Finish $Lines
    } else {
        Finish @("FAIL  harness  as-user.ps1 wrote no results (exit $($p.ExitCode)). See as-user.stderr.log.")
    }
} catch {
    Finish @("FAIL  harness  $($_.Exception.Message)")
}

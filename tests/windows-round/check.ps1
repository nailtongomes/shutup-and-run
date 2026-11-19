# The part of a Windows hand test (docs/WINDOWS-PENDING.md) that needs no
# person: run it on the PC where the worker is installed, in a PowerShell of
# the user who installed it. No administrator needed.
#
#   .\tests\windows-round\check.ps1
#   .\tests\windows-round\check.ps1 -JobName round-check
#
# It writes a Markdown table (ok / falhou / ver) to -Out and prints it. It
# changes nothing on the PC: the two `pause` calls it makes are ones the CLI
# refuses. With -JobName it also starts that managed container once
# (`managed start`; managed mode on, the container enabled) and watches
# VmmemWSL and the job's usage while it runs and after it ends.
# What is left for a person (battery, plug, full screen, sleep) is in
# docs/WINDOWS-PENDING.md. Exit code 1 when an item failed.
param(
    [string]$Out = "",
    [string]$JobName = "",
    [int]$JobTimeoutMinutes = 15
)

$ErrorActionPreference = "Continue"
$Data = Join-Path $env:ProgramData "shutup-and-run"
$Distro = "shutup-and-run"
$Page = "http://127.0.0.1:8787/"
if (-not $Out) { $Out = Join-Path (Get-Location) ("rodada-" + (Get-Date -Format "yyyyMMdd-HHmm") + ".md") }

$Results = New-Object System.Collections.Generic.List[object]
function Add-Result([string]$Item, [string]$Result, [string]$Detail) {
    $clean = ($Detail -replace '\|', '/' -replace "`r?`n", ' ').Trim()
    if ($clean.Length -gt 400) { $clean = $clean.Substring(0, 400) + "..." }
    $Results.Add([pscustomobject]@{ Item = $Item; Result = $Result; Detail = $clean })
}
function Invoke-Sar([string[]]$Words) {
    $text = (& shutup-and-run @Words 2>&1 | Out-String)
    return @{ Code = $LASTEXITCODE; Text = $text.Trim() }
}
function Invoke-Distro([string]$Script) {
    $text = (& wsl.exe -d $Distro -u root -e sh -c $Script 2>$null | Out-String)
    return $text.Trim()
}
function Get-Vmmem {
    $sum = (Get-Process -Name vmmemWSL, vmmem -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Sum).Sum
    if ($sum) { return [int64]$sum } else { return [int64]0 }
}
function Format-Size([int64]$Bytes) {
    if ($Bytes -ge 1GB) { return "{0:N1} GiB" -f ($Bytes / 1GB) }
    return "{0:N0} MiB" -f ($Bytes / 1MB)
}
function Test-Has($Object, [string]$Name) {
    return ($null -ne $Object) -and ($Object.PSObject.Properties.Name -contains $Name)
}

# Version: the .exe and the binary in the distribution.
$v = Invoke-Sar @("version")
Add-Result "versão" "ver" $v.Text

# The doctor, as an agent reads it.
$d = Invoke-Sar @("doctor", "--json")
$doctor = $null
try { $doctor = $d.Text | ConvertFrom-Json } catch { }
if ($doctor) {
    $notOk = @($doctor.checks | Where-Object { $_.status -ne "ok" } | ForEach-Object { "$($_.name)=$($_.code)" })
    if ($notOk.Count -eq 0) { Add-Result "doctor --json" "ok" "todas as linhas ok" }
    else { Add-Result "doctor --json" "ver" ($notOk -join ", ") }
    foreach ($name in @("cleanup", "wsl-disk")) {
        $line = @($doctor.checks | Where-Object { $_.name -eq $name }) | Select-Object -First 1
        if ($line) { Add-Result "doctor: $name" $(if ($line.status -eq "ok") { "ok" } else { "ver" }) "$($line.code) $($line.detail)" }
        else { Add-Result "doctor: $name" "falhou" "linha ausente" }
    }
} else {
    Add-Result "doctor --json" "falhou" "não é JSON (exit $($d.Code)): $($d.Text)"
}

# status.json: the fields of 0.2.21 and 0.2.22, and never a program name.
$statusPath = Join-Path $Data "status.json"
$raw = Get-Content -Raw -ErrorAction SilentlyContinue $statusPath
$status = $null
try { $status = $raw | ConvertFrom-Json } catch { }
if ($status) {
    $missing = @()
    foreach ($k in @("battery_percent", "utc_offset_secs", "clock_skew_secs", "metered_network")) {
        if (-not (Test-Has $status.lending $k)) { $missing += "lending.$k" }
    }
    foreach ($k in @("memory", "cleanup")) {
        if (-not (Test-Has $status $k)) { $missing += $k }
    }
    if ($missing.Count -eq 0) { Add-Result "status.json: campos" "ok" "lending.battery_percent=$($status.lending.battery_percent), utc_offset_secs=$($status.lending.utc_offset_secs), clock_skew_secs=$($status.lending.clock_skew_secs)" }
    else { Add-Result "status.json: campos" "falhou" ("faltam: " + ($missing -join ", ")) }
} else {
    Add-Result "status.json: campos" "falhou" "não lido: $statusPath"
}

# The programs list is written "chrome 3.1 GiB": look for that shape with
# the names of the programs holding the most memory now.
$names = @(Get-Process | Where-Object { $_.WorkingSet64 -gt 100MB } | Select-Object -ExpandProperty ProcessName -Unique)
$log = Get-Content -Raw -ErrorAction SilentlyContinue (Join-Path $Data "supervisor.log")
$leaks = @()
foreach ($n in $names) {
    $shape = "(?i)\b" + [regex]::Escape($n) + " \d+(\.\d+)? (MiB|GiB)"
    if ($raw -and $raw -match $shape) { $leaks += "status.json: $n" }
    if ($log -and $log -match $shape) { $leaks += "supervisor.log: $n" }
}
if ($leaks.Count -eq 0) { Add-Result "sem nomes de programa" "ok" ("procurados: " + ($names -join ", ")) }
else { Add-Result "sem nomes de programa" "falhou" ($leaks -join ", ") }

# The WSL2 clock against Windows'.
$wslNow = Invoke-Distro "date +%s"
$winNow = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
[int64]$parsed = 0
if ([int64]::TryParse($wslNow, [ref]$parsed)) {
    $diff = $parsed - $winNow
    Add-Result "relógio do WSL2" $(if ([math]::Abs($diff) -le 5) { "ok" } else { "falhou" }) "WSL2 - Windows = $diff s"
} else {
    Add-Result "relógio do WSL2" "falhou" "date no WSL2 respondeu: $wslNow"
}

# The page without JavaScript: times in the Windows zone.
$offset = [TimeZoneInfo]::Local.GetUtcOffset([DateTime]::Now)
if ($offset.Ticks -eq 0) { $zone = "UTC" }
else { $zone = "UTC" + $(if ($offset.Ticks -lt 0) { "-" } else { "+" }) + ("{0:hh\:mm}" -f $offset) }
try {
    $html = (Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 $Page).Content
    $times = @([regex]::Matches($html, '<time datetime="[^"]+">([^<]+)</time>') | ForEach-Object { $_.Groups[1].Value })
    if ($times.Count -eq 0) { Add-Result "horas da página sem JS" "ver" "nenhuma hora na página (sem tarefas recentes?)" }
    else {
        $wrong = @($times | Where-Object { -not $_.EndsWith(" $zone") })
        Add-Result "horas da página sem JS" $(if ($wrong.Count -eq 0) { "ok" } else { "falhou" }) ("esperado $zone; exemplo: " + $times[0])
    }
    Add-Result "página: linha Limpeza" $(if ($html -match "<td>(Cleanup|Limpeza)</td>") { "ok" } else { "falhou" }) ""
} catch {
    Add-Result "horas da página sem JS" "falhou" "$Page não respondeu: $($_.Exception.Message)"
}

# The CLI: two pauses it refuses, so nothing changes; status; the summary.
$until = Invoke-Sar @("pause", "--until", "24:00")
Add-Result "pause --until 24:00 recusado" $(if ($until.Code -eq 2) { "ok" } else { "falhou" }) "exit $($until.Code): $($until.Text)"
$week = Invoke-Sar @("pause", "--for", "week")
Add-Result "pause --for week recusado" $(if ($week.Code -eq 2) { "ok" } else { "falhou" }) "exit $($week.Code): $($week.Text)"
$s = Invoke-Sar @("status")
Add-Result "status" $(if ($s.Code -eq 0) { "ok" } else { "falhou" }) $s.Text
$j = Invoke-Sar @("jobs", "--summary", "--json")
$summary = $null
try { $summary = $j.Text | ConvertFrom-Json } catch { }
if ($summary -and (Test-Has $summary "images")) {
    $per = @($summary.images | ForEach-Object { "$($_.image): $($_.jobs) jobs, memory_to_ask=$($_.memory_to_ask)" })
    Add-Result "jobs --summary por imagem" "ok" ($per -join "; ")
} else {
    Add-Result "jobs --summary por imagem" "falhou" "exit $($j.Code): $($j.Text)"
}

# .wslconfig and the WSL2 disk on Windows.
$wslconfig = Join-Path $env:USERPROFILE ".wslconfig"
if (Test-Path $wslconfig) {
    $text = Get-Content -Raw $wslconfig
    Add-Result ".wslconfig: autoMemoryReclaim" $(if ($text -match "(?im)^\s*autoMemoryReclaim\s*=") { "ok" } else { "ver" }) ""
} else {
    Add-Result ".wslconfig: autoMemoryReclaim" "ver" "sem .wslconfig"
}
$vhd = Get-Item -ErrorAction SilentlyContinue (Join-Path $Data "vhd\ext4.vhdx")
if ($vhd) {
    $sparse = ($vhd.Attributes -band [IO.FileAttributes]::SparseFile) -ne 0
    $used = Invoke-Distro "df -B1 --output=used / | tail -n 1"
    $usedText = $used
    [int64]$usedBytes = 0
    if ([int64]::TryParse($used, [ref]$usedBytes)) { $usedText = Format-Size $usedBytes }
    Add-Result "disco do WSL2" "ver" ("ext4.vhdx " + (Format-Size $vhd.Length) + ", em uso no WSL2 $usedText, esparso: $sparse")
} else {
    Add-Result "disco do WSL2" "falhou" "sem $Data\vhd\ext4.vhdx"
}

# The battery: Windows' two readings and the worker's.
$battery = Get-CimInstance Win32_Battery -ErrorAction SilentlyContinue | Select-Object -First 1
if ($battery) {
    $worker = if ($status) { $status.lending.battery_percent } else { "?" }
    Add-Result "bateria" "ver" "Win32_Battery $($battery.EstimatedChargeRemaining)%, status.json battery_percent=$worker (nulo na tomada)"
}

# The network: what Windows says it costs, and what the worker read.
$cost = "?"
try {
    $null = [Windows.Networking.Connectivity.NetworkInformation, Windows.Networking.Connectivity, ContentType = WindowsRuntime]
    $netProfile = [Windows.Networking.Connectivity.NetworkInformation]::GetInternetConnectionProfile()
    if ($netProfile) { $cost = $netProfile.GetConnectionCost().NetworkCostType } else { $cost = "sem conexão" }
} catch { }
$metered = if ($status) { $status.lending.metered_network } else { "?" }
Add-Result "rede limitada" "ver" "Windows NetworkCostType=$cost (Unrestricted: não limitada), status.json metered_network=$metered"

# What the service logged that a round looks for.
if ($log) {
    $counts = @()
    foreach ($line in @("set the WSL2 clock", "gave the memory of the last job back", "removed what no job used")) {
        $counts += "'$line': " + ([regex]::Matches($log, [regex]::Escape($line))).Count
    }
    Add-Result "supervisor.log" "ver" ($counts -join "; ")
}

Add-Result "VmmemWSL agora" "ver" (Format-Size (Get-Vmmem))

# One job, when asked: usage while it runs, VmmemWSL before, during, after.
if ($JobName) {
    $before = Get-Vmmem
    $started = [DateTimeOffset]::UtcNow
    $start = Invoke-Sar @("managed", "start", $JobName)
    if ($start.Code -ne 0) {
        Add-Result "tarefa $JobName" "falhou" "managed start: exit $($start.Code): $($start.Text)"
    } else {
        $peak = $before
        $soFar = ""
        $live = ""
        $job = $null
        $deadline = (Get-Date).AddMinutes($JobTimeoutMinutes)
        while ((Get-Date) -lt $deadline) {
            Start-Sleep -Seconds 10
            $peak = [math]::Max($peak, (Get-Vmmem))
            $list = Invoke-Sar @("jobs", "--limit", "1", "--json")
            try { $job = ($list.Text | ConvertFrom-Json).jobs | Select-Object -First 1 } catch { $job = $null }
            if (-not $job -or ([DateTimeOffset]$job.started_at) -lt $started.AddSeconds(-5)) { continue }
            if ($job.status -eq "running") {
                $text = (Invoke-Sar @("jobs", "--limit", "1")).Text
                if ($text -match "used: .* so far") { $soFar = $Matches[0] }
                if (-not $live) {
                    $logs = Invoke-Sar @("jobs", "logs", $job.id, "--tail", "3", "--json")
                    try { $l = $logs.Text | ConvertFrom-Json; if ($l.live) { $live = "$($l.bytes) bytes até agora" } } catch { }
                }
            } else { break }
        }
        Add-Result "uso durante a tarefa" $(if ($soFar) { "ok" } else { "ver" }) $(if ($soFar) { $soFar } else { "sem 'so far' (a tarefa durou menos de 30 s?)" })
        Add-Result "saída durante a tarefa" $(if ($live) { "ok" } else { "ver" }) $(if ($live) { "jobs logs: $live" } else { "jobs logs não leu ao vivo (a tarefa durou menos de 10 s?)" })
        if ($job -and $job.status -ne "running") {
            Start-Sleep -Seconds 60
            $after = Get-Vmmem
            Add-Result "tarefa $JobName" "ver" ("status $($job.status); rede $($job.network_bytes) bytes; VmmemWSL antes " + (Format-Size $before) + ", pico " + (Format-Size $peak) + ", 60 s depois " + (Format-Size $after))
        } else {
            Add-Result "tarefa $JobName" "falhou" "não terminou em $JobTimeoutMinutes min"
        }
    }
}

$lines = @(
    "# Rodada automática, $(Get-Date -Format 'yyyy-MM-dd HH:mm'), $env:COMPUTERNAME",
    "",
    "| Item | Resultado | Detalhe |",
    "| --- | --- | --- |"
)
foreach ($r in $Results) { $lines += "| $($r.Item) | $($r.Result) | $($r.Detail) |" }
$lines += ""
$lines += "O que precisa de uma pessoa (tomada, bateria abaixo de 60%, tela cheia, suspensão) está em docs/WINDOWS-PENDING.md."
$lines | Set-Content -Encoding UTF8 $Out
$lines | ForEach-Object { Write-Host $_ }
Write-Host ""
Write-Host "Escrito em $Out"
if (@($Results | Where-Object { $_.Result -eq "falhou" }).Count -gt 0) { exit 1 }
exit 0

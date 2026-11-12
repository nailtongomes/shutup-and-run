@echo off
setlocal EnableExtensions
:: Fallback when shutup-and-run.exe cannot run (no WSL2). Not the product:
:: no allowlist, no slots, no HTTP API. Prefer the .exe. See docs/WINDOWS.md.
:: Downloads MAIN_SCRIPT and HELPERS, then runs the script. That is remote
:: code: use https and a host you control. There is no checksum.
:: Each URL is BASE_URL, a slash, the file name, then URL_SUFFIX.
:: A lock younger than LOCK_TTL minutes exits 2 and is left in place.
:: An older lock is stale and is replaced. A killed window keeps the lock
:: until it is stale. The lock is deleted on every exit after it was taken.
:: ============================================================
:: LAUNCHER SAR - CONFIG HERE
:: ============================================================
set "RUN_MODE=single_task"
set "BOT_TASK=TASK_NAME_ID"

set "BASE_DIR=C:\Users\Public\SAR\src"
set "BASE_URL=https://www.domain.example/files"
set "URL_SUFFIX=/0.0/0"

:: Script (.py or .pyc)
set "MAIN_SCRIPT=smoke_test.py"

:: Helpers scripts to download in same url
set "HELPERS=helpers robots"
:: helpers ext: py ou pyc
set "HELPER_EXT=py"

:: Cache (min)
set "TTL_MAIN=60"
set "TTL_HELPER=1440"

:: Lock (min)
set "LOCK_TTL=30"

:: Network
set "RETRIES=3"
set "TIMEOUT_SEC=30"

:: ============================================================
:: RUN - DO NOT TOUCH
:: ============================================================
if not exist "%BASE_DIR%" mkdir "%BASE_DIR%" || (echo [ERROR] not created %BASE_DIR% & exit /b 1)
cd /d "%BASE_DIR%"

set "LOCK_FILE=%BASE_DIR%\%BOT_TASK%.lock"
powershell -NoProfile -Command ^
 "$l=$env:LOCK_FILE; if((Test-Path $l) -and ((Get-Item $l).LastWriteTime -gt (Get-Date).AddMinutes(-[int]$env:LOCK_TTL))){exit 1}; Set-Content -Path $l -Value ((Get-Date -Format s)+' PID launcher') -Force; exit 0"
if errorlevel 1 (
    echo [LOCK] %BOT_TASK% running - %LOCK_TTL%min. Exit...
    exit /b 2
)

set "PYTHON="
where py >nul 2>&1 && set "PYTHON=py -3"
if not defined PYTHON where python >nul 2>&1 && set "PYTHON=python"
if not defined PYTHON (echo [ERROR] Python not in PATH. & del "%LOCK_FILE%" 2>nul & exit /b 1)

set "HELPER_FAIL=0"
for %%H in (%HELPERS%) do (
    call :fetch "%%H.%HELPER_EXT%" %TTL_HELPER%
    if errorlevel 1 set "HELPER_FAIL=1"
)
if "%HELPER_FAIL%"=="1" echo [WARN] some helper fail.

call :fetch "%MAIN_SCRIPT%" %TTL_MAIN%
if errorlevel 1 (
    echo [ERROR] Script not found. Aborting...
    del "%LOCK_FILE%" 2>nul
    exit /b 1
)

echo.
echo Starting %MAIN_SCRIPT% [RUN_MODE=%RUN_MODE% BOT_TASK=%BOT_TASK%] with %PYTHON%...
%PYTHON% "%BASE_DIR%\%MAIN_SCRIPT%"
set "RC=%errorlevel%"
del "%LOCK_FILE%" 2>nul
echo Finishing with code %RC%.
endlocal & exit /b %RC%

:fetch
set "_FILE=%BASE_DIR%\%~1"
set "_URL=%BASE_URL%/%~1%URL_SUFFIX%"
set "_TTL=%~2"
powershell -NoProfile -ExecutionPolicy Bypass -Command ^
 "$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue';" ^
 "[Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12;" ^
 "$f=$env:_FILE; $u=$env:_URL; $n=[IO.Path]::GetFileName($f); $t=$f+'.tmp';" ^
 "if((Test-Path $f) -and ((Get-Item $f).LastWriteTime -gt (Get-Date).AddMinutes(-[int]$env:_TTL))){Write-Host ('[CACHE] '+$n); exit 0};" ^
 "for($i=1; $i -le [int]$env:RETRIES; $i++){" ^
 "  try{" ^
 "    Invoke-WebRequest -Uri $u -OutFile $t -UseBasicParsing -TimeoutSec ([int]$env:TIMEOUT_SEC);" ^
 "    $len=(Get-Item $t).Length; if($len -eq 0){throw 'empty file'};" ^
 "    if($f -like '*.py'){ $h=Get-Content $t -TotalCount 1; if($h -match '^\s*<'){throw 'server returns HTML'} };" ^
 "    Move-Item $t $f -Force; Write-Host ('[OK] '+$n+' downloaded ('+$len+' bytes)'); exit 0" ^
 "  }catch{ Write-Host ('[WARN] '+$n+' try '+$i+': '+$_.Exception.Message); Start-Sleep -Seconds (2*$i) }" ^
 "};" ^
 "Remove-Item $t -ErrorAction SilentlyContinue;" ^
 "if(Test-Path $f){Write-Host ('[FALLBACK] using local copy -  '+$n); exit 0};" ^
 "Write-Host ('[ERROR] '+$n+' not found'); exit 1"
exit /b %errorlevel%

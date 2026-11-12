@echo off
rem shutup-and-run: is it running, and can this PC be turned off?
rem Double-click it. It starts the worker if it is stopped, then says one of:
rem   PODE DESLIGAR / YOU CAN TURN THE PC OFF
rem   NAO DESLIGUE AGORA / DO NOT TURN OFF NOW
rem "shutup-and-run.bat stop" stops the worker without asking.
setlocal
title shutup-and-run
set "EXE=%ProgramData%\shutup-and-run\shutup-and-run.exe"
set "TASK=shutup-and-run"

if not exist "%EXE%" (
  echo [NAO INSTALADO / NOT INSTALLED]  %EXE%
  goto :end
)
if /i "%~1"=="stop" goto :stop

call :running
if errorlevel 1 (
  echo Iniciando... / Starting...
  schtasks /Run /TN "%TASK%" >nul 2>&1
  timeout /t 10 /nobreak >nul
  call :running
)
if errorlevel 1 (
  echo [PARADO / STOPPED]  O servico nao iniciou. / The service did not start.
  echo Veja / See:  "%EXE%" wsl logs
  goto :end
)

set JOBS=0
for /f %%n in ('wsl.exe -d shutup-and-run -u root -e docker ps -q --filter "label=shutup-and-run=1" 2^>nul ^| find /c /v ""') do set JOBS=%%n

echo [RODANDO / RUNNING]  Tarefas agora / Jobs now: %JOBS%
if "%JOBS%"=="0" (
  echo PODE DESLIGAR O PC. / YOU CAN TURN THE PC OFF.
  goto :end
)
echo NAO DESLIGUE AGORA. / DO NOT TURN OFF NOW.
echo Espere as tarefas terminarem, ou pare o servico antes de desligar.
echo Wait for the jobs to end, or stop the service before turning off.
choice /c PS /n /m "P = parar / stop     S = sair / exit : "
if errorlevel 2 goto :end

:stop
echo Parando... / Stopping...
"%EXE%" wsl stop >nul 2>&1
echo [PARADO / STOPPED]  PODE DESLIGAR O PC. / YOU CAN TURN THE PC OFF.
goto :end

:running
tasklist /FI "IMAGENAME eq shutup-and-run.exe" 2>nul | find /i "shutup-and-run.exe" >nul
exit /b %errorlevel%

:end
echo.
if "%~1"=="" pause
endlocal

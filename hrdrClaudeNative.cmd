@echo off
setlocal EnableDelayedExpansion

REM ============================================================
REM  herdr + Claude (native)  --  terminal+agent pair
REM  ------------------------------------------------------------
REM  Launches Claude Code as a tracked "Claude" agent pane inside
REM  the herdr multiplexer, via this repo's RunClaude.cmd launcher.
REM  Claude appears as a tracked "Claude" pane in herdr's sidebar;
REM  state comes from herdr's native Claude integration hook, which
REM  is installed here on first run if it is missing.
REM
REM  Self-contained: if herdr is not on PATH it falls back to the
REM  known install dir, and if no herdr server is running it starts
REM  one, waits for it to become ready, then injects the pane and
REM  attaches this console to the herd.
REM
REM  Model override:
REM      setx CLAUDE_MODEL "claude-sonnet-5"
REM  (default: claude-opus-5 -- the latest, most capable model)
REM ============================================================

if not defined CLAUDE_MODEL set "CLAUDE_MODEL=claude-opus-5"

REM  Use this repo's RunClaude.cmd as the in-pane command so the
REM  Claude Code launch (flags, model, cwd) stays in one place.
set "CLAUDE_CMD=%~dp0RunClaude.cmd"
for %%I in ("%CLAUDE_CMD%") do set "CLAUDE_CMD=%%~fI"
if not exist "%CLAUDE_CMD%" (
    echo ERROR: could not find RunClaude.cmd at:
    echo     %CLAUDE_CMD%
    echo.
    pause
    endlocal
    exit /b 1
)

REM  Repo dir, without the trailing backslash so it survives argument
REM  quoting when passed to herdr.
set "REPO_DIR=%~dp0"
if "%REPO_DIR:~-1%"=="\" set "REPO_DIR=%REPO_DIR:~0,-1%"

REM ---- Locate herdr: PATH first, then its known install dir ----
set "HERDR="
where herdr >nul 2>nul && set "HERDR=herdr"
if not defined HERDR if exist "%LOCALAPPDATA%\Programs\Herdr\bin\herdr.exe" set "HERDR=%LOCALAPPDATA%\Programs\Herdr\bin\herdr.exe"
if not defined HERDR (
    echo ERROR: herdr was not found on PATH or at
    echo     "%LOCALAPPDATA%\Programs\Herdr\bin\herdr.exe"
    echo     Install herdr, or add its bin directory to PATH, then retry.
    echo.
    pause
    endlocal
    exit /b 1
)

REM  Ensure herdr's native Claude integration hook is installed so the
REM  pane reports Claude's state (idle/working/blocked) in the sidebar.
REM  Reinstall only when 'integration status' reports it not installed;
REM  an already-installed hook is left alone (no reinstall churn).
"%HERDR%" integration status 2>nul | findstr /I /C:"claude: not installed" >nul
if not errorlevel 1 (
    echo Installing herdr native integration: claude ...
    "%HERDR%" integration install claude
)

REM ---- Ensure a herdr server is running to host the pane ----
"%HERDR%" status server 2>nul | findstr /I /C:"status: running" >nul
if not errorlevel 1 goto :server_ready
echo No herdr server running - starting a herdr session...
start "herdr" "%HERDR%"
set "STARTED_HERDR=1"
for /l %%i in (1,1,20) do (
    "%HERDR%" status server 2>nul | findstr /I /C:"status: running" >nul
    if not errorlevel 1 (
        "%HERDR%" pane list 2>nul | findstr /I /C:"pane_id" >nul
        if not errorlevel 1 goto :server_ready
    )
    timeout /t 1 /nobreak >nul 2>&1
)
echo.
echo ERROR: herdr did not become ready in time.
echo.
pause
endlocal
exit /b 1

:server_ready
REM  A herdr agent name is unique per session: 'agent start Claude' fails once a
REM  "Claude" pane already exists. Probe with 'agent get' (exit 0 = taken) and add
REM  a numeric suffix until a free name is found, so every launch starts a fresh
REM  tracked Claude pane ("Claude", "Claude 2", "Claude 3", ...).
set "AGENT_NAME=Claude"
set "N=1"
:namecheck
"%HERDR%" agent get "!AGENT_NAME!" >nul 2>nul
if not errorlevel 1 (
    set /a N+=1
    set "AGENT_NAME=Claude !N!"
    goto :namecheck
)

REM  Launch Claude as a tracked agent pane under the free name. --cwd anchors it
REM  to this repo; --env forwards the chosen model so it is honoured even if the
REM  herdr server's captured environment predates a CLAUDE_MODEL change here.
echo Launching !AGENT_NAME! inside herdr...
"%HERDR%" agent start "!AGENT_NAME!" --cwd "%REPO_DIR%" --env CLAUDE_MODEL=%CLAUDE_MODEL% --focus -- cmd /c "%CLAUDE_CMD%"
if errorlevel 1 (
    echo.
    echo ERROR: herdr could not start the !AGENT_NAME! pane.
    echo.
    pause
    endlocal
    exit /b 1
)

REM ---- Attach this console to the herd (unless we just opened one) ----
if defined STARTED_HERDR (
    echo !AGENT_NAME! is running in the new herdr window - switch to it.
    endlocal
    exit /b 0
)
echo Attaching to the herd ^(detach with the herdr prefix, Ctrl+B then d^)...
"%HERDR%"
endlocal
exit /b 0

@echo off
setlocal EnableDelayedExpansion

REM ============================================================
REM  herdr + Claude (native)  --  terminal+agent pair
REM  ------------------------------------------------------------
REM  Launches Claude Code as a tracked, named agent inside the
REM  herdr multiplexer, via this repo's RunClaude.cmd launcher.
REM  Claude appears as a named pane in herdr's sidebar; its
REM  idle/working/blocked state comes from herdr's native Claude
REM  integration hook, which is installed here if it is missing
REM  or out of date.
REM
REM  Self-contained: if herdr is not on PATH it falls back to the
REM  known install dir, and if no herdr server is running it starts
REM  one, waits for it to become ready, then creates the tab and
REM  attaches this console to the herd.
REM
REM  Topology: a tab in the workspace labelled after this repo,
REM  creating that workspace on first run. Repeat launches add
REM  further tabs named claude, claude-2, claude-3, ...
REM
REM  Model override (forwarded to Claude as --model):
REM      setx CLAUDE_MODEL "claude-sonnet-5"
REM  (default: claude-opus-5 -- the latest, most capable model)
REM ============================================================

if not defined CLAUDE_MODEL set "CLAUDE_MODEL=claude-opus-5"
set "STARTED_HERDR="

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
REM  quoting when passed to herdr, plus its leaf name for the workspace label.
set "REPO_DIR=%~dp0"
if "%REPO_DIR:~-1%"=="\" set "REPO_DIR=%REPO_DIR:~0,-1%"
for %%I in ("%REPO_DIR%") do set "WS_LABEL=%%~nxI"

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

REM  Ensure herdr's native Claude integration hook is installed and current so
REM  the pane reports Claude's state (idle/working/blocked) in the sidebar.
REM  'integration status' prints "claude: current (vN)" only when it is up to
REM  date; anything else (not installed, outdated) means install.
"%HERDR%" integration status 2>nul | findstr /I /C:"claude: current" >nul
if errorlevel 1 (
    echo Installing herdr native integration: claude ...
    "%HERDR%" integration install claude
)

REM ---- Ensure a herdr server is running to host the tab ----
"%HERDR%" status server 2>nul | findstr /I /C:"status: running" >nul
if not errorlevel 1 goto :server_ready
echo No herdr server running - starting a herdr session...
start "herdr" "%HERDR%"
set "STARTED_HERDR=1"
set "TRIES=0"

:waitserver
"%HERDR%" status server 2>nul | findstr /I /C:"status: running" >nul
if not errorlevel 1 (
    "%HERDR%" workspace list 2>nul | findstr /I /C:"workspace_id" >nul
    if not errorlevel 1 goto :server_ready
)
set /a TRIES+=1
if !TRIES! GEQ 30 (
    echo.
    echo ERROR: herdr did not become ready in time.
    echo.
    pause
    endlocal
    exit /b 1
)
ping -n 2 127.0.0.1 >nul
goto :waitserver

:server_ready
REM  herdr agent names are lowercase, unique among live agents, and match
REM  [a-z][a-z0-9_-]{0,31}. Probe with 'agent get' (exit 0 = taken) and add a
REM  numeric suffix until a free name is found, so every launch gets a fresh
REM  tracked pane ("claude", "claude-2", "claude-3", ...).
set "AGENT_NAME=claude"
set "N=1"

:namecheck
"%HERDR%" agent get "!AGENT_NAME!" >nul 2>nul
if not errorlevel 1 (
    set /a N+=1
    set "AGENT_NAME=claude-!N!"
    goto :namecheck
)

REM  Create the pane that will host Claude: a tab in this repo's workspace,
REM  creating that workspace if this is the first launch here. herdr replies
REM  with JSON, so PowerShell does the lookup and pulls out the new IDs.
REM  HERDR / REPO_DIR / WS_LABEL are read from the environment to keep the
REM  embedded command free of nested quoting.
echo Opening a herdr pane for !AGENT_NAME! in %WS_LABEL%...
set "PANE_ID="
set "TAB_ID="
for /f "usebackq tokens=1,2" %%A in (`powershell -NoProfile -ExecutionPolicy Bypass -Command "$h=$env:HERDR; $c=$env:REPO_DIR; $l=$env:WS_LABEL; $m='CLAUDE_MODEL=' + $env:CLAUDE_MODEL; $w=$null; foreach ($x in (ConvertFrom-Json (& $h workspace list)).result.workspaces) { if ($x.label -eq $l) { $w=$x; break } }; if ($w) { $r=(ConvertFrom-Json (& $h tab create --workspace $w.workspace_id --cwd $c --label $l --env $m --focus)).result } else { $r=(ConvertFrom-Json (& $h workspace create --cwd $c --label $l --env $m --focus)).result }; ($r.root_pane.pane_id + ' ' + $r.tab.tab_id)"`) do (
    set "PANE_ID=%%A"
    set "TAB_ID=%%B"
)
if not defined PANE_ID (
    echo.
    echo ERROR: herdr could not create a pane for !AGENT_NAME!.
    echo.
    pause
    endlocal
    exit /b 1
)

REM  Start Claude in that pane. 'pane run' sends the command line plus Enter to
REM  the pane's shell; wrapping in cmd /c keeps it shell-agnostic (herdr panes
REM  may run PowerShell) and tolerant of spaces in the repo path. CLAUDE_MODEL
REM  reaches Claude through RunClaude.cmd, which turns it into --model.
"%HERDR%" pane run "!PANE_ID!" "cmd /c ""%CLAUDE_CMD%""" >nul
if errorlevel 1 (
    echo.
    echo ERROR: herdr could not start Claude in pane !PANE_ID!.
    echo.
    "%HERDR%" tab close "!TAB_ID!" >nul 2>nul
    pause
    endlocal
    exit /b 1
)

REM  Wait for herdr to recognise Claude in the pane, then give it the name we
REM  reserved. Detection arrives via the integration hook a moment after the
REM  TUI paints, so poll rather than assume.
set "TRIES=0"

:detect
"%HERDR%" agent get "!PANE_ID!" >nul 2>nul
if not errorlevel 1 goto :detected
set /a TRIES+=1
if !TRIES! GEQ 45 (
    echo.
    echo WARNING: Claude is running in pane !PANE_ID! but herdr has not
    echo          recognised it, so it stays unnamed and untracked.
    echo          Check: "%HERDR%" integration status
    echo.
    goto :attach
)
ping -n 2 127.0.0.1 >nul
goto :detect

:detected
"%HERDR%" agent rename "!PANE_ID!" "!AGENT_NAME!" >nul 2>nul
echo !AGENT_NAME! is ready in pane !PANE_ID!.

:attach
REM ---- Attach this console to the herd (unless we just opened one) ----
if defined STARTED_HERDR (
    echo Switch to the new herdr window to use it.
    endlocal
    exit /b 0
)
echo Attaching to the herd ^(detach with the herdr prefix, Ctrl+B then d^)...
"%HERDR%"
endlocal
exit /b 0

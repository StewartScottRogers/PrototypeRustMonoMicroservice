@echo off
rem ---------------------------------------------------------------------------
rem List the dead letters. Change nothing.
rem
rem   DevReplayDryRun.cmd
rem
rem Exactly DevReplay.cmd --dry-run, and it exists because those two are a
rem dangerous pair to type from memory: one of them lists messages and the other
rem moves every one of them back onto the live queue, and the difference is a
rem flag at the end of the line. Somebody deciding what to do about a
rem dead-letter queue should be able to look without being one forgotten
rem argument away from acting.
rem
rem This is the safe half. Nothing is published, nothing is consumed, and the
rem dead-letter queue is exactly as long afterwards as it was before.
rem
rem When you have read the list and fixed whatever made those messages fail,
rem DevReplay.cmd is what actually puts them back.
rem ---------------------------------------------------------------------------
setlocal

rem `call`, because running one batch file from another without it transfers
rem control and never comes back - the exit code below would never be reached.
rem
rem %* is forwarded as well, so a narrowing argument the tool grows later works
rem here too without this script needing to know about it.
call "%~dp0DevReplay.cmd" --dry-run %*

rem The exit code of the script this one wrapped, not of the `call` itself, so a
rem stopped stack or an unreachable broker still reports failure to whatever ran
rem this.
exit /b %errorlevel%

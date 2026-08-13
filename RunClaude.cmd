@echo off
setlocal
pushd "%~dp0"

REM  Optional model override, honoured both standalone and when
REM  hrdrClaudeNative.cmd injects CLAUDE_MODEL into the herdr pane:
REM      setx CLAUDE_MODEL "claude-sonnet-5"
set "MODEL_ARG="
if defined CLAUDE_MODEL set "MODEL_ARG=--model %CLAUDE_MODEL%"

call claude --dangerously-skip-permissions --verbose %MODEL_ARG% %*

popd
endlocal

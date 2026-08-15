@echo off
rem ---------------------------------------------------------------------------
rem Replay this repository's history as an animated tree, using Gource.
rem
rem   DevGource.cmd            open a live window and watch it now
rem   DevGource.cmd --video    render gource.mp4 instead of opening a window
rem
rem Anything else on the command line is handed straight to Gource, so a
rem one-off override needs no edit here:
rem
rem   DevGource.cmd --seconds-per-day 5
rem   DevGource.cmd --start-date 2026-08-14
rem
rem The settings live in gource.conf, which the Gource workflow reads as well,
rem so what renders in continuous integration matches what you see here.
rem
rem Gource itself is not installed by this script:  winget install acaudwell.Gource
rem ---------------------------------------------------------------------------
setlocal
pushd "%~dp0"

rem The history is handed to Gource on its standard input rather than letting
rem Gource run git itself. Gource can do that - point it at a directory and it
rem works out the rest - but it does so by starting a child process, and that
rem fails with a bare "Access is denied" under a terminal that has no real
rem console attached, which covers most automation. Running git here instead
rem makes the local script and the workflow do exactly the same thing, and the
rem log format below is the one Gource asks git for anyway.
rem
rem The doubled percent signs are batch-file escaping: `%%aN` reaches git as
rem `%aN`, the author name. A single `%` would be read as a variable here and
rem vanish. `%n` is a newline in git's format string, not a batch construct.
set "GITLOG=git log --pretty=format:user:%%aN%%n%%ct --reverse --raw --encoding=UTF-8 --no-renames --no-show-signature"

set "MODE=window"
set "EXTRA="

rem Walk the arguments one at a time. `shift` moves the window along, and the
rem goto-based loop avoids the parenthesised block that would expand %EXTRA%
rem before the loop ever ran.
:parse
if "%~1"=="" goto :parsed
if /i "%~1"=="--video" goto :want_video
set "EXTRA=%EXTRA% %1"
shift
goto :parse

:want_video
set "MODE=video"
shift
goto :parse

:parsed

rem Find gource.exe itself, and deliberately not the `gource` on the executable
rem search path. What the installer puts there is gource.cmd, a wrapper that
rem locates the real executable relative to its own path with %~dp0. Started
rem from inside a pipeline, that wrapper is handed a bare name as %0, so %~dp0
rem becomes the *current* directory and the wrapper looks for the executable
rem next to this repository. The failure is loud but misleading:
rem   '"Z:\repos\PrototypeRustMonoMicroservice\..\gource.exe"' is not recognized
rem Naming the executable directly sidesteps the wrapper entirely.
set "GOURCE="
if exist "%LOCALAPPDATA%\Gource\gource.exe" set "GOURCE=%LOCALAPPDATA%\Gource\gource.exe"
if not defined GOURCE if exist "%ProgramFiles%\Gource\gource.exe" set "GOURCE=%ProgramFiles%\Gource\gource.exe"
if not defined GOURCE for /f "delims=" %%g in ('where gource.exe 2^>nul') do if not defined GOURCE set "GOURCE=%%g"

if defined GOURCE goto :found_gource

echo [DevGource] Gource is not installed.
echo [DevGource] Install it with:  winget install acaudwell.Gource
goto :fail

:found_gource

if "%MODE%"=="video" goto :video

rem -----------------------------------------------------------------------
rem Live window. 1280x720 rather than full screen so it sits beside an editor.
rem Escape or the window close button ends it.
rem
rem The trailing `-` is where the log comes from: standard input.
rem -----------------------------------------------------------------------
echo [DevGource] Replaying history. Press Escape to stop.
%GITLOG% | "%GOURCE%" --load-config gource.conf --log-format git -1280x720 %EXTRA% -
if errorlevel 1 goto :fail
goto :done

:video
where ffmpeg >nul 2>&1
if errorlevel 1 (
    echo [DevGource] ffmpeg is not installed, and rendering needs it.
    echo [DevGource] Install it with:  winget install Gyan.FFmpeg
    goto :fail
)

rem -----------------------------------------------------------------------
rem Rendering. Gource writes raw frames in the Portable Pixmap format to its
rem standard output, one uncompressed frame at a time, and ffmpeg compresses
rem the stream as it arrives - nothing is ever written to disk uncompressed.
rem
rem   --stop-at-end       without it the stream never ends and ffmpeg waits
rem   --output-framerate  must match the -r given to ffmpeg, or the video
rem                       plays at the wrong speed
rem   -pix_fmt yuv420p    what players and browsers actually accept; the
rem                       default that libx264 would choose is rejected by
rem                       Windows Media Player, QuickTime and Safari
rem   -crf 23             quality, where lower is better and larger. 18 is
rem                       visually lossless, 28 is noticeably soft
rem
rem Three processes in one pipeline: git writes the history, Gource turns it
rem into frames, ffmpeg turns the frames into a file.
rem
rem 1280x720 here where the workflow renders 1920x1080, because the frames
rem cross two pipes uncompressed and that is the slow part - a 1080p frame is
rem six megabytes, and sixty of them a second is more than a Windows pipe
rem carries. Halving the pixels roughly halves the wait. Override it when the
rem video is for showing to someone:  DevGource.cmd --video -1920x1080
rem
rem If this misbehaves - a stream ffmpeg rejects, or a render that never
rem finishes - do not fight it. Run the Gource workflow from the Actions tab
rem instead and download the artifact. It renders on Linux, where the frame
rem stream is a plain byte pipe rather than one that a text-mode standard
rem output can rewrite underneath it. The live window above is the part of this
rem script that Windows is genuinely good at.
rem -----------------------------------------------------------------------
echo [DevGource] Rendering to gource.mp4. Expect a few minutes.
%GITLOG% | "%GOURCE%" --load-config gource.conf --log-format git -1280x720 --stop-at-end --output-framerate 60 --output-ppm-stream - %EXTRA% - | ffmpeg -y -loglevel warning -r 60 -f image2pipe -vcodec ppm -i - -vcodec libx264 -preset medium -pix_fmt yuv420p -crf 23 gource.mp4
if not exist "gource.mp4" goto :fail

echo [DevGource] Wrote gource.mp4
goto :done

:done
popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1

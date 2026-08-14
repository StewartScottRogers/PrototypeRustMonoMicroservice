@echo off
rem ---------------------------------------------------------------------------
rem Remove everything this stack created: containers, the network, the images
rem built from this repository, and the Postgres and Redis volumes.
rem
rem THIS DELETES DATA. Any rows in the dev database are gone afterwards.
rem
rem Pass -y to skip the confirmation, for scripted use.
rem ---------------------------------------------------------------------------
setlocal enabledelayedexpansion
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevRemove] Docker is not responding, so there is nothing to remove.
    goto :fail
)

if /i "%~1"=="-y" goto :confirmed
if /i "%~1"=="--yes" goto :confirmed

echo.
echo [DevRemove] This will delete:
echo             - all containers in this stack
echo             - the compose network
echo             - the images built from this repository
echo             - the Postgres and Redis volumes, INCLUDING THEIR DATA
echo.
echo [DevRemove] Use DevDelete.cmd instead if you want to keep the data.
echo.
set "ANSWER="
set /p "ANSWER=Type YES to continue: "
if /i not "!ANSWER!"=="YES" (
    echo [DevRemove] Cancelled. Nothing was removed.
    goto :done
)

:confirmed
echo [DevRemove] Removing containers, network, volumes, and locally built images...
rem --volumes deletes the named volumes; --rmi local deletes only images this
rem compose file built, leaving postgres and redis pulled from Docker Hub alone.
docker compose down --volumes --rmi local --remove-orphans
if errorlevel 1 goto :fail

echo [DevRemove] Done. DevStart.cmd will rebuild from scratch.

:done
popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1

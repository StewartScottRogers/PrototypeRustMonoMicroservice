@echo off
rem ---------------------------------------------------------------------------
rem Delete the containers and the compose network.
rem
rem Keeps the built images and the Postgres and Redis volumes, so the next
rem DevStart.cmd is fast and your database data is still there.
rem
rem Use DevRemove.cmd instead to delete those too.
rem ---------------------------------------------------------------------------
setlocal
pushd "%~dp0"

docker compose version >nul 2>&1
if errorlevel 1 (
    echo [DevDelete] Docker is not responding, so there is nothing to delete.
    goto :fail
)

echo [DevDelete] Removing containers and the network...
rem --remove-orphans also clears containers left behind by a service that has
rem since been renamed or deleted from compose.yaml.
docker compose down --remove-orphans
if errorlevel 1 goto :fail

echo [DevDelete] Done. Images and volumes were kept.

popd
endlocal
exit /b 0

:fail
popd
endlocal
exit /b 1

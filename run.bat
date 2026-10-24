@echo off
rem Runs the EH3D server. Builds it first if needed. Extra arguments are passed on: run.bat [--config FILE]
cd /d "%~dp0"
if not exist "target\release\eh3d-server.exe" (
    set NOPAUSE=1
    call "%~dp0build.bat"
    set NOPAUSE=
)
if not exist "target\release\eh3d-server.exe" (
    echo eh3d-server.exe was not built.
    goto :end
)
"target\release\eh3d-server.exe" %*
:end
if not defined NOPAUSE pause

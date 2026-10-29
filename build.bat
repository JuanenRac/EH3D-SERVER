@echo off
rem PRETTY_RUN_BEGIN
set "NOPAUSE=1"
if not defined PRETTY_MODE set "PRETTY_MODE=build"
set "PRETTY_PY=python"
where py >nul 2>&1 && set "PRETTY_PY=py -3"
if not defined PRETTY_RUN if exist "%~dp0tools\pretty_run.py" (
    set "PRETTY_RUN=1"
    %PRETTY_PY% "%~dp0tools\pretty_run.py" --wrap --mode %PRETTY_MODE% -- cmd /d /c "%~f0" %*
    exit /b
)
rem PRETTY_RUN_END
rem Builds the EH3D server (release). It needs EH3D-DRILLPRO and EH3D-ROUTEPRO next to this folder.
cd /d "%~dp0"
where cargo >nul 2>&1 || (echo Rust is not installed. Get it from https://rustup.rs & goto :end)
if not exist "..\EH3D-DRILLPRO" (echo EH3D-DRILLPRO must be next to this folder & goto :end)
if not exist "..\EH3D-ROUTEPRO" (echo EH3D-ROUTEPRO must be next to this folder & goto :end)
cargo build --release
:end
if not defined NOPAUSE pause

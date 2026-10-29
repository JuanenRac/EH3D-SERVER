@echo off
rem EH3D-SERVER test launcher: the tests, shown in a live panel; the version is never changed. GPL-3.0-or-later.
rem PRETTY_RUN_BEGIN
set "NOPAUSE=1"
if not defined PRETTY_MODE set "PRETTY_MODE=test"
set "PRETTY_PY=python"
where py >nul 2>&1 && set "PRETTY_PY=py -3"
if not defined PRETTY_RUN if exist "%~dp0tools\pretty_run.py" (
    set "PRETTY_RUN=1"
    %PRETTY_PY% "%~dp0tools\pretty_run.py" --wrap --mode %PRETTY_MODE% -- cmd /d /c "%~f0" %*
    exit /b
)
rem PRETTY_RUN_END
cd /d "%~dp0"
cargo test
exit /b %ERRORLEVEL%

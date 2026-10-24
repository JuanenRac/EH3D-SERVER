@echo off
rem Builds the EH3D server (release). It needs EH3D-DRILLPRO and EH3D-ROUTEPRO next to this folder.
cd /d "%~dp0"
where cargo >nul 2>&1 || (echo Rust is not installed. Get it from https://rustup.rs & goto :end)
if not exist "..\EH3D-DRILLPRO" (echo EH3D-DRILLPRO must be next to this folder & goto :end)
if not exist "..\EH3D-ROUTEPRO" (echo EH3D-ROUTEPRO must be next to this folder & goto :end)
cargo build --release
:end
if not defined NOPAUSE pause

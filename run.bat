@echo off
REM Launch Cubara (release build). Double-click in File Explorer on Windows to run
REM it, or run it from a shell. Any extra arguments pass straight through to the
REM app, so this covers every mode:
REM
REM   run.bat                       - windowed game
REM   run.bat --caps                - GPU adapter capability report
REM   run.bat --bench               - headless benchmark (default radius 12)
REM   run.bat --bench 20            - headless benchmark at a larger radius
REM   run.bat --screenshot out.png  - headless single-frame screenshot
REM
REM Windows. macOS + Linux users: double-click run.command instead.

REM Run from the repo root regardless of where the script is launched from.
cd /d "%~dp0"

cargo run --release -- %*

REM Keep the console open after the app exits so a double-click user can read the
REM output (benchmark results, an error, ...).
echo.
pause

@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
set "VCVARS64="

if exist "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat" (
  set "VCVARS64=C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
)

if not defined VCVARS64 if exist "C:\Program Files\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat" (
  set "VCVARS64=C:\Program Files\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
)

if not defined VCVARS64 (
  echo [run-tauri-dev] Could not find vcvars64.bat. Please install Visual Studio Build Tools.
  exit /b 1
)

call "%VCVARS64%"
if errorlevel 1 exit /b %errorlevel%

set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
set "COCKPIT_FORCE_MAIN_WINDOW=1"
cd /d "%SCRIPT_DIR%"

call npm run tauri dev
exit /b %errorlevel%

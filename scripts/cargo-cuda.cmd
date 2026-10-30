@echo off
rem Run cargo with the MSVC and CUDA environments loaded, for `--features cuda`.
rem nvcc needs cl.exe as its host compiler, which only a Visual Studio
rem developer environment puts on PATH.
rem
rem   scripts\cargo-cuda.cmd build --release -p ramwarden-daemon --features embed,cuda
rem   scripts\cargo-cuda.cmd test --release -p ramwarden-ml --features cuda

setlocal EnableDelayedExpansion
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" (
  echo Visual Studio Installer not found. Install Build Tools with the "Desktop development with C++" workload.
  exit /b 1
)
rem Delayed expansion: the ")" in "Program Files (x86)" would otherwise close
rem the for loop's parentheses early.
for /f "usebackq tokens=*" %%i in (`"!VSWHERE!" -latest -products * -property installationPath`) do set "VSDIR=%%i"
if not defined VSDIR (
  echo Visual Studio Build Tools not found. Install the "Desktop development with C++" workload.
  exit /b 1
)
rem vcvars64.bat itself runs vswhere by bare name.
set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%PATH%"
call "%VSDIR%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1

if not defined CUDA_PATH (
  for /d %%d in ("%ProgramFiles%\NVIDIA GPU Computing Toolkit\CUDA\v*") do set "CUDA_PATH=%%d"
)
if not defined CUDA_PATH (
  echo CUDA Toolkit not found. Install it: winget install --id Nvidia.CUDA -e
  exit /b 1
)
rem CUDA 13 keeps its runtime DLLs in bin\x64; earlier versions in bin.
set "PATH=%CUDA_PATH%\bin;%CUDA_PATH%\bin\x64;%USERPROFILE%\.cargo\bin;%PATH%"
rem RTX 50xx (Blackwell). Override for other cards, e.g. set CUDA_COMPUTE_CAP=89.
if not defined CUDA_COMPUTE_CAP set CUDA_COMPUTE_CAP=120
rem CUDA 13's CCCL headers refuse MSVC's traditional preprocessor. cl.exe
rem reads extra options from %CL%, which reaches the cl that nvcc invokes.
set "CL=/Zc:preprocessor %CL%"

cargo %*

@echo off
rem cargo with the build environment for CTranslate2 + CUDA:
rem   VS 2022 Build Tools (C++), and NVIDIA's CUDA 12.9 redistributable zips unpacked into
rem   %LOCALAPPDATA%\cuda-redist\12.9 (or set CUDA_REDIST). Only needed to build, not to run.
rem   CUDA_ARCH_LIST defaults to 8.9 (RTX 40) for development; release builds set the full list.
setlocal
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
if "%CUDA_REDIST%"=="" set "CUDA_REDIST=%LOCALAPPDATA%\cuda-redist\12.9"
rem CMake's FindCUDA chokes on backslashes
set "CUDA_PATH=%CUDA_REDIST:\=/%"
set "CUDA_TOOLKIT_ROOT_DIR=%CUDA_PATH%"
set "PATH=%CUDA_REDIST%\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin;C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;%PATH%"
set CMAKE_GENERATOR=Ninja
if "%CUDA_ARCH_LIST%"=="" set CUDA_ARCH_LIST=8.9
cargo %*

# Builds dist\StreamTranslator\StreamTranslator.exe and dist\StreamTranslator-Setup-<version>.exe.
# Run from this folder:  powershell -ExecutionPolicy Bypass -File build.ps1
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$py = ".\.venv\Scripts\python.exe"
if (-not (Test-Path $py)) { throw "No .venv here. Create it first (see README)." }

Write-Host "Build tools..." -ForegroundColor Cyan
if (Get-Command uv -ErrorAction SilentlyContinue) { uv pip install --python $py -q "pyinstaller>=6.10" pillow }
else { & $py -m pip install -q "pyinstaller>=6.10" pillow }
& $py make_icon.py

Write-Host "Building the app (a few minutes: the GPU libraries are large)..." -ForegroundColor Cyan
& $py -m PyInstaller --noconfirm --clean StreamTranslator.spec
if ($LASTEXITCODE -ne 0) { throw "PyInstaller failed" }

Write-Host "Removing unused Qt parts..." -ForegroundColor Cyan
$q = "dist\StreamTranslator\_internal\PySide6"
foreach ($f in @("opengl32sw.dll", "Qt6VirtualKeyboard.dll", "Qt6Quick3D*.dll", "Qt63D*.dll", "Qt6Charts*.dll",
                 "Qt6DataVisualization*.dll", "Qt6Graphs*.dll", "translations\qtwebengine_locales\*.pak")) {
    Get-ChildItem (Join-Path $q $f) -ErrorAction SilentlyContinue | Where-Object { $_.Name -notlike "en-US*" } |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host "Self-test of the built app..." -ForegroundColor Cyan
$st = Join-Path $env:TEMP "streamtranslator-selftest.txt"
Remove-Item $st -ErrorAction SilentlyContinue
Start-Process -Wait -FilePath "dist\StreamTranslator\StreamTranslator.exe" -ArgumentList "--selftest", "`"$st`""
Get-Content $st
if (Select-String -Path $st -Pattern "FAIL" -Quiet) { throw "Self-test failed" }

Write-Host "Building the installer..." -ForegroundColor Cyan
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
          "$env:ProgramFiles\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) {
    Write-Host "Inno Setup not found - installing it (build tool, current user only)..."
    winget install -e --id JRSoftware.InnoSetup --scope user --silent --accept-source-agreements --accept-package-agreements
    $iscc = "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe"
}
$ver = & $py -c "import sys; sys.path.insert(0, '.'); import streamtranslator; print(streamtranslator.__version__)"
Remove-Item dist\StreamTranslator-Setup*.exe -ErrorAction SilentlyContinue
& $iscc /Q "/DAppVersion=$ver" installer.iss
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }

$setup = Get-Item "dist\StreamTranslator-Setup-$ver.exe"
$app = (Get-ChildItem dist\StreamTranslator -Recurse | Measure-Object Length -Sum).Sum

# Keep only the installer: the unpacked app and PyInstaller's work folder are ~5 GB of temp files.
Remove-Item -Recurse -Force build, dist\StreamTranslator -ErrorAction SilentlyContinue

Write-Host ("`nDone: {0} ({1:N0} MB installer, {2:N0} MB installed)" -f $setup.FullName, ($setup.Length / 1MB), ($app / 1MB)) -ForegroundColor Green
Write-Host "Double-click it to install (per user, no admin). It needs FFmpeg and an NVIDIA GPU."

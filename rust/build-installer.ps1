# Builds the StreamTranslator 3 installer: dist\StreamTranslator-Setup-<version>.exe (in this rust folder).
#
#   powershell -ExecutionPolicy Bypass -File build-installer.ps1            # release build for all supported GPUs
#   powershell -ExecutionPolicy Bypass -File build-installer.ps1 -Test      # separate test app (own AppId and folder)
#
# Same installer name, AppId, folder and silent-install flags as the Python app's installer, so installed 2.x
# copies update to it with their own updater. This script only builds; release.ps1 (one folder up) publishes.
param(
    [switch]$Test,
    # NVIDIA GPU generations to compile for: GTX 10 (6.1) through RTX 50 (12.0); PTX lets newer GPUs run it too.
    # (GTX 900 / 5.2 is out: CTranslate2 4.8 has half-precision kernels that need 5.3+.)
    [string]$CudaArchs = "6.1;7.0;7.5;8.0;8.6;8.9;9.0;12.0+PTX",
    [switch]$SkipCargo,
    # where cargo builds; separate from development builds (which compile for RTX 40 only)
    [string]$Target = (Join-Path $PSScriptRoot "target-dist")
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
$target = $Target
$vendor = Join-Path $PSScriptRoot "vendor"
$stage = Join-Path $PSScriptRoot "dist\StreamTranslator"

function Fetch($url, $file, $sha) {
    if (-not (Test-Path $file) -or (Get-FileHash $file -Algorithm SHA256).Hash -ne $sha) {
        Write-Host "  downloading $url"
        New-Item -ItemType Directory -Force (Split-Path $file) | Out-Null
        Invoke-WebRequest $url -OutFile $file -UseBasicParsing
    }
    $got = (Get-FileHash $file -Algorithm SHA256).Hash
    if ($got -ne $sha) { throw "$file has the wrong checksum ($got). Not using it." }
}
function Unzip-One($zip, $inner, $to) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $z = [IO.Compression.ZipFile]::OpenRead($zip)
    try {
        $e = $z.Entries | Where-Object { $_.FullName -eq $inner } | Select-Object -First 1
        if (-not $e) { throw "$inner is not in $zip" }
        [IO.Compression.ZipFileExtensions]::ExtractToFile($e, $to, $true)
    } finally { $z.Dispose() }
}

Write-Host "Third-party files (pinned versions, checksums verified)..." -ForegroundColor Cyan
$dl = Join-Path $vendor "dl"
# ONNX Runtime 1.24.4 with DirectML (voice detection, voice isolation, speaker labels) and DirectML 1.15.4
Fetch "https://www.nuget.org/api/v2/package/Microsoft.ML.OnnxRuntime.DirectML/1.24.4" "$dl\ort.nupkg" "57E9F11B73437BEF7A309496135D4C1F96B1A8E9DDBA60013FA27BFC1D788681"
Fetch "https://www.nuget.org/api/v2/package/Microsoft.AI.DirectML/1.15.4" "$dl\dml.nupkg" "4E7CB7DDCE8CF837A7A75DC029209B520CA0101470FCDF275C1F49736A3615B9"
# yt-dlp's official folder build (starts in 0.3 s; the single-file exe unpacks itself every run). The app updates it later.
Fetch "https://github.com/yt-dlp/yt-dlp/releases/download/2026.10.26/yt-dlp_win.zip" "$dl\yt-dlp_win.zip" "FD51B212CF8380C7A52EE08C0D6BE7F3C59B2F2C832EAF7B45ACFAE6EAB57419"
# Silero VAD v6, the same file faster-whisper ships
Fetch "https://github.com/SYSTRAN/faster-whisper/raw/master/faster_whisper/assets/silero_vad_v6.onnx" "$dl\silero_vad_v6.onnx" "4CBF549B8326F60F80F2536D9EEFEB450A9ABE83365A098031C89719F1BE17D2"

if (-not $SkipCargo) {
    Write-Host "Building the app (first time: CTranslate2 for every GPU generation takes a long time)..." -ForegroundColor Cyan
    $env:CUDA_ARCH_LIST = $CudaArchs
    $env:CARGO_TARGET_DIR = $target
    & cmd /c "`"$PSScriptRoot\cargo.bat`" build --release -p st-app"
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
}
$exe = Join-Path $target "release\StreamTranslator.exe"
if (-not (Test-Path $exe)) { throw "$exe is missing" }

Write-Host "Collecting the files..." -ForegroundColor Cyan
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage, "$stage\ffmpeg", "$stage\yt-dlp" | Out-Null
Copy-Item $exe $stage
Unzip-One "$dl\ort.nupkg" "runtimes/win-x64/native/onnxruntime.dll" "$stage\onnxruntime.dll"
Unzip-One "$dl\dml.nupkg" "bin/x64-win/DirectML.dll" "$stage\DirectML.dll"
Copy-Item "$dl\silero_vad_v6.onnx" $stage
Expand-Archive "$dl\yt-dlp_win.zip" "$stage\yt-dlp" -Force
# ffmpeg: the same build the Python app bundles (winget install Gyan.FFmpeg)
$ff = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
if (-not $ff) { throw "ffmpeg not found on PATH; it gets bundled into the app (winget install Gyan.FFmpeg)" }
$link = (Get-Item $ff).Target; if ($link) { $ff = $link }
Copy-Item $ff "$stage\ffmpeg\ffmpeg.exe"
# ONNX Runtime needs the Visual C++ runtime; ship it next to the app like the Python version did
$vs = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Redist\MSVC"
$crt = Get-ChildItem $vs -Directory | Where-Object { Test-Path "$($_.FullName)\x64\Microsoft.VC143.CRT" } | Sort-Object Name | Select-Object -Last 1
if (-not $crt) { throw "Visual C++ runtime files not found under $vs" }
foreach ($f in "msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll", "vcruntime140_1.dll") {
    Copy-Item "$($crt.FullName)\x64\Microsoft.VC143.CRT\$f" $stage
}

Write-Host "Self-test of the collected app..." -ForegroundColor Cyan
$st = Join-Path $env:TEMP "streamtranslator-selftest.txt"
Remove-Item $st -ErrorAction SilentlyContinue
$env:PATH = ($env:PATH -split ';' | Where-Object { $_ -notlike '*ffmpeg*' }) -join ';'   # must find its own ffmpeg
Start-Process -Wait -FilePath "$stage\StreamTranslator.exe" -ArgumentList "--selftest", "`"$st`""
Get-Content $st
if (-not (Test-Path $st) -or (Select-String -Path $st -Pattern "^FAIL" -Quiet)) { throw "Self-test failed" }
if (-not (Select-String -Path $st -SimpleMatch "$stage\ffmpeg" -Quiet)) { throw "The app didn't use its own ffmpeg" }

Write-Host "Building the installer..." -ForegroundColor Cyan
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
          "$env:ProgramFiles\Inno Setup 6\ISCC.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found (winget install -e --id JRSoftware.InnoSetup --scope user)" }
$ver = (Select-String -Path "Cargo.toml" -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
$defs = @("/DAppVersion=$ver")
if ($Test) { $defs += "/DTestBuild=1" }
& $iscc /Q @defs "installer\StreamTranslator.iss"
if ($LASTEXITCODE -ne 0) { throw "Inno Setup failed" }

$name = if ($Test) { "StreamTranslator-Test-Setup-$ver.exe" } else { "StreamTranslator-Setup-$ver.exe" }
$setup = Get-Item "dist\$name"
$size = (Get-ChildItem $stage -Recurse | Measure-Object Length -Sum).Sum
Write-Host ("`nDone: {0} ({1:N0} MB installer, {2:N0} MB installed)" -f $setup.FullName, ($setup.Length / 1MB), ($size / 1MB)) -ForegroundColor Green

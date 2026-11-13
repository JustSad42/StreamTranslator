# Publishes a new version: sets the version, runs the tests, builds the installer, commits,
# tags, pushes to GitHub and creates a GitHub release with the installer attached.
# Installed copies see the release at their next start and offer "Update now".
#
# From 3.0.0 on this builds the Rust app in rust\ (rust\build-installer.ps1). The Python app in
# streamtranslator\ is no longer released; its build.ps1 still works for the 2.x line.
#
#   powershell -ExecutionPolicy Bypass -File release.ps1 -Version 3.0.1 -Notes "Fixed the chat translator"
#   (-Notes can also be a path to a text/markdown file, e.g. release-notes\3.0.1.md)
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [Parameter(Mandatory = $true)][string]$Notes
)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Version must look like 3.0.1" }
$tag = "v$Version"
if (git tag --list $tag) { throw "$tag already exists. Pick a higher version." }
gh auth status *> $null
if ($LASTEXITCODE -ne 0) { throw "Not logged in to GitHub. Run: gh auth login" }

Write-Host "Version $Version..." -ForegroundColor Cyan
# the first version = line in rust\Cargo.toml is [workspace.package], which every crate uses
$toml = "rust\Cargo.toml"
$re = [regex]'(?m)^version = "[^"]+"'
$re.Replace((Get-Content $toml -Raw), "version = `"$Version`"", 1) | Set-Content $toml -NoNewline

Write-Host "Tests..." -ForegroundColor Cyan
$env:CARGO_TARGET_DIR = Join-Path $PSScriptRoot "rust\target-dist"
Push-Location rust
cargo test -q -p st-core -p st-appkit
$ok = $LASTEXITCODE -eq 0
Pop-Location
Remove-Item Env:CARGO_TARGET_DIR
if (-not $ok) { throw "Tests failed. Nothing was published." }

& powershell -ExecutionPolicy Bypass -File rust\build-installer.ps1
if ($LASTEXITCODE -ne 0) { throw "Build failed. Nothing was published." }
$setup = "rust\dist\StreamTranslator-Setup-$Version.exe"
if (-not (Test-Path $setup)) { throw "$setup wasn't built." }

Write-Host "Commit, tag and push..." -ForegroundColor Cyan
git add -A
git diff --cached --quiet
if ($LASTEXITCODE -ne 0) { git commit -q -m "Version $Version" }
git tag -a $tag -m "Version $Version"
git push -q origin HEAD
git push -q origin $tag

Write-Host "GitHub release..." -ForegroundColor Cyan
$notesArgs = if (Test-Path $Notes) { @("--notes-file", $Notes) } else { @("--notes", $Notes) }
gh release create $tag $setup --title "StreamTranslator $Version" @notesArgs
if ($LASTEXITCODE -ne 0) { throw "Creating the release failed. The tag is pushed; retry with: gh release create $tag $setup" }
Write-Host "`nPublished $tag. Installed apps will offer the update next time they open." -ForegroundColor Green

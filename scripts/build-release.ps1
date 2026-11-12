# Build a release binary and package it into dist\DontreeCrab-<version>.zip
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\build-release.ps1 -SkipBuild
#
# The fonts are embedded into the executable (include_bytes!), so only the
# SoundFont has to travel next to the exe.

param([switch]$SkipBuild)

$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

if (-not $SkipBuild) {
    Write-Host "building release..."
    cargo build --release -p dontreecrab
}

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"').Matches[0].Groups[1].Value
$name = "DontreeCrab-$version"
$dist = Join-Path "dist" $name

Remove-Item -Recurse -Force $dist -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $dist | Out-Null

Copy-Item "target\release\DontreeCrab.exe" $dist
Copy-Item "README.md" $dist
Copy-Item "LICENSE" $dist
Copy-Item "THIRD_PARTY_NOTICES.md" $dist
Copy-Item "CHANGELOG.md" $dist

$localesDir = Join-Path $dist "locales"
New-Item -ItemType Directory -Force $localesDir | Out-Null
Copy-Item "locales\*.po" $localesDir
Copy-Item "locales\*.pot" $localesDir
Copy-Item "locales\README.txt" $localesDir
Write-Host "bundled translation catalogs"

$fontLicenseDir = Join-Path $dist "assets\fonts"
New-Item -ItemType Directory -Force $fontLicenseDir | Out-Null
Copy-Item "assets\fonts\*.txt" $fontLicenseDir
Copy-Item -Recurse -Force "assets\fonts\egui" $fontLicenseDir
Write-Host "bundled font licenses"

# The public package never contains a SoundFont.
$sfDir = Join-Path $dist "soundfonts"
New-Item -ItemType Directory -Force $sfDir | Out-Null
Set-Content -Path (Join-Path $sfDir "PUT-YOUR-SOUNDFONT-HERE.txt") -Value @"
Put a General MIDI SoundFont (*.sf2) in this folder.
The player picks the first .sf2 it finds here.
Suggested free fonts: GeneralUser GS, FluidR3_GM.
"@

$zip = Join-Path "dist" "$name.zip"
Remove-Item -Force $zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $dist "*") -DestinationPath $zip

Write-Host ""
Write-Host "wrote $zip"
Write-Host "to register .kar files: .\$name\DontreeCrab.exe associate"

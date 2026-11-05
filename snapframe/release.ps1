# Builds the shipped exe (whole-program LTO) and copies it to ..\release\slint\Snapframe.exe.
# Usage: powershell -File snapframe\release.ps1
$ErrorActionPreference = "Stop"
Push-Location $PSScriptRoot
try {
    cargo build --profile dist
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    $out = Join-Path $PSScriptRoot "..\release\slint"
    New-Item -ItemType Directory -Force $out | Out-Null
    Copy-Item "target\dist\snapframe.exe" (Join-Path $out "Snapframe.exe") -Force
    Write-Host "Copied to $(Resolve-Path $out)\Snapframe.exe"
} finally {
    Pop-Location
}

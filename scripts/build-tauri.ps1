$ErrorActionPreference = 'Stop'

$projectRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$manifest = Join-Path $projectRoot 'src-tauri\Cargo.toml'
$builtExecutable = Join-Path $projectRoot 'src-tauri\target\release\openshores-launcher.exe'
$distPath = Join-Path $projectRoot 'dist'
$artifact = Join-Path $distPath 'OpenShores-Launcher.exe'

if (-not (Test-Path -LiteralPath $cargo)) { throw 'Rust is not installed. Install the stable MSVC Rust toolchain first.' }

& (Join-Path $PSScriptRoot 'sync-version.ps1') | Out-Null

& $cargo build --release --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { throw "Tauri build failed with exit code $LASTEXITCODE." }
if (-not (Test-Path -LiteralPath $builtExecutable)) { throw "Tauri did not create $builtExecutable." }

[System.IO.Directory]::CreateDirectory($distPath) | Out-Null
[System.IO.File]::Copy($builtExecutable, $artifact, $true)
Write-Output $artifact

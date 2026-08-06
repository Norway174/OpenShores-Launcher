$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path -LiteralPath $cargo)) { throw 'Rust is not installed.' }
& (Join-Path $PSScriptRoot 'sync-version.ps1') | Out-Null
& $cargo run --manifest-path (Join-Path $projectRoot 'src-tauri\Cargo.toml')
exit $LASTEXITCODE

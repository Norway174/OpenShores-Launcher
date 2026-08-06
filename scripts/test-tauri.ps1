$ErrorActionPreference = 'Stop'
$projectRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (-not (Test-Path -LiteralPath $cargo)) { throw 'Rust is not installed.' }
& $cargo test --manifest-path (Join-Path $projectRoot 'src-tauri\Cargo.toml')
exit $LASTEXITCODE

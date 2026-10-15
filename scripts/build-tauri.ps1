param([string]$Notes = '')

$ErrorActionPreference = 'Stop'

$projectRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
$manifest = Join-Path $projectRoot 'src-tauri\Cargo.toml'
$builtExecutable = Join-Path $projectRoot 'src-tauri\target\release\openshores-launcher.exe'
$distPath = Join-Path $projectRoot 'dist'
$artifact = Join-Path $distPath 'OpenShores-Launcher.exe'

if (-not $env:CARGO_HOME -and (Test-Path -LiteralPath 'C:/Rust/cargo')) { $env:CARGO_HOME = 'C:/Rust/cargo' }
if (-not (Test-Path -LiteralPath $cargo)) { throw 'Rust is not installed. Install the stable MSVC Rust toolchain first.' }

# Tauri's custom-protocol feature embeds frontendDist instead of loading devUrl.
# Enabling it explicitly keeps production builds independent of the Tauri CLI.
& $cargo build --release --manifest-path $manifest --features tauri/custom-protocol
if ($LASTEXITCODE -ne 0) { throw "Tauri build failed with exit code $LASTEXITCODE." }
if (-not (Test-Path -LiteralPath $builtExecutable)) { throw "Tauri did not create $builtExecutable." }

[System.IO.Directory]::CreateDirectory($distPath) | Out-Null
[System.IO.File]::Copy($builtExecutable, $artifact, $true)
$package = Get-Content -LiteralPath (Join-Path $projectRoot 'package.json') -Raw | ConvertFrom-Json
$info = [ordered]@{
    version = $package.version
    sha256  = (Get-FileHash -LiteralPath $artifact -Algorithm SHA256).Hash.ToLower()
    size    = (Get-Item -LiteralPath $artifact).Length
    notes   = $Notes
}
$infoPath = Join-Path $distPath 'launcher.json'
[System.IO.File]::WriteAllText($infoPath, ($info | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))
Write-Output $artifact
Write-Output $infoPath

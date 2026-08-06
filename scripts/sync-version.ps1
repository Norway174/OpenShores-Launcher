$ErrorActionPreference = 'Stop'

$projectRoot = [System.IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
$packagePath = Join-Path $projectRoot 'package.json'
$cargoPath = Join-Path $projectRoot 'src-tauri\Cargo.toml'
$cargoLockPath = Join-Path $projectRoot 'src-tauri\Cargo.lock'
$tauriConfigPath = Join-Path $projectRoot 'src-tauri\tauri.conf.json'
$utf8 = [System.Text.UTF8Encoding]::new($false)

$package = Get-Content -LiteralPath $packagePath -Raw | ConvertFrom-Json
$version = [string]$package.version
if ($version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$') {
    throw "package.json contains an invalid semantic version: $version"
}

function Set-FileVersion {
    param(
        [Parameter(Mandatory)] [string] $Path,
        [Parameter(Mandatory)] [string] $Pattern,
        [Parameter(Mandatory)] [string] $Replacement
    )

    $content = [System.IO.File]::ReadAllText($Path)
    if (-not [regex]::IsMatch($content, $Pattern)) {
        throw "Could not locate version metadata in $Path"
    }
    $updated = [regex]::Replace($content, $Pattern, $Replacement, 1)
    [System.IO.File]::WriteAllText($Path, $updated, $utf8)
}

Set-FileVersion -Path $cargoPath -Pattern '(?ms)(^\[package\]\s+.*?^version\s*=\s*")[^"]+("\s*$)' -Replacement "`${1}$version`${2}"
Set-FileVersion -Path $cargoLockPath -Pattern '(?ms)(^\[\[package\]\]\s+name\s*=\s*"openshores-launcher"\s+version\s*=\s*")[^"]+("\s*$)' -Replacement "`${1}$version`${2}"
Set-FileVersion -Path $tauriConfigPath -Pattern '(?m)^(  "version": ")[^"]+(",\s*)$' -Replacement "`${1}$version`${2}"

$packageLockPath = Join-Path $projectRoot 'package-lock.json'
if (Test-Path -LiteralPath $packageLockPath) {
    Set-FileVersion -Path $packageLockPath -Pattern '(?m)^(  "version": ")[^"]+(",\s*)$' -Replacement "`${1}$version`${2}"
    Set-FileVersion -Path $packageLockPath -Pattern '(?ms)("packages":\s*\{\s*"":\s*\{\s*"name":\s*"openshores-launcher",\s*"version":\s*")[^"]+("\s*,)' -Replacement "`${1}$version`${2}"
}

Write-Output $version

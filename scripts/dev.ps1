param([switch]$Release)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'environment.ps1')
$env:CSVSCOPE_CACHE_DIR = Join-Path $workspace '.cache'
Push-Location $workspace
try {
    & (Join-Path $PSScriptRoot 'runtime.ps1')
    if ($Release) { npm run desktop:build -- --features custom-protocol }
    else { npm run desktop }
} finally { Pop-Location }

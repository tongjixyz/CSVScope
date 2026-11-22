#Requires -Version 7.0
$ErrorActionPreference = 'Stop'
$prepareWorkspace = Split-Path -Parent $PSScriptRoot
& (Join-Path $PSScriptRoot 'runtime.ps1')
Push-Location $prepareWorkspace
try {
    npm run build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally { Pop-Location }

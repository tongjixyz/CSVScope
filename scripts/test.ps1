#Requires -Version 7.0
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'environment.ps1')
Push-Location $workspace
try {
    npm run check:version
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    & (Join-Path $PSScriptRoot 'runtime.ps1')
    npm test
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    npm run build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    cargo test --workspace --features csvscope/custom-protocol --locked
    exit $LASTEXITCODE
} finally { Pop-Location }

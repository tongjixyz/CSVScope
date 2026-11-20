param([switch]$Release, [switch]$Installer)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'environment.ps1')
Push-Location $workspace
try {
    & (Join-Path $PSScriptRoot 'runtime.ps1')
    npm run build
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    if ($Installer) {
        npm run desktop:build -- --features custom-protocol -- --locked
    } elseif ($Release) {
        cargo build --workspace --features csvscope/custom-protocol --release --locked
    } else {
        cargo build --workspace --features csvscope/custom-protocol --locked
    }
    exit $LASTEXITCODE
} finally { Pop-Location }

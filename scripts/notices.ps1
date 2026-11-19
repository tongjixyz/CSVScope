#Requires -Version 7.0
param([string]$Output = 'artifacts\THIRD-PARTY-NOTICES.txt')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'environment.ps1')
Push-Location $workspace
try {
    $outputPath = [System.IO.Path]::GetFullPath($(if ([System.IO.Path]::IsPathRooted($Output)) { $Output } else { Join-Path $workspace $Output }))
    [System.IO.Directory]::CreateDirectory((Split-Path -Parent $outputPath)) | Out-Null
    $text = [System.Text.StringBuilder]::new()
    [void]$text.AppendLine('CSVScope — third-party notices')
    [void]$text.AppendLine('Generated from the locked local source packages. This inventory also includes Rust build/test dependencies; inclusion does not imply a component is shipped.')
    [void]$text.AppendLine('Microsoft MSVC runtime DLLs are proprietary redistributable components supplied under Microsoft Visual Studio redistribution terms. Their exact versions and hashes are in runtime-manifest.json.')
    [void]$text.AppendLine('Microsoft Edge WebView2 is a separate system runtime and is not included in the portable package.')
    $metadataText = & cargo metadata --locked --offline --format-version 1 --filter-platform x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw '无法读取 Cargo 锁定依赖。' }
    $metadata = ($metadataText -join "`n") | ConvertFrom-Json
    foreach ($package in ($metadata.packages | Sort-Object name,version)) {
        if (-not $package.source) { continue }
        [void]$text.AppendLine("`n========== Rust: $($package.name) $($package.version) [$($package.license)] ==========")
        $sourceDirectory = Split-Path -Parent $package.manifest_path
        foreach ($license in (Get-ChildItem -LiteralPath $sourceDirectory -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)([.\-_]|$)' } | Sort-Object Name)) {
            [void]$text.AppendLine("--- $($license.Name) ---")
            [void]$text.AppendLine([System.IO.File]::ReadAllText($license.FullName))
        }
        if ($package.name -eq 'libduckdb-sys') {
            $nativeLicense = Join-Path $sourceDirectory 'duckdb\LICENSE'
            if (Test-Path -LiteralPath $nativeLicense) {
                [void]$text.AppendLine('--- DuckDB native LICENSE ---')
                [void]$text.AppendLine([System.IO.File]::ReadAllText($nativeLicense))
            }
        }
    }
    $lock = Get-Content -LiteralPath 'package-lock.json' -Raw | ConvertFrom-Json -AsHashtable
    foreach ($entry in ($lock.packages.GetEnumerator() | Sort-Object Key)) {
        if (-not $entry.Key -or $entry.Value.dev) { continue }
        $moduleDirectory = Join-Path $workspace $entry.Key
        $packageFile = Join-Path $moduleDirectory 'package.json'
        if (-not (Test-Path -LiteralPath $packageFile)) { throw "缺少锁定的前端依赖：$($entry.Key)；请先 npm ci。" }
        $modulePackage = Get-Content -LiteralPath $packageFile -Raw | ConvertFrom-Json
        [void]$text.AppendLine("`n========== npm: $($modulePackage.name) $($modulePackage.version) [$($modulePackage.license)] ==========")
        foreach ($license in (Get-ChildItem -LiteralPath $moduleDirectory -File | Where-Object { $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)([.\-_]|$)' } | Sort-Object Name)) {
            [void]$text.AppendLine("--- $($license.Name) ---")
            [void]$text.AppendLine([System.IO.File]::ReadAllText($license.FullName))
        }
    }
    [System.IO.File]::WriteAllText($outputPath, $text.ToString(), [System.Text.UTF8Encoding]::new($false))
    Get-Item -LiteralPath $outputPath | Select-Object FullName,Length
} finally { Pop-Location }

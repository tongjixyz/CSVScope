#Requires -Version 7.0
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$packageWorkspace = Split-Path -Parent $PSScriptRoot
Push-Location $packageWorkspace
try {
    if (-not $SkipBuild) {
        & (Get-Process -Id $PID).Path -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build.ps1') -Release
        if ($LASTEXITCODE -ne 0) { throw 'Release 构建失败。' }
    }
    $version = (Get-Content -LiteralPath 'package.json' -Raw | ConvertFrom-Json).version
    $destination = Join-Path $packageWorkspace "artifacts\CSVScope-$version-windows-x64"
    [System.IO.Directory]::CreateDirectory($destination) | Out-Null
    Copy-Item -LiteralPath 'target\release\csvscope.exe' -Destination $destination
    foreach ($dll in Get-ChildItem -LiteralPath 'runtime' -File -Filter '*.dll') {
        Copy-Item -LiteralPath $dll.FullName -Destination $destination
    }
    Copy-Item -LiteralPath 'runtime\manifest.json' -Destination (Join-Path $destination 'runtime-manifest.json')
    foreach ($document in @('README.md', 'LICENSE')) {
        Copy-Item -LiteralPath $document -Destination $destination
    }
    $docsDirectory = Join-Path $destination 'docs'
    [System.IO.Directory]::CreateDirectory($docsDirectory) | Out-Null
    $sourceDocs = Join-Path $packageWorkspace 'docs'
    foreach ($document in Get-ChildItem -LiteralPath $sourceDocs -File -Recurse) {
        $documentDestination = Join-Path $docsDirectory ([System.IO.Path]::GetRelativePath($sourceDocs, $document.FullName))
        [System.IO.Directory]::CreateDirectory((Split-Path -Parent $documentDestination)) | Out-Null
        Copy-Item -LiteralPath $document.FullName -Destination $documentDestination
    }
    & (Join-Path $PSScriptRoot 'notices.ps1') -Output (Join-Path $destination 'THIRD-PARTY-NOTICES.txt')
    $quickStart = @"
CSVScope $version Preview — Windows x64

1. 解压整个文件夹，运行 csvscope.exe。请保留同目录 DLL。
2. 需要 Microsoft Edge WebView2 Runtime。若系统缺少该运行时，请先通过微软提供的安装程序安装。
3. Ctrl+O 打开 CSV；首先显示有限预览，随后完整准备。只有“完整数据已就绪”才代表全部数据可筛选。
4. 列标题右侧菜单提供联动筛选；点击确定才应用。Ctrl+Z 撤销，Ctrl+Y 重做。
5. 导出创建新文件；不会覆盖已有文件。可以选择全部列或可见列。
6. 程序正常处理本地文件不需要网络。此便携包不含 WebView2 离线安装器，也不需要开发工具。

性能实测和未完成的验收项见 docs/benchmarks.zh-CN.md 与 docs/implementation-status.zh-CN.md。
此预览版尚未在 16GB 机器和全新 Windows 环境完成验收。发行物未做代码签名。
"@
    $quickStart | Set-Content -LiteralPath (Join-Path $destination '打开说明.txt') -Encoding utf8
    $manifest = [ordered]@{
        product = 'CSVScope'; version = $version; platform = 'windows-x64'
        createdAt = [DateTimeOffset]::Now.ToString('o')
        cargoLockSha256 = (Get-FileHash -LiteralPath 'Cargo.lock' -Algorithm SHA256).Hash
        npmLockSha256 = (Get-FileHash -LiteralPath 'package-lock.json' -Algorithm SHA256).Hash
        files = @(Get-ChildItem -LiteralPath $destination -File -Recurse |
            Where-Object { $_.Name -ne 'manifest.json' } | Sort-Object FullName | ForEach-Object {
                [ordered]@{ path = [System.IO.Path]::GetRelativePath($destination, $_.FullName); bytes = $_.Length
                    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
            })
    }
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $destination 'manifest.json') -Encoding utf8
    $zipPath = "$destination.zip"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath }
    [System.IO.Compression.ZipFile]::CreateFromDirectory($destination, $zipPath, [System.IO.Compression.CompressionLevel]::Optimal, $true)
    [ordered]@{
        folder = $destination; zip = $zipPath; zipBytes = (Get-Item -LiteralPath $zipPath).Length
        sha256 = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
        uncompressedBytes = (@(Get-ChildItem -LiteralPath $destination -File -Recurse) | Measure-Object -Property Length -Sum).Sum
    } | ConvertTo-Json | Tee-Object -FilePath (Join-Path $packageWorkspace 'artifacts\portable-package.json')
} finally { Pop-Location }

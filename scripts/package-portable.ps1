#Requires -Version 7.0
param([switch]$SkipBuild, [string]$OutputDirectory = '')
$ErrorActionPreference = 'Stop'
$packageWorkspace = Split-Path -Parent $PSScriptRoot
Push-Location $packageWorkspace
try {
    if (-not $SkipBuild) {
        & (Get-Process -Id $PID).Path -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'build.ps1') -Release
        if ($LASTEXITCODE -ne 0) { throw 'Release 构建失败。' }
    }
    $version = (Get-Content -LiteralPath 'package.json' -Raw | ConvertFrom-Json).version
    $destination = if ($OutputDirectory) {
        $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
    } else { Join-Path $packageWorkspace "artifacts\CSVScope-$version-windows-x64" }
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
4. 表头下拉三角提供筛选与排序；绿色漏斗表示已筛选。Ctrl/Shift 点击表头多选列，右键隐藏/删除；点击源记录号选择行。
5. 双击或 F2 编辑完整单元格，Ctrl+Enter 应用；右键可标色。Ctrl+Z 撤销，Ctrl+Y 重做。Ctrl+S 另存为真正的 CSV，可选择全部记录或筛选结果及列范围。保存使用新文件名。
   有颜色时同时生成 .csv.csvscope.json；移动或备份 CSV 时保留该文件，以便重新打开后恢复颜色。
6. 程序正常处理本地文件不需要网络。此便携包不含 WebView2 离线安装器，也不需要开发工具。
7. 自动记录运行日志，默认位于程序旁的 logs 文件夹；右上角“打开运行日志”可直接定位。卡顿后请保留整个 logs 文件夹和大致发生时间。

性能实测和未完成的验收项见 docs/benchmarks.zh-CN.md 与 docs/implementation-status.zh-CN.md。
此预览版尚未在 16GB 机器和全新 Windows 环境完成验收。发行物未做代码签名。
"@
    $quickStart | Set-Content -LiteralPath (Join-Path $destination '打开说明.txt') -Encoding utf8
    $distributionFiles = @(Get-ChildItem -LiteralPath $destination -File -Recurse |
        Where-Object { $_.Name -ne 'manifest.json' -and
            [System.IO.Path]::GetRelativePath($destination, $_.FullName) -notmatch '^logs[\\/]' } |
        Sort-Object FullName)
    $manifest = [ordered]@{
        product = 'CSVScope'; version = $version; platform = 'windows-x64'
        createdAt = [DateTimeOffset]::Now.ToString('o')
        cargoLockSha256 = (Get-FileHash -LiteralPath 'Cargo.lock' -Algorithm SHA256).Hash
        npmLockSha256 = (Get-FileHash -LiteralPath 'package-lock.json' -Algorithm SHA256).Hash
        files = @($distributionFiles | ForEach-Object {
                [ordered]@{ path = [System.IO.Path]::GetRelativePath($destination, $_.FullName); bytes = $_.Length
                    sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
            })
    }
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $destination 'manifest.json') -Encoding utf8
    $zipPath = "$destination.zip"
    if (Test-Path -LiteralPath $zipPath) { Remove-Item -LiteralPath $zipPath }
    $archiveFiles = @($distributionFiles) + @(Get-Item -LiteralPath (Join-Path $destination 'manifest.json'))
    $archive = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        $archiveRoot = Split-Path -Leaf $destination
        foreach ($file in $archiveFiles) {
            $relative = [System.IO.Path]::GetRelativePath($destination, $file.FullName).Replace('\', '/')
            [System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName,
                "$archiveRoot/$relative", [System.IO.Compression.CompressionLevel]::Optimal) | Out-Null
        }
    } finally { $archive.Dispose() }
    [System.IO.Directory]::CreateDirectory((Join-Path $packageWorkspace 'artifacts')) | Out-Null
    [ordered]@{
        folder = $destination; zip = $zipPath; zipBytes = (Get-Item -LiteralPath $zipPath).Length
        sha256 = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
        uncompressedBytes = ($archiveFiles | Measure-Object -Property Length -Sum).Sum
    } | ConvertTo-Json | Tee-Object -FilePath (Join-Path $packageWorkspace 'artifacts\portable-package.json')
} finally { Pop-Location }

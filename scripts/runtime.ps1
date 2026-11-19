$ErrorActionPreference = 'Stop'
$runtimeWorkspace = Split-Path -Parent $PSScriptRoot
$runtimeDestination = Join-Path $runtimeWorkspace 'runtime'
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere)) { throw '未找到 Visual Studio 安装定位工具；请安装 MSVC C++ 构建工具。' }
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $installation) { throw '未找到包含 C++ 工具的 Visual Studio。' }
$redistRoot = Join-Path $installation 'VC\Redist\MSVC'
$version = Get-ChildItem -LiteralPath $redistRoot -Directory |
    Where-Object { $_.Name -match '^\d+\.\d+\.\d+$' } |
    Sort-Object { [version]$_.Name } -Descending |
    Select-Object -First 1
if (-not $version) { throw '未找到 MSVC 可再发行运行库。' }
$runtimeSource = Get-ChildItem -LiteralPath (Join-Path $version.FullName 'x64') -Directory -Filter 'Microsoft.VC*.CRT' | Select-Object -First 1
if (-not $runtimeSource) { throw '未找到 x64 CRT 可再发行目录。' }
[System.IO.Directory]::CreateDirectory($runtimeDestination) | Out-Null
$runtimeFiles = @()
foreach ($dll in Get-ChildItem -LiteralPath $runtimeSource.FullName -File -Filter '*.dll') {
    $destination = Join-Path $runtimeDestination $dll.Name
    $sha = (Get-FileHash -LiteralPath $dll.FullName -Algorithm SHA256).Hash
    if (-not (Test-Path -LiteralPath $destination) -or (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash -ne $sha) {
        Copy-Item -LiteralPath $dll.FullName -Destination $destination
    }
    $runtimeFiles += [ordered]@{ name = $dll.Name; version = $dll.VersionInfo.FileVersion; bytes = $dll.Length; sha256 = $sha }
}
[ordered]@{ source = 'MSVC x64 redistributable CRT'; version = $version.Name; files = $runtimeFiles } |
    ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $runtimeDestination 'manifest.json') -Encoding utf8

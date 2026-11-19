$ErrorActionPreference = 'Stop'
$workspace = Split-Path -Parent $PSScriptRoot
$localCargo = Join-Path $workspace '.tools\cargo\bin\cargo.exe'
if (Test-Path -LiteralPath $localCargo) {
    $env:CARGO_HOME = Join-Path $workspace '.tools\cargo'
    $env:RUSTUP_HOME = Join-Path $workspace '.tools\rustup'
    $env:PATH = (Join-Path $workspace '.tools\cargo\bin') + ';' + $env:PATH
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw '未找到 Rust。请安装 rust-toolchain.toml 指定的 stable 工具链和 MSVC C++ 构建工具。'
}

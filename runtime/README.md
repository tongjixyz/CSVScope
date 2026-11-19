此目录的 DLL 由 `scripts/runtime.ps1` 从本机 Visual Studio 的 **x64 可再发行 CRT 目录**准备，不从 `System32` 随意复制。

本次部署检查发现原生 DuckDB 依赖 `MSVCP140.dll`。发行目录把 MSVC CRT 与程序并排提供，避免要求用户另外安装开发工具。具体版本、大小及 SHA-256 记录在生成的 `manifest.json` 中。Windows 10/11 自带的 UCRT 和系统 DLL 不复制；WebView2 Runtime 单独部署。

DLL 与生成清单不提交源码仓库。构建脚本自动准备；使用原始 Cargo 命令前可先运行 `scripts/runtime.ps1`。

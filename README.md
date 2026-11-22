# CSVScope · 数览

面向 Windows 10/11 x64 的本地大文件 CSV 工具，支持多标签浏览、联动筛选、排序、编辑和导出。基于 Tauri、Rust、React 和 DuckDB，数据处理在本机完成。

项目处于预览阶段，当前功能与限制见[实现状态](docs/implementation-status.zh-CN.md)。

## 下载与运行

从 [Releases](https://github.com/tongjixyz/CSVScope/releases) 下载发行包，或在 [Actions](https://github.com/tongjixyz/CSVScope/actions) 的成功运行中下载 `csvscope-windows-x64` 构建产物。

- **便携版**：解压 ZIP，运行 `csvscope.exe`，保留同目录的运行库 DLL；系统需安装 WebView2 Runtime。
- **常规安装器**：缺少 WebView2 时会下载安装。
- **离线安装器**：包含 WebView2，适合断网部署。

运行时无需 Node、Rust 或数据库服务。文件校验值见随包提供的 `RELEASE.sha256`。

## 常用操作

| 操作 | 入口 |
| --- | --- |
| 打开多个文件 | `Ctrl+O`，支持 CSV、TSV 和文本文件 |
| 筛选与排序 | 列标题菜单，支持候选多选、搜索、文本条件及数值/日期范围 |
| 查看或编辑完整字段 | 双击单元格，完整加载后也可按 `F2` 编辑 |
| 复制选区 | `Ctrl+C` |
| 撤销 / 重做 | `Ctrl+Z` / `Ctrl+Y` 或 `Ctrl+Shift+Z` |
| 跳转记录 | 底部行号输入框，按当前筛选和排序结果定位 |
| 调整列布局 | 调整列宽、隐藏列、固定前几列 |
| 删除行列或标色 | 在表格中选择行、列或单元格，再打开右键菜单 |
| 保存结果 | `Ctrl+S` 另存为 CSV，可选择全部记录或筛选结果，以及全部保留列或可见列 |
| 查看运行日志 | 右上角“打开运行日志” |

同列多选按 OR、跨列条件按 AND 组合。首次加载和重建缓存时可以边读边筛选，完整加载后可全局排序和编辑。有标色的 CSV 另存时会生成 `.csvscope.json` 配套文件。

## 数据格式

- 支持 UTF-8、UTF-8 BOM、GBK、GB18030，以及逗号、分号和 Tab 分隔；UTF-8 另支持竖线。
- 字段默认保留原始文本，包括前导零、长编号和空格；数值与日期解释仅用于查询和排序。
- 自动识别不准确时可手动指定解析设置；非法编码、列数不一致或尺寸超限会报错停止。
- CSV 输出保留字段内容，记录换行统一为 CRLF，不保证与源文件逐字节一致。

## 开发与构建

需要 PowerShell 7、Node.js 22、项目指定的 Rust 工具链，以及 Visual Studio MSVC C++ 构建工具和 x64 运行库。在 PowerShell 7 中执行：

```powershell
npm ci
.\scripts\dev.ps1                  # 启动开发环境
.\scripts\build.ps1 -Release       # 构建独立应用
.\scripts\build.ps1 -Installer     # 构建常规安装器
.\scripts\package-portable.ps1     # 打包便携 ZIP
```

构建产物位于 `target/release/` 和 `artifacts/`。首次编译内嵌 DuckDB 耗时较长。

```powershell
.\scripts\test.ps1                   # 前端与 Rust 测试
.\scripts\desktop-smoke.ps1 -Release  # 真实桌面检查
```

## 更多文档

- [实现状态与已知限制](docs/implementation-status.zh-CN.md)
- [性能实测与测量口径](docs/benchmarks.zh-CN.md)
- [架构与设计目标](docs/architecture.zh-CN.md)
- [渐进加载与增量筛选](docs/incremental-filtering.zh-CN.md)
- [运行日志与卡顿排查](docs/diagnostics.zh-CN.md)
- [版本维护](docs/versioning.zh-CN.md)

性能目标与实测结果分别记录；开发脚本、桌面检查和基准工具见 `scripts/`。

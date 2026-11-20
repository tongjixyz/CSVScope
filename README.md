# CSVScope · 数览

面向 Windows 10/11 x64 的本地大型 CSV 查看、联动筛选、排序与导出工具。Tauri 2 / Rust / React 18 / Glide Data Grid / 原生 DuckDB；正常读取、查询与导出不访问远程服务。

当前版本为 **0.1.1 Preview**。实际运行与测量记录以 `docs/implementation-status.zh-CN.md` 和 `docs/benchmarks.zh-CN.md` 为准。设计基线在 `docs/architecture.zh-CN.md`，其中性能目标不能当成实测值。

## 运行

便携发行包解压后，运行其中的 `csvscope.exe`，并保留同目录的 MSVC 运行库 DLL。程序包含前端资源与 DuckDB，无需运行 Vite、Node、Rust 或数据库服务。需要系统已安装 Microsoft Edge WebView2 Runtime。不要只复制一个 EXE 到未安装 C++ 运行库的机器。

GitHub Release 提供便携 ZIP、常规 NSIS 安装器和离线 NSIS 安装器；普通提交的自动构建包可从对应 Actions 运行的 Artifacts 下载。本地打包结果位于 `artifacts/`，不提交到源码仓库。常规安装器在缺少 WebView2 时下载微软引导程序。离线安装器包含经过签名验证的 WebView2 x64 安装程序，可用于断网部署。已有运行时的机器正常使用本工具不需要联网。发行文件的 SHA-256 见随包提供的 `RELEASE.sha256`，干净 Windows 安装验收待测。

打开文件后先显示最多 200 条完整记录的预览，同时在后台准备完整磁盘缓存。准备期间可以输入条件，但预览不会冒充完整筛选结果。状态变为“完整数据已就绪”后，条件才在全部数据上执行。

## 运行日志

0.1.1 起自动记录运行日志，默认在程序旁的 `logs` 文件夹，右上角“打开运行日志”可直接定位。日志包括操作耗时、后台任务进度、无响应与恢复、前端错误及 Rust 异常。每个普通日志约 5 MiB，最多保留最近 10 个；程序目录不可写时会自动回退。日志与故障排查细节见 [运行日志说明](docs/diagnostics.zh-CN.md)，本版验证结果见 [0.1.1 维护记录](docs/releases/0.1.1.zh-CN.md)。

## 操作

- `Ctrl+O`：选择一个或多个 CSV / TSV / 文本文件。
- 列标题菜单：搜索完整候选集合、复选框多选、全选或取消全选、全选全部搜索命中、文本条件、数值或日期范围、清除此列。
- 当前列候选值应用其他列的筛选，排除当前列自己的条件。同列勾选为 OR，不同列为 AND。
- 候选菜单每次显示 200 项，上一页/下一页不会丢失勾选；确认才提交，取消丢弃草稿。
- 标题点击：文本升序、降序、恢复源顺序。菜单可明确选择文本、精确数值、无时区日期时间排序。
- `Ctrl+Z`：撤销。`Ctrl+Y` / `Ctrl+Shift+Z`：重做。每个标签有独立历史。
- 双击单元格：查看及复制完整字段。表格显示的长文本会截断。
- `Ctrl+C`：复制选中的单元格区域；一次最多 100,000 个单元格、8MiB，更多内容请导出。
- 底部行号输入：按筛选/排序后的结果位置跳转。记录序号列显示源逻辑记录号；引号内换行不增加记录号。
- 列布局：调整列宽、隐藏列、固定前几列。切换标签保留布局、浏览位置和当前窗口内的选区。
- 导出：选择全部原始列或可见列，遵循当前排序；导出期间冻结条件。
- 缓存管理：查看路径、占用及清理未被打开文件/任务占用的基础缓存。

无排序时保持源记录顺序；排序并列值按源记录号排序。表格的前后翻页和远跳读取已生成结果，不重新解析 CSV、不反复执行筛选，也不使用不断增大的 OFFSET。

## 解析与数据语义

- 编码：UTF-8、UTF-8 BOM、GBK、GB18030。后两者使用 `encoding_rs` 的 WHATWG 映射；不宣称 GB18030-2022 认证兼容。
- 自动探测只检查有限样本。ASCII 开头但后面才出现 GBK 字符时可能需要手工指定编码；不会在导入中途悄悄改变规则。
- 分隔符：逗号、分号、Tab；UTF-8 也支持竖线。为避免误拆 GBK 多字节字符，GBK/GB18030 不接受竖线分隔。
- 默认第一条记录是表头，可在解析设置中关闭。重复/空表头有独立内部列 ID。
- 所有字段默认保存为原始文本，保留空格、前导零、长编号及原始日期字符串。
- 空字段和带引号的空字符串都保存为空文本；空白字符串和字面值 `NULL` 是普通文本。缺失字段、额外字段均报错停止，不补齐、不跳过。
- 数值条件和排序显式采用 `DECIMAL(38,9)`：最多 29 位整数、9 位小数，不接受科学记数法、NaN 或隐式去空格，不对超过精度的值做四舍五入。
- 日期解释为 `YYYY-MM-DD` 或 `YYYY-MM-DD[ T]HH:MM:SS[.ffffff]`，无时区；仅日期按当天零点比较。带时区、歧义格式和转换失败值不命中范围，类型排序时放在末尾。
- 默认每条原始/解码记录 16MiB、字段 8MiB、2048 列。超限、非法编码或列数错误会定位并停止准备。
- 导出保证字段内容，不保证与原 CSV 的引号选择、BOM 或记录终止符逐字节相同；记录终止符统一为 CRLF，字段内换行保留。

## 构建

开发环境需要 Node（本机验证为 22.22.2）、npm、`rust-toolchain.toml` 指定的 Rust stable，以及 Visual Studio MSVC C++ 工具及其 x64 可再发行 CRT。锁定 `Cargo.lock` 与 `package-lock.json`。构建脚本从本机 Visual Studio 的 Redist 目录准备应用本地 DLL，不从其他程序安装目录抽取系统文件。

```powershell
npm ci
.\scripts\dev.ps1
```

生成自带前端的可执行文件：

```powershell
.\scripts\build.ps1 -Release
```

生成 NSIS 安装包：

```powershell
.\scripts\build.ps1 -Installer
```

生成包含本地运行库、文档、第三方许可和 SHA-256 清单的便携 ZIP（PowerShell 7）：

```powershell
.\scripts\package-portable.ps1
```

离线 WebView2 安装包：

```powershell
npm run desktop:build -- --features custom-protocol --locked --config src-tauri/tauri.offline.conf.json
```

脚本优先识别本工作区 `.tools` 中的 Rust；不存在时使用系统工具链。`.tools`、构建产物、依赖和基准数据不属于发行包。DuckDB bundled 首次 C++ 编译耗时明显，后续构建复用相同依赖产物。

## GitHub 自动构建与发布

工作流为 `.github/workflows/windows.yml`，使用 GitHub 托管的 Windows x64 环境。每次推送分支、提交 Pull Request 或在 Actions 页面手动运行时，自动安装锁定依赖、检查版本、执行前端与 Rust 测试，并生成以下文件：

- `CSVScope-<版本>-windows-x64.zip`：便携包，含 MSVC CRT、文档和许可。
- `CSVScope-<版本>-windows-x64-setup.exe`：常规安装器。
- `CSVScope-<版本>-windows-x64-offline-setup.exe`：含 WebView2 的离线安装器。
- `RELEASE.sha256` 和 `THIRD-PARTY-NOTICES.txt`：校验清单与第三方许可。

构建结果在对应 Actions 运行的 `csvscope-windows-x64` Artifact 中保留 14 天。首次原生 DuckDB 编译较慢；后续运行会尝试复用 Rust 与 npm 缓存。自动构建不运行本机桌面交互和大文件性能验收，这些仍按下方验证步骤单独执行。

多对话同时开发时，各任务使用独立工作树和分支；完成一版后提交并推送对应分支，再核对远端提交编号。具体约定见 [Git 备份与并行开发](docs/git-workflow.zh-CN.md)。

第一次上传时，先在 GitHub 创建一个空仓库（不自动添加 README、License 或 `.gitignore`），然后在本项目目录执行：

```powershell
git remote add origin https://github.com/tongjixyz/CSVScope.git
git push -u origin main
```

发布当前 `0.1.1` 版本：

```powershell
npm run check:version
git tag -a v0.1.1 -m "CSVScope 0.1.1"
git push origin v0.1.1
```

推送 `v*` 标签后，工作流在测试与打包通过后自动创建 GitHub Release，并上传上述文件。标签必须等于 `v` 加项目版本；`package.json`、`package-lock.json`、`Cargo.toml` 的工作区版本和 `src-tauri/tauri.conf.json` 必须一致。后续版本先同步修改并提交这些文件，再推送新标签。含 `-` 的版本标签（例如 `v0.2.0-beta.1`）会标记为预发布。

发布使用 GitHub 自动提供的 `GITHUB_TOKEN`，无需另配个人访问令牌；发布任务申请 `contents: write` 权限。若仓库或组织策略限制该权限，需要在 GitHub 侧允许工作流创建 Release。安装器当前不做代码签名。

## 验证

```powershell
.\scripts\test.ps1
.\scripts\desktop-smoke.ps1 -Release
```

第二个脚本只对测试启动的应用启用独立的本机 CDP 调试端口，操作真实 WebView2 表格，保存截图并统计应用与 WebView2 子进程。正常启动不启用该端口。`.\scripts\desktop-smoke.ps1 -Release -Diagnostics` 会在单独测试实例中模拟 12.5 秒前端卡顿，验证错误日志、独立卡顿监测与恢复记录。
桌面自动化和基准采样脚本使用 PowerShell 7；本机验证版本为 7.6.9。

基准工具：

```powershell
.\target\release\csvbench.exe generate .bench\example.csv 1024 12
.\scripts\benchmark.ps1 -Profile release -File .bench\example.csv -Mode open
.\scripts\benchmark.ps1 -Profile release -File .bench\example.csv -Mode exercise
.\target\release\csvbench.exe multi .bench\multi-cache .bench\multi-1.csv .bench\multi-2.csv
```

`exercise` 用于生成器数据，覆盖完整准备、完整筛选、连续修改条件、候选搜索、排序、连续页、远跳、导出数量校验与取消。CLI 内存报告包含其进程树；桌面程序应另用包含 WebView2 的完整进程树测量，二者不能混称。

不要把“删除应用缓存”称为 OS 冷缓存。脚本默认记录 OS 文件缓存状态未控制；只运行一次的结果不是 P95。

## 项目结构

| 位置 | 职责 |
| --- | --- |
| `crates/core/src/parser.rs` | 有预算的成熟 CSV 增量解析、编码与稳定源读取 |
| `crates/core/src/filter.rs` | 参数化 SQL、联动候选语义、明确的类型解释 |
| `crates/core/src/history.rs` | 轻量撤销/重做与容量 |
| `crates/core/src/store.rs` | Arrow 分块结果、记录索引、显示页与二进制 IPC |
| `crates/core/src/engine.rs` | 文件会话、缓存租约、准备、调度、查询、候选与导出 |
| `crates/core/src/model.rs` | 前后端契约及全局初始预算 |
| `src-tauri/src/main.rs` | Tauri 命令、阻塞任务适配、原始二进制响应 |
| `src/App.tsx` | 多标签、Glide 虚拟表格、按需列与页、布局与历史 |
| `src/FilterMenu.tsx` | 候选分页、搜索、草稿、规则选择与类型排序 |
| `scripts/` | 构建、回归、真实桌面检查和可复现基准 |

首版只读，不提供单元格编辑、公式、实时日志追尾或跨文件 SQL。
初始资源策略最多同时打开 16 个标签，共用一个 DuckDB 内存预算和一个重任务名额；不会给每个文件分别分配 2GiB。

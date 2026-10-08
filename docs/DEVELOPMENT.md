# 开发与构建

面向需要从源码搭建 AzurLaneWorkbook 开发环境、运行测试验证以及执行生产构建的开发者。日常使用说明见 [运行环境](RUNTIME.md)，Excel 模板定制见 [工作簿说明](WORKBOOK.md#模板布局维护)。

## 开发环境准备

项目基于 Windows 10/11 x64 开发，核心构建目标为 Windows 宿主（`x86_64-pc-windows-msvc`）与 Android Native 注入模块（`x86_64`）。

### 基础环境要求

- **PowerShell 7+**（命令行可执行 `pwsh`）
- **Git** 与 **Rustup**
- **Visual Studio**（勾选“使用 C++ 的桌面开发”工作负载及 Windows SDK）

### 版本配置与依赖管理

项目核心版本与工具锁文件定义：

| 配置文件 | 作用 |
| --- | --- |
| [`rust-toolchain.toml`](../rust-toolchain.toml) | 锁定 Rust 工具链版本、组件与 target |
| [`Cargo.lock`](../Cargo.lock) | 锁定 Rust crate 依赖版本 |
| [`build/dependencies.lock.json`](../build/dependencies.lock.json) | 锁定 CMake、Ninja、Android NDK、Platform Tools 的版本与下载摘要 |
| [`.gitmodules`](../.gitmodules) | 声明 jsmn 与 Monocypher 子模块的路径与来源，具体提交由 Git 记录 |

## 首次克隆与初始化

在终端中执行以下命令（建议包含子模块克隆）：

```powershell
# 1. 克隆仓库及子模块
git clone --recurse-submodules https://github.com/SuzuShiro-sh/AzurLaneWorkbook
cd AzurLaneWorkbook

# 2. 执行依赖与环境初始化脚本
pwsh -NoProfile -File scripts/Bootstrap-Dev.ps1
```

引导脚本会自动完成以下操作：
1. 校验宿主编译环境。
2. 检查并拉取 Git 子模块。
3. 安装锁定的 Rust 工具链及编译目标。
4. 按 `build/dependencies.lock.json` 下载并解压外部工具链（CMake / Ninja / NDK / Platform Tools）至 `.dependencies/`，并进行 SHA-256 校验。

AndKittyInjector 与 KittyMemoryEx 源码已随本仓库提供，不通过子模块下载；来源、许可证与集成方式见 [Native 第三方依赖](../native/third_party/README.md)。

下载时会显示依赖名称、版本和原始地址；连接中断或超时最多尝试 3 次，每次重试前删除未完成的下载文件。HTTP 错误、证书验证错误、磁盘写入错误和大小或 SHA-256 校验失败会直接报错。若网络仍不可用，可先检查报错中的下载地址是否能在该电脑访问，再重新运行初始化命令；已通过校验的归档会复用，无需删除 `.dependencies/`。

### 常用引导参数

```powershell
# 校验失败时自动修复重新解压
pwsh -NoProfile -File scripts/Bootstrap-Dev.ps1 -Repair

# 纯离线模式（仅使用本地已有缓存，不产生网络请求）
pwsh -NoProfile -File scripts/Bootstrap-Dev.ps1 -Offline
```

## 代码质量检查与本地测试

在仓库根目录下运行以下命令核验修改：

```powershell
# 格式检查与 Clippy 静态分析
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# 单元测试与集成测试
cargo test --workspace --all-targets --locked

# 依赖引导与构建脚本测试
pwsh -NoLogo -NoProfile -NonInteractive -File tests/powershell/ScriptStructure.Tests.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File tests/powershell/ProductionBuild.Tests.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File tests/powershell/DependencyBootstrap.Tests.ps1
```

### 独立窗口测试（无需连接模拟器）

如需预览或测试原生窗口界面，可启动 GUI Fixture 样本：

```powershell
cargo run --bin native_gui_fixture --features native-gui-test --locked
```

## 生产构建与打包交付

### 执行生产构建

```powershell
pwsh -NoProfile -File scripts/Build-Production.ps1
```

构建脚本会自动完成以下流程：
1. 分别编译 Rust 生产包（Release）与 Android x86_64 Native 注入库。
2. 组装产物，将预置的 ADB、注入工具、模板布局与默认配置嵌入程序。
3. 在候选发布目录中执行自检验证（`doctor` 与清单核验）。
4. 发布至输出目录 `target/production/`，并在 `target/rollback/` 自动备份上一版本以便回滚。

### 输出结构

最终输出的生产目录包含单文件可执行程序 `AzurLaneWorkbook.exe`。运行后会在程序同级目录生成受控配置与数据目录 `.suzushiro/`：
- `target/production/`：发布目标目录。
- `target/rollback/`：历史版本备份（默认保留最近 2 个构建版本）。

## 架构与源码导航

构建脚本的职责、目录划分、依赖关系及测试方式见 [脚本说明](../scripts/README.md)。

| 目录 / 文件 | 核心职责 |
| --- | --- |
| `src/main.rs`、`src/cli/` | CLI 启动入口、命令解析、帮助系统及查询/换装动作分发 |
| `src/interfaces/native_gui/` | 原生 GUI 窗口控制器、状态驱动与异步后台任务 |
| `src/interfaces/mcp/` | MCP 协议支持、Schema 声明、长任务与证据存储 |
| `src/application/` | 业务用例层（工作簿编排、计划解析、设置与诊断契约） |
| `src/adapters/workbook/` | Excel/OOXML 适配器（工作簿生成、数据解析与状态写回） |
| `src/adapters/device/` | 模拟器交互适配（ADB 封装、内存映射、数据采集与换装执行） |
| `src/adapters/release/`、`scripts/` | 依赖引导、资源打包、版本清单与生产组装脚本 |
| `native/` | C++ Native 注入代理、Lua 交互协议及设备测试用例 |
| `crates/` | 基础设施与底层复用库 |

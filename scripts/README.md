# 构建与维护脚本

使用 PowerShell 7，从仓库根目录运行入口脚本。公共实现按职责放在子目录中，由使用方通过 `$PSScriptRoot` 加载。

## 操作入口

| 入口 | 职责 | 前置条件与输出 |
| --- | --- | --- |
| `Bootstrap-Dev.ps1` | 准备固定版本的开发依赖、Rust 工具链和 Git 子模块 | 已安装 Git、Rustup 和 Visual Studio C++ 工具链；外部工具默认存放在 `.dependencies/` |
| `Build-Production.ps1` | 编译、组装、离线验证并替换本地生产目录 | 已完成依赖准备；默认输出 `target/production/`，旧版本保留在 `target/rollback/` |
| `Test-Native.ps1` | 编译测试运行器，在指定 Android x86_64 设备上执行 Native 测试并核验证据 | 已完成依赖准备，并提供 `-DeviceSerial`；默认构建目录为 `target/native-device-tests/` |

```powershell
pwsh -NoProfile -File scripts/Bootstrap-Dev.ps1
pwsh -NoProfile -File scripts/Build-Production.ps1
pwsh -NoProfile -File scripts/Test-Native.ps1 -DeviceSerial '<ADB 设备序列号>'
```

生产构建会保留已有设置、布局和用户数据，并在替换失败时尝试恢复旧目录。它执行发布包离线自检；Rust 回归测试和设备测试需分别运行。构建和 Native 测试只读取已安装的固定依赖，不自动安装工具。

## 公共实现

| 目录 | 文件 | 职责 |
| --- | --- | --- |
| `common/` | `Paths.ps1` | 路径边界、普通文件和目录检查、归档校验、摘要和受控删除 |
| `common/` | `Process.ps1` | 子进程输出捕获、验证用路径解析、UTF-8 文件写入 |
| `dependencies/` | `Lock.ps1` | 依赖锁解析、归档和安装完整性检查 |
| `dependencies/` | `Install.ps1` | 下载重试、缓存复用、安装与修复、安装状态记录 |
| `dependencies/` | `CacheBundle.ps1` | 锁文件约束下的离线归档导入导出 |
| `build/` | `Environment.ps1` | Visual Studio 环境、构建命令和 Release Rust 参数 |
| `build/` | `Lock.ps1` | 生产构建与 Native 测试共享的排他锁 |
| `release/` | `Publish.ps1` | 用户状态迁移、布局升级、发布事务、恢复和回滚保留 |

公共文件显式加载自身依赖，只定义函数和设置脚本错误处理策略；加载不会启动安装、构建或发布。

| 使用方 | 直接依赖 |
| --- | --- |
| `Bootstrap-Dev.ps1` | `dependencies/Install.ps1`、`dependencies/CacheBundle.ps1`、`build/Environment.ps1` |
| `Build-Production.ps1` | `dependencies/Lock.ps1`、`build/Environment.ps1`、`build/Lock.ps1`、`release/Publish.ps1` |
| `Test-Native.ps1` | `dependencies/Lock.ps1`、`build/Environment.ps1`、`build/Lock.ps1`、`common/Process.ps1` |
| `dependencies/Install.ps1`、`dependencies/CacheBundle.ps1` | `dependencies/Lock.ps1` |
| `dependencies/Lock.ps1`、`build/Environment.ps1`、`build/Lock.ps1` | `common/Paths.ps1` |
| `release/Publish.ps1` | `common/Paths.ps1`、`common/Process.ps1` |

## 配置和证据

- 工具版本、下载地址、摘要和安装位置由 `build/dependencies.lock.json` 定义；Rust 版本由 `rust-toolchain.toml` 定义。
- 初始化支持 `-Offline`、`-Repair`、`-CacheBundle` 和 `-ExportCacheBundle`。缓存包只包含锁文件中 `cache_bundle: true` 的归档；当前为 CMake 和 Ninja。NDK、Platform Tools、Rust、Cargo 依赖和子模块需另行准备。
- Native 测试默认在 `$HOME/suzushiro/scratch/azur-lane-workbook-native-tests/` 下创建证据目录，scratch 根目录需已存在。可通过 `-EvidenceDirectory` 指定仓库外尚不存在的新目录。
- Native 测试保留调用参数、工具摘要、运行器输出和逐项测试报告，并检查设备进程、临时目录和 ADB 资源的清理结果。

## 验证

```powershell
pwsh -NoLogo -NoProfile -NonInteractive -File tests/powershell/ScriptStructure.Tests.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File tests/powershell/DependencyBootstrap.Tests.ps1
pwsh -NoLogo -NoProfile -NonInteractive -File tests/powershell/ProductionBuild.Tests.ps1
```

结构测试检查脚本语法和公共文件的独立加载。其余两组测试使用测试夹具检查依赖缓存、下载异常、安装完整性、构建锁、状态迁移、发布回滚和中断恢复，不替换正式生产目录。

# 模拟器与运行环境

说明 AzurLaneWorkbook 如何发现模拟器、与游戏建立通信、持久化数据以及在异常中断后进行恢复。界面与命令用法详见 [GUI](GUI.md)、[CLI](CLI.md) 和 [MCP](MCP.md)。

## 连接前提

在运行程序前，请确认以下前提条件：

1. **操作系统**：Windows x64，当前用户对程序运行目录具有读写权限。
2. **模拟器与 Root**：使用 MuMu12 或雷电 9，实例已启动并开启 Root 权限。
3. **架构与包体**：目前随包提供 `bilibili-cn-x86_64-runtime` Profile，针对 B 站官方 Android `x86_64` 客户端（包名 `com.bilibili.azurlane`，核心模块 `libtolua.so`）。
4. **游戏就绪**：必须完成登录并进入游戏主界面。仅停留在登录窗口时无法同步。
5. **排他占用**：单个模拟器实例同一时刻只允许一个工具宿主（GUI / CLI / MCP）操作。

## 目录结构与数据持久化

程序运行所需资源和输出数据均存放在安装目录下的 `.suzushiro/` 中，不依赖终端当前路径或 AppData：

```text
AzurLaneWorkbook.exe
.suzushiro/
  manifest.json              # 发布清单与校验和
  settings.json              # 运行设置与持久化偏好
  workbook-layout.xlsx       # Excel 生成模板
  runtime/                   # ADB、Native 注入组件与 Profile
  data/
    workbooks/               # 生成的工作簿（含用户编写的换装计划）
    backups/                 # 执行前的 Excel 自动备份
    history/                 # 历史检查与执行回执（含运行状态快照）
    logs/                    # 运行日志（应用日志、运行时事件及 ADB 输出）
    resident/                # 进程代理状态与重连凭据（DPAPI 加密保护）
    temp/                    # 临时文件与静态图鉴 RPC 缓存
    cache/ship-acquisition/  # 舰船获取途径网络缓存
    mcp-tasks/               # MCP 异步长任务状态
    mcp-results/             # MCP 详细证据数据
```

- **数据保留**：`data/workbooks/` 保存了用户规划的表格，迁移或升级程序时应完整保留 `.suzushiro/` 目录。
- **日志数据**：`history/` 与 `logs/` 包含玩家账号信息、船坞与背包数据。

## 模拟器实例发现

### 实例标识规范

实例标识统一携带模拟器提供方前缀，例如 `mumu12:0`、`ldplayer:0`。

```powershell
# 查询当前检测到的模拟器实例
.\AzurLaneWorkbook.exe instances

# 指定实例查询舰船
.\AzurLaneWorkbook.exe --instance mumu12:0 ships --limit 5
```

- **单实例指定**：CLI 通过全局参数 `--instance` 指定；MCP 在参数中传入 `instance`；GUI 则在顶部下拉菜单中选择。
- **模拟器管理器绑定**：如果安装了多套相同模拟器，可在 `settings.json` 中配置 `device.manager_path` 指向目标管理器的绝对路径。

### 检测与识别机制

| 模拟器 | 核心检测手段 | 验证要素 |
| --- | --- | --- |
| MuMu12 | 调用 `MuMuManager info` | 检查 Root 权限及 Android Boot ID 与 ADB 读取值的一致性 |
| 雷电 9 | 调用 `ldconsole list2` | 核验 ADB 端口监听进程归属，确认设备启动标识 |

若 MuMu 管理器查询失败，程序会尝试读取 `.nemu` 静态配置文件展示候选（此时实例会标为未就绪）。管理器单次查询预算为 5 秒，自动发现共用 15 秒查询预算，并非整个刷新过程的硬时限。

## 核心配置与超时

常用配置项位于 `.suzushiro/settings.json`：

| 配置项 | 默认值 | 作用说明 |
| --- | --- | --- |
| `device.mode` | `auto` | 实例发现模式：`auto` 自动检测，`manual` 手动指定目标 |
| `device.manager_path` | `""` | 模拟器管理器绝对路径（用于多安装定位） |
| `device.instance` | `""` | 默认目标实例标识（如 `mumu12:0`） |
| `runtime.connect_timeout_seconds` | `30` | ADB 与网络连接等待超时（秒） |
| `runtime.startup_timeout_seconds` | `180` | 通信代理启动与数据通道建立超时（秒） |
| `runtime.unload_after_sync` | `false` | 同步完成后是否自动卸载注入代理 |
| `diagnostics.detailed` | `true` | 是否记录细粒度的运行与通信诊断日志 |

获取途径、诊断级别和同步后卸载等偏好可通过 GUI 设置面板、CLI `settings set` 或 MCP `preferences_update` 便捷调整。

## 通信代理与会话生命周期

### 代理常驻机制

程序通过向游戏进程注入轻量级 Native 代理实现。代理在进程内常驻，会话身份与运行资源匹配时，后续通过 GUI、CLI 或 MCP 查询或换装可直接复用，无需重复注入。

```powershell
# 查询代理常驻状态（不触发注入）
.\AzurLaneWorkbook.exe agent status mumu12:0

# 手动加载代理
.\AzurLaneWorkbook.exe agent inject mumu12:0

# 从游戏进程卸载代理
.\AzurLaneWorkbook.exe agent unload mumu12:0
```

- 重启模拟器或重启游戏后，代理会自动失效。
- 宿主程序退出不会卸载游戏内的常驻代理，下次打开即可恢复会话连接。

### 会话恢复与安全约束

- 重连凭据保存在 `data/resident/`，采用当前 Windows 用户绑定的 DPAPI 加密保护。
- 建立新连接时会严格校验进程 PID、启动时间、Boot ID 与模块哈希。若游戏重启或进程改变，旧会话自动失效并重新认证。

## 日志排查与离线工具

### 日志与历史回执

遇到错误或结果待确认时，优先查阅本地记录：

```powershell
# 查看历史操作记录
.\AzurLaneWorkbook.exe history
.\AzurLaneWorkbook.exe history show 001-check.json --fields report

# 查看运行日志
.\AzurLaneWorkbook.exe logs
.\AzurLaneWorkbook.exe logs show 001-app.log --tail 20 --status failed
```

- `001-app.log`：核心业务日志与错误链。
- `002-runtime.jsonl`：逐行的运行时通信事件记录。
- `003-adb.log`：隔离 ADB 服务的原始输出。
- `*-check.json` / `*-exec.json`：历史检查与实际执行的详细结构化回执。

### 离线诊断命令

```powershell
# 离线环境与依赖诊断
.\AzurLaneWorkbook.exe doctor

# 发布版本完整性校验（SHA-256 核对）
.\AzurLaneWorkbook.exe verify-release
```

- `doctor`：离线检测安装完整性、必要组件、配置文件与模板布局。
- `verify-release`：对比 `manifest.json` 校验发布清单中登记文件的完整性。

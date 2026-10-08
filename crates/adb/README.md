# suzushiro-adb

`suzushiro-adb` 用于管理随包内置的 Windows ADB 工具，提供进程环境隔离、独占服务守护（`server nodaemon`）以及定向回环连接，不依赖宿主系统的全局 ADB 配置。

## 使用与所有权模型

1. **配置注入**：调用方传入 `AdbConfig`，指定受控根目录、ADB 二进制文件相对路径、独占状态目录与日志目录。ADB 可执行文件旁必须同时具备 `AdbWinApi.dll`、`NOTICE.txt` 和 `source.properties`。
2. **日志接入**：实现 `AdbLogSink` 接口，将 ADB 服务的原始输出与运行时结构化事件写入日志文件。
3. **服务生命周期**：
   - `AdbBundle::load` 校验工具完整性与修订版本。
   - `OwnedAdbServer::start` 为单个回环 TCP 目标生成专用密钥对，启动独立的 `server nodaemon` 子进程并动态分配通信端口。
   - `connect_target` 确认设备连通状态；`run_target_checked` 与 `run_target_status` 执行定向指令。
4. **安全释放**：显式调用 `shutdown` 回收子进程并清理临时目录；发生提前退出或 Drop 时也会通过析构逻辑定向终止托管的 ADB 进程。

状态目录、资源目录与日志目录互不嵌套。同一状态目录同一时刻仅允许一个服务实例使用。

## 文件职责

- `config.rs`：目录约束与 `AdbLogSink` 日志接口。
- `bundle.rs`：ADB 二进制文件、配套动态库及修订版本号校验。
- `environment.rs`：显式进程环境变量白名单，彻底隔离宿主全局环境变量（如 `ADB_SERVER_SOCKET` 等）的干扰。
- `server.rs`：独立 ADB 守护进程的启动、所有权持有与安全停止。
- `client.rs`：绑定专属端口与目标的客户端命令封装。
- `keys.rs`：受控通信认证密钥生成与校验。
- `cleanup.rs`：持有子进程的安全回收与端口释放确认。
- `error.rs`：记录错误发生的阶段、关联路径与完整底层错误链。

## 测试

```powershell
cargo test -p suzushiro-adb
```

测试使用内置的资源样本和模拟子进程运行，不拉起宿主全局 ADB 服务。

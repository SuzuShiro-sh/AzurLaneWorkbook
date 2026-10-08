# suzushiro-host-command

`suzushiro-host-command` 提供安全的宿主命令执行抽象，支持原生 Windows 与 WSL 交叉调用场景。

## 原生 Windows 特性

- **参数结构化**：严格通过数组传递命令参数，杜绝命令行字符串拼接注入风险。
- **环境隔离策略**：支持继承父进程环境（`inherited`）或使用显式白名单隔离环境。
- **进程作业（Job Object）隔离**：默认 180 秒执行预算，命令子进程自动纳入专属 Job Object。发生超时回收或管道被后代进程占用时，精准只终止该 Job Object 内的进程树，不误伤系统其他同名程序。
- **子进程主动脱离**：支持子进程显式脱离 Job Object（用于拉起长期独立运行的后台服务）。
- **交替排空防死锁**：交替读取与排空 `stdout` 与 `stderr`，单路管道缓冲上限为 16 MiB，防止子进程缓冲区填满导致管道阻塞挂起。
- **静默无弹窗**：原生 Windows 下默认隐藏控制台子窗口。

## WSL 交叉调用支持

在 Linux / WSL 环境下构建时，通过 `pwsh.exe` 与 `ProcessStartInfo.ArgumentList` 交叉调用宿主 Windows 程序，通过异步流式读取完整输出，并在超时到达时通过 `Process.Kill(true)` 回收进程树。

## 使用示例

```rust,no_run
use suzushiro_host_command::{NativeCommandPolicy, run_native_with_policy};

let policy = NativeCommandPolicy::inherited();
let output = run_native_with_policy(
    "adb.exe",
    &["devices".to_owned()],
    "adb.devices",
    &policy,
)?;

assert_eq!(output.exit_code, 0);
# Ok::<(), Box<dyn std::error::Error>>(())
```

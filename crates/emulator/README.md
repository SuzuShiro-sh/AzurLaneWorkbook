# 模拟器适配器接入

`suzushiro-emulator` 负责 Windows 平台模拟器安装发现、厂商协议交互、实例规范化及 Root 执行分发。

```text
调用方 → suzushiro-emulator → suzushiro-host-command

src/
  adapter.rs          适配器接口契约
  instance.rs         中立实例模型与地址校验
  error.rs            错误类型定义
  registry.rs         内置厂商适配器注册
  discovery.rs        系统安装与进程检测
  endpoint.rs         TCP 端口归属判定
  process_image.rs    进程映像查询
  manager_command.rs  模拟器管理命令调用
  transport.rs        Root 命令分派与统一回执
  mumu12/             MuMu12 适配实现（命令行交互、JSON 解析、.nemu 静态读取）
  ldplayer.rs         雷电 9 适配实现（ldconsole list2 控制台协议与端口映射）
```

- **架构解耦**：`EmulatorAdapter` 向上层暴露规范化的 `EmulatorInstance`，厂商原始协议与专有数据结构封装在各子模块内部。
- **职责边界**：库本身不管理业务会话，不创建全局 ADB 服务；`RootShell` 接收调用方绑定好的目标执行闭包，仅负责参数转换与退出码处理。

## 新增模拟器适配器

1. **实现接口**：在子模块中实现 `adapter.rs` 的 `EmulatorAdapter` 特征。
2. **封装协议**：处理该厂商的管理程序调用、进程名识别、端口检测与 Root 执行参数组装，对外返回统一的 `EmulatorInstance`。
3. **注册适配器**：在 `registry.rs` 的 `ADAPTERS` 列表中登记，无需在公共调用流中添加硬编码分支。
4. **补充测试**：编写命令解析、状态提取与端口绑定的单元测试。

## 发现机制与时间预算

- **查询策略**：`discover_report` 返回已识别的可用模拟器管理器列表与警告日志。
  - `strict_hint = false` 时，路径提示作为首选，并继续遍历注册表与系统路径查找其他安装。
  - `strict_hint = true` 且提供管理器路径时，严格限定仅检测该指定安装。
- **超时预算控制**：单次模拟器查询默认预算为 5 秒，全量自动检测共用 15 秒查询预算，向底层管理器调用传递剩余预算，超时后停止后续探测；同步调用仍可能阻塞，界面调用方需在后台调度。
- **静态降级保护**：若 MuMu 管理程序（`MuMuManager`）调用超时或未就绪，支持通过读取同一安装下的 `.nemu` 静态配置文件提取实例标识与端口，此时实例状态明确标记为未启动（`running = false`），避免误判为就绪目标。

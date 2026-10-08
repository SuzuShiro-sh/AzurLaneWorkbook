# MCP 使用说明

AzurLaneWorkbook 内置提供 39 个 MCP 工具，通过标准输入输出（stdio）与客户端通信，与 CLI、GUI 共享同一安装目录下的配置、数据和通信代理。


## 客户端接入

在客户端文件中添加（具体根据客户端调整）：

```json
{
  "mcpServers": {
    "azur-lane-workbook": {
      "command": "C:\\Apps\\AzurLaneWorkbook\\AzurLaneWorkbook.exe",
      "args": ["mcp"]
    }
  }
}
```

- 进程由客户端拉起并维护生命周期，配置与数据始终定位在程序自身安装目录下的 `.suzushiro/`。
- 直接在终端运行 `.\AzurLaneWorkbook.exe mcp` 会等待 stdio 输入，查看帮助请使用 `.\AzurLaneWorkbook.exe help mcp`。
- 启动后客户端通过 `tools/list` 获取完整工具列表与参数 Schema。协议通信走标准输出，调试与启动日志输出至标准错误，业务日志保存在 `.suzushiro/data/logs/`。
- 调用游戏操作前，先通过 `instances` 工具获取可用的 `instance_id` 作为 `instance` 参数。

## 工具目录与职责

客户端在 `initialize` 时收到统一操作说明，在 `tools/list` 中获取每个工具的用途、参数来源和限制。工具名使用目录中的 `name` 原值。

最短调用流程：

- 查实时数据：`instances` → 指定 `instance` 调用 `ships`、`equipment` 等查询工具。静态图鉴查询也需要游戏运行环境。
- 直接操作装备：查询真实 ID → `equipment_actions_check` → 核对 `result.check` → 以相同 `actions`、`instance` 和 `result.plan_hash` 调用 `equipment_actions_apply`。
- 执行表格计划：`workbooks_list` → `workbook_check` → 核对步骤与消耗 → `workbook_execute`。
- 生成表格或更新获取途径：提交 `workbook_generate` / `acquisition_update` 并保存 `request_id`、`task_id` → 至少间隔 5 秒调用 `task_get` 至终态。

`ship_id` 是账号内舰船实例，`config_id` 是配置，`family_id` 是装备族，`recipe_id` 是合成配方；从相应查询结果复制，不使用显示序号代替。`target_level: 10` 表示最终强化到 +10。`actions` 数组表达最终要求，实际执行顺序由计划依赖决定。

| 类别 | 工具名称 | 主要作用 |
| --- | --- | --- |
| 游戏查询 | `ships`、`equipment` | 持有舰船状态、槽位装备、仓库库存与挂载位置 |
| 静态图鉴 | `catalog_ships`、`catalog_equipment`、`catalog_skills` | 舰船图鉴、装备属性、技能数值与 Buff 效果 |
| 资源材料 | `recipes`、`items`、`resources` | 装备配方、背包材料、物资与仓库上限 |
| 编队科技 | `fleets`、`technology` | 舰队成员与阵营科技达成状态 |
| 直接装备操作 | `equipment_actions_check`、`equipment_actions_apply` | 装备合成、装配、强化、卸下与拆解（支持 Dry-Run 校验） |
| 模拟器实例 | `instances` | 查询已连接的模拟器实例及状态 |
| 代理控制 | `agent_status`、`agent_inject`、`agent_unload` | 检查、注入或卸载游戏进程内的通信代理 |
| 工作簿管理 | `workbooks_list`、`workbook_generate`、`workbook_check`、`workbook_check_save`、`workbook_execute`、`workbook_open` | 完整的 Excel 工作簿生成、计划检查、写回、执行与打开 |
| 模板布局 | `layout_check`、`layout_upgrade`、`layout_preview` | 模板结构验证、升级与预览文件生成 |
| 偏好设置 | `settings_get`、`preferences_get`、`preferences_update` | 获取运行配置，查看与更新四项持久化偏好 |
| 获取途径 | `acquisition_update` | 在线补全或刷新舰船获取途径缓存 |
| 异步任务 | `task_get`、`task_cancel` | 查询长时间运行任务状态或发起协作取消 |
| 详细证据 | `result_get` | 结构化证据下钻读取（支持分页与路径字段筛选） |
| 历史记录 | `history_list`、`history_show` | 历史执行记录列表与详情查看 |
| 运行日志 | `logs_list`、`logs_show` | 本地日志列表与筛选读取 |
| 诊断打开 | `diagnostic_open` | 调用系统关联程序打开指定日志或历史文件 |
| 离线诊断 | `doctor`、`release_verify` | 安装完整性与发布文件 SHA-256 校验 |

## 常用查询与参数格式

JSON 参数中布尔值使用 `true` / `false`，列表使用数组。与 CLI 命令行选项的对应关系如下：

| 用途 | MCP 字段 | CLI 对应项 | 说明 |
| --- | --- | --- | --- |
| 字段筛选 | `fields: ["name", "stats.air"]` | `--fields name,stats.air` | 支持点号下钻，主键始终保留 |
| 全量字段 | `full: true` | `--full` | 与 `fields` 互斥 |
| 类别筛选 | `object_type` | `--type` | 舰船或装备类型 |
| 降序排列 | `descending: true`（配合 `sort`） | `--desc`（配合 `--sort`） | 排序方向 |
| 技能等级 | `skill_level` | `--level` | 技能图鉴等级，默认为 1 |
| 等级区间 | `level_min`、`level_max` | `--level-min`、`--level-max` | 舰船或装备强化等级范围 |
| 分页控制 | `offset`、`limit` | `--offset`、`--limit` | 分页起始与每页数量 |

### 查询调用示例

查询舰船名称与航空属性：
```json
{
  "instance": "mumu12:0",
  "name": "企业",
  "level_min": 100,
  "fields": ["name", "stats.air"],
  "limit": 20
}
```

查询指定舰船某槽位的装备详情：
```json
{
  "instance": "mumu12:0",
  "ship": 9001,
  "slot": 2,
  "fields": ["name", "weapons", "skills"]
}
```

查询当前可合成的配方及最大件数：
```json
{
  "instance": "mumu12:0",
  "available_only": true
}
```

## 直接装备操作流程

直接装备操作遵循“**先预检（Dry-Run）核对，再带 Hash 执行**”的安全模式。

### 1. 计划核验（Dry-Run）

调用 `equipment_actions_check` 提交拟执行的动作集合：

```json
{
  "instance": "mumu12:0",
  "actions": [
    {
      "action": "equip_family",
      "target": {"ship_id": 9001, "slot_index": 2},
      "family_id": 1000,
      "policy": "warehouse-compose",
      "target_level": 3
    }
  ]
}
```

返回包含计划细节与校验摘要：
- `result.plan_hash`：计划的 64 位校验哈希，用于执行时绑定版本。
- `result.check.plan.steps`：编译出的实际步骤（含自动合成、强化到目标等级、换装顺序）。
- `result.check.plan.resource_delta`：预估的物资与材料增减。
- `result.check.warnings`：相关预警或资源提示。

### 2. 绑定哈希并正式执行

核对无误后，调用 `equipment_actions_apply`。必须传入与检查时相同的 `actions`、`instance` 和返回的 `plan_hash`；若重新编译的计划摘要不一致，服务会拒绝执行。下例哈希仅为格式示意，实际调用须使用检查返回的完整 64 位摘要：

```json
{
  "instance": "mumu12:0",
  "actions": [
    {
      "action": "equip_family",
      "target": {"ship_id": 9001, "slot_index": 2},
      "family_id": 1000,
      "policy": "warehouse-compose",
      "target_level": 3
    }
  ],
  "plan_hash": "6f2e8d3b4a0123456f2e8d3b4a0123456f2e8d3b4a0123456f2e8d3b4a012345"
}
```

### 动作类型与参数规范

| `action` | 必填参数 | 说明 |
| --- | --- | --- |
| `compose` | `recipe_id`、`count` | 合成指定数量的装备 |
| `equip_family` | `family_id`、`policy`、`target_level`、`target` | 按装备族准备并装配（支持自动合成与强化） |
| `equip` | `source`、`target` | 从精确来源装配到目标槽位 |
| `unequip` | `target` | 卸下指定槽位装备 |
| `enhance` | `source`、`quantity`、`target_level` | 强化指定件数的装备到目标等级 |
| `dismantle` | `source`、`quantity` | 拆解指定数量的装备 |

- `target`：形如 `{"ship_id": 9001, "slot_index": 2}`，槽位为 1–5。
- `source`：仓库为 `{"kind": "warehouse", "config_id": 1001}`；舰上为 `{"kind": "ship_slot", "ship_id": 9002, "slot_index": 2}`。
- `policy`：`warehouse-only`（仅仓库）、`warehouse-compose`（仓库不足时合成）、`compose-only`（仅合成）。

## 工作簿操作流程

工作簿流程用于基于 Excel 计划输入列的大规模配装：

1. **生成工作簿**：调用 `workbook_generate`，传入唯一 `request_id` 提交后台异步任务，获取 `task_id` 并通过 `task_get` 等待完成。
2. **下载或查看**：通过 `workbooks_list` 获取最新文件名，使用 `workbook_open` 打开并完成编辑。
3. **核验计划**：调用 `workbook_check` 预览拟生效的改动；需要固化并写回表格检查状态时调用 `workbook_check_save`。
4. **正式应用**：调用 `workbook_execute`，并传入检查阶段获取的 `plan_hash` 执行实际变动。

执行工作簿计划的参数示例（哈希须替换为检查返回的完整摘要）：

```json
{
  "instance": "mumu12:0",
  "workbook": "fleet-001.xlsx",
  "plan_hash": "9c1a0e0000000000000000000000000000000000000000000000000000000000"
}
```

## 异步长任务（生成与更新）

生成工作簿（`workbook_generate`）与更新网络资料（`acquisition_update`）耗时较长，以异步任务形式运行：

### 1. 提交任务

调用接口时需传入自定义的唯一 `request_id`；超时或响应丢失后重试，须复用原编号及相同参数，不能用新编号重复提交：

```json
{
  "instance": "mumu12:0",
  "request_id": "gen-20261007-001"
}
```

接口立即返回初始状态：`{"task_id": "...", "request_id": "gen-20261007-001", "state": "queued"}`。

### 2. 轮询状态

使用 `task_get` 查询进度（建议轮询间隔不低于 5 秒）：

```json
{
  "request_id": "gen-20261007-001"
}
```

任务状态（`state`）：
- `running` / `queued`：执行中或排队，继续等待。
- `cancelling`：取消已请求，继续查询实际终态；已完成的操作不会撤回。
- `succeeded`：成功完成，业务结果位于 `result`。
- `incomplete`：部分完成或存在警告。
- `failed` / `cancelled`：执行失败或已取消。
- `unknown` / `interrupted`：结果未知或执行进程异常结束，先核对文件、缓存及日志，不要自动重新提交业务。

### 3. 取消任务

必要时可通过 `task_cancel` 发起停止请求（参数同 `task_get`）。

## 偏好配置与维护工具

### 偏好设置管理

四项全局偏好（GUI / CLI / MCP 共享）保存在 `.suzushiro/settings.json`。更新时先通过 `preferences_get` 读取当前值作为 `original`，以避免并发冲突：

```json
{
  "original": {
    "ship_acquisition_enabled": false,
    "acquisition_update_policy": "use_cache",
    "detailed_diagnostics": true,
    "unload_after_sync": false
  },
  "preferences": {
    "ship_acquisition_enabled": true,
    "acquisition_update_policy": "use_cache",
    "detailed_diagnostics": true,
    "unload_after_sync": false
  }
}
```

### 证据与日志下钻

- **证据获取**：使用 `result_get` 传入 `result_id`，通过 `field` 点号路径读取详情；数组分页须先用 `field` 选中数组，再传 `offset` / `limit`。
- **日志与历史**：使用 `logs_list` / `logs_show` 检索诊断日志；使用 `history_list` / `history_show` 查验过去的检查与执行记录。

普通操作的证据 ID 位于 `details.result_id`，长任务查询中位于 `result.details.result_id`。第一次调用 `result_get` 只传 `result_id`，读取 `summary` 和 `fields` 后，再选择实际存在的 `field`。数组分页默认 20 条、上限 100 条，单次响应不超过 48 KiB；对象字段不能使用数组分页。证据 ID、任务 ID 和计划摘要不能混用。

### 错误定位与恢复

- JSON-RPC 参数错误保留 `-32602`，消息指出工具、参数及修正方式。业务参数反序列化错误保留原始原因，并尽可能定位到嵌套字段或动作数组项，例如 `actions[0]`。
- 业务结果先看 `status`，失败时检查 `error.message`、`error.causes`，以及存在时的 `code`、`stage`、`context`。部分执行、保存或清理错误也可能位于 `result` 中的各阶段对象，不只检查顶层 `error`。
- `incomplete` 可能已经生成文件或执行部分操作。写操作超时或失败后先查回执、实际游戏状态和日志，再检查剩余动作；不要直接重放整批操作。
- 长任务同时检查 `result`、`diagnostic`、`persistence_error`。响应丢失后使用原 `request_id` 查询；同一次提交重发需保持编号和其他参数完全相同。
- `result_get` 读取失败会包含证据 ID、文件位置及原因。读取失败不代表原操作未发生，不应通过重复执行原操作来恢复证据。

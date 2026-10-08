# 命令行使用

AzurLaneWorkbook CLI 支持数据查询、工作簿计划检查与执行、直接装备操作及离线维护。MCP 模式参数与 `plan_hash` 协议见 [MCP 使用说明](MCP.md)。

## 启动、帮助与结果

程序运行时以自身安装目录下的 `.suzushiro/` 定位配置与数据，不受当前终端工作目录影响。

```powershell
.\AzurLaneWorkbook.exe --help
.\AzurLaneWorkbook.exe ships --help
.\AzurLaneWorkbook.exe help ships
.\AzurLaneWorkbook.exe catalog skills --help
```

无参数启动时打开 GUI；指定子命令时在控制台运行并等待完成。帮助信息为纯文本，业务输出为 UTF-8 编码的 JSON。

| 退出码 | 含义 |
| --- | --- |
| `0` | 命令执行成功 |
| `1` | 参数错误或运行失败，详情见标准错误与日志 |
| `2` | 装备操作未成功完成，需核对逐步回执与游戏实际状态 |

按下 Ctrl+C 会请求取消操作，但已发送到游戏的动作无法撤回。

## 选择实例与准备游戏

先确认[连接前提](RUNTIME.md#连接前提)，启动游戏并进入主界面，随后查询可用实例：

```powershell
.\AzurLaneWorkbook.exe instances
.\AzurLaneWorkbook.exe --instance mumu12:0 ships --fields name,level
```

实例标识来自 `instances` 返回的 `instance_id`（如 `mumu12:0`、`ldplayer:0`）。全局参数 `--instance` 位于子命令之前：

```text
AzurLaneWorkbook.exe --instance INSTANCE COMMAND [参数]
```

`--instance` 适用于游戏查询、直接装备操作以及工作簿的 `generate`、`check`、`check-save`、`execute` 命令。`agent` 命令在末尾指定实例；`open`、`workbooks`、布局、设置和离线诊断命令无需此参数。多个模拟器在线时，必须显式指定目标实例。

## 游戏查询

查询命令只读取游戏数据，不修改装备与游戏状态。

### 查询对象与 ID

| 命令 | 返回对象 | ID 含义 |
| --- | --- | --- |
| `ships` | 持有舰船、等级、槽位装备、技能与编队归属 | 舰船实例 `ship_id` |
| `equipment` | 仓库装备数量与舰船装载位置 | 包含强化等级的装备配置 `config_id` |
| `catalog ships` | 游戏内置舰船图鉴配置 | 舰船配置 ID |
| `catalog equipment` | 装备属性、挂载武器与技能配置 | 装备配置 ID |
| `catalog skills` | 技能数值与 Buff 效果 | 技能 ID（必须配合 `--ids`） |
| `recipes` | 装备合成配方及当前可合成数量 | 配方 ID |
| `items` | 背包材料与数量 | 材料 ID |
| `resources` | 物资、仓库容量占用与上限 | 全局资源，无需指定 ID |
| `fleets` | 编队成员及成员装备 | 编队 ID |
| `technology` | 阵营科技奖励及达成状态 | 舰船组 ID |

```powershell
.\AzurLaneWorkbook.exe ships --ids 9001,9002 --fields name,level,slots
.\AzurLaneWorkbook.exe equipment --ids 1001 --full
.\AzurLaneWorkbook.exe catalog ships --name 企业
.\AzurLaneWorkbook.exe catalog equipment --ids 1001 --fields name,stats,weapons,skills
.\AzurLaneWorkbook.exe catalog skills --ids 10001 --level 10
.\AzurLaneWorkbook.exe recipes --available-only
.\AzurLaneWorkbook.exe items --name 图纸
.\AzurLaneWorkbook.exe resources
.\AzurLaneWorkbook.exe fleets --ids 1 --fields name,ships
.\AzurLaneWorkbook.exe technology --ship-type 1
```

- **装备汇总**：仓库中相同配置的装备合并计件；查询同时覆盖仓库库存与舰船槽位装载。
- **编队与科技**：`fleets` 不返回空编队；`technology` 按舰船组统计，不按单艘舰船实例统计。

### 字段、筛选与分页

| 参数 | 用法与说明 |
| --- | --- |
| `--ids ID,ID` | 指定对象 ID 列表；省略时返回全部匹配项 |
| `--fields FIELD,FIELD` | 筛选输出字段（支持点号下钻子字段）；主键 ID 始终保留 |
| `--full` | 返回支持的全部字段（与 `--fields` 互斥） |
| `--name TEXT` | 按名称模糊匹配 |
| `--type`、`--nation`、`--rarity` | 类别、阵营、稀有度筛选；类型与阵营支持名称或数值，稀有度使用数值 |
| `--level-min`、`--level-max` | 舰船等级或装备强化等级范围 |
| `--sort FIELD [--desc]` | 按指定字段排序（支持降序） |
| `--offset N --limit N` | 分页偏移量与每页数量 |
| `--fleet`、`--locked`、`--slot` | 舰船编队、锁定状态与槽位筛选 |
| `--family`、`--location`、`--ship`、`--slot` | 装备族、位置（`warehouse` / `equipped`）、宿主舰船与槽位；装备 `--slot` 需配合 `--ship` |

```powershell
.\AzurLaneWorkbook.exe ships --name 企业 --level-min 100 --locked true --fields name,stats.air
.\AzurLaneWorkbook.exe ships --ids 9001 --slot 2 --fields slots.slot_index,slots.equipment.config_id
.\AzurLaneWorkbook.exe equipment --location warehouse --family 1000 --sort enhance_level --desc --limit 20
.\AzurLaneWorkbook.exe equipment --ship 9001 --slot 2 --fields name,weapons,skills
.\AzurLaneWorkbook.exe recipes --equipment 1001 --available
```

- 指定 `--ids` 未找到的目标会在 `missing_ids` 中列出；因筛选条件排除的对象不计入缺失。
- 舰船 `details` 包含客户端完整单船信息，`stats` 为有效面板属性。装备 `definition` 包含配置及派生属性。
- 配方查询增加 `--available` 可返回考虑物资与材料后的 `max_count`，`--available-only` 仅列出当前可合成项。

## 直接装备操作

### 来源格式与单步操作

装备来源格式：
- 仓库装备：`warehouse:CONFIG_ID`
- 舰上装备：`ship:SHIP_ID:SLOT`（槽位范围 `1` 至 `5`，数量固定为 `1`）

| 命令 | 必填参数 | 说明 |
| --- | --- | --- |
| `compose RECIPE_ID COUNT` | 配方 ID、合成数量 | 合成装备并放入仓库 |
| `equip SHIP_ID SLOT SOURCE` | 目标舰船、槽位、来源 | 装配或替换指定槽位装备 |
| `unequip SHIP_ID SLOT` | 目标舰船、槽位 | 卸下指定槽位装备 |
| `enhance SOURCE LEVEL QUANTITY` | 来源、目标强化等级、件数 | 将指定件数装备强化到目标等级 |
| `dismantle SOURCE QUANTITY` | 来源、件数 | 拆解指定装备 |
| `equipment-actions FILE` | JSON 文件路径 | 检查或批量执行一组动作 |

所有直接装备命令**默认只执行计划检查（Dry-Run）**，不会修改游戏数据。确认回执中的步骤与消耗后，追加 `--apply` 参数正式执行：

```powershell
# 仅检查：输出装配路径与所需消耗
.\AzurLaneWorkbook.exe --instance mumu12:0 equip 9001 2 warehouse:1001

# 确认无误后执行
.\AzurLaneWorkbook.exe --instance mumu12:0 equip 9001 2 warehouse:1001 --apply
```

### 多舰船与装备族配装

支持针对多艘舰船统一换装或按装备族自动合成与强化：

```powershell
# 将仓库装备装配到多艘舰船的 2 号槽位
.\AzurLaneWorkbook.exe equip --ships 9001,9002 --slot 2 --source warehouse:1001

# 按装备族自动处理（优先仓库，不足时自动合成并强化到 +3）
.\AzurLaneWorkbook.exe equip --ships 9001,9002 --slot 2 --family 1000 --policy warehouse-compose --level 3

# 批量卸下全部槽位装备（或指定 --slots 1,2）
.\AzurLaneWorkbook.exe unequip --ships 9001,9002 --all-slots

# 批量强化指定槽位装备到 +10
.\AzurLaneWorkbook.exe enhance --ships 9001,9002 --slot 2 --level 10
```

装备族策略（`--policy`）：
- `warehouse-only`：仅使用仓库已有装备
- `warehouse-compose`：优先使用仓库，库存不足时自动合成
- `compose-only`：直接合成新装备

### JSON 批量动作文件

复杂的批量改动可编写为 JSON 文件统一处理。创建 `actions.json`（文件大小上限 1 MiB）：

```json
{
  "actions": [
    {"action":"equip","target":{"ship_id":9001,"slot_index":2},"source":{"kind":"warehouse","config_id":1001}},
    {"action":"unequip","target":{"ship_id":9002,"slot_index":3}}
  ]
}
```

```powershell
# 预检批量计划
.\AzurLaneWorkbook.exe equipment-actions actions.json

# 正式执行
.\AzurLaneWorkbook.exe equipment-actions actions.json --apply
```

动作类型及字段定义：

| `action` | 必填字段 | 说明 |
| --- | --- | --- |
| `compose` | `recipe_id`、`count` | 合成装备放入仓库 |
| `equip_family` | `family_id`、`policy`、`target_level`、`target` | 按装备族准备并装配（支持自动合成与强化） |
| `equip` | `source`、`target` | 从精确来源装配 |
| `unequip` | `target` | 卸下指定槽位装备 |
| `enhance` | `source`、`quantity`、`target_level` | 强化指定数量装备 |
| `dismantle` | `source`、`quantity` | 拆解指定数量装备 |

- `target` 格式：`{"ship_id": 9001, "slot_index": 2}`
- `source` 格式：仓库为 `{"kind": "warehouse", "config_id": 1001}`；舰上为 `{"kind": "ship_slot", "ship_id": 9002, "slot_index": 2}`
- 计划器会根据依赖关系自动安排执行次序（如优先合成、强化再装备），而非按数组顺序机械执行。共享材料、物资与背包容量统一核算，任何一步发生冲突或失败均会中止后续动作，已完成的操作不会自动回滚。

## 工作簿流程

工作簿命令操作本程序管理的文件，参数传入文件名即可（无需完整路径，通过 `workbooks` 命令查看）。

```powershell
# 1. 读取当前游戏数据并生成工作簿
.\AzurLaneWorkbook.exe --instance mumu12:0 generate

# 2. 查看或打开工作簿
.\AzurLaneWorkbook.exe workbooks
.\AzurLaneWorkbook.exe open fleet-001.xlsx

# 3. 编辑保存后，检查配装计划（只读，不修改游戏）
.\AzurLaneWorkbook.exe --instance mumu12:0 check fleet-001.xlsx

# 4. 检查并将校验结果与状态写回工作簿
.\AzurLaneWorkbook.exe --instance mumu12:0 check-save fleet-001.xlsx

# 5. 执行工作簿中的装备计划
.\AzurLaneWorkbook.exe --instance mumu12:0 execute fleet-001.xlsx
```

| 命令 | 行为说明 |
| --- | --- |
| `generate [NAME]` | 读取游戏数据生成 Excel 工作簿；省略文件名时自动编号 |
| `workbooks` | 列出当前已生成的工作簿文件 |
| `open NAME` | 调用系统默认程序打开指定工作簿 |
| `check NAME` | 核验工作簿中的换装计划与当前游戏资源，输出变更步骤与消耗 |
| `check-save NAME` | 核验计划并将检查状态写入工作簿，同时记录历史 |
| `execute NAME` | 预检、备份并正式执行游戏装备变更，随后回读状态写回表格 |

`execute` 不需要追加 `--apply`，调用即会执行。执行前会自动重新核验游戏状态，完成后记录历史并写回更新后的表格。

## 偏好设置、获取途径与布局

```powershell
# 查看配置与偏好
.\AzurLaneWorkbook.exe settings
.\AzurLaneWorkbook.exe settings preferences

# 修改偏好（立即生效并持久化）
.\AzurLaneWorkbook.exe settings set ship_acquisition_enabled true
.\AzurLaneWorkbook.exe settings set acquisition_update_policy use_cache
.\AzurLaneWorkbook.exe settings set detailed_diagnostics true
.\AzurLaneWorkbook.exe settings set unload_after_sync false

# 更新工作簿中舰船的获取途径缓存
.\AzurLaneWorkbook.exe update-acquisition fleet-001.xlsx --mode missing
```

- `ship_acquisition_enabled`：是否在工作簿中包含舰船获取途径（`true` / `false`）。
- `acquisition_update_policy`：获取途径更新策略（`use_cache` 仅读缓存 / `refresh` 在线刷新）。
- `detailed_diagnostics`：是否记录更详细的运行诊断信息。
- `unload_after_sync`：同步完成后是否自动卸载游戏内注入代理。
- `update-acquisition`：补全工作簿中舰船的获取途径数据，默认 `--mode missing` 抓取缺失及上次失败项，`--mode refresh` 重新拉取全部。

```powershell
# 布局检查、预览与升级
.\AzurLaneWorkbook.exe layout-check
.\AzurLaneWorkbook.exe layout-preview
.\AzurLaneWorkbook.exe layout-upgrade
```

布局维护命令用于验证或迁移 Excel 模板布局格式。`layout-preview` 与 `layout-upgrade` 会生成独立文件，不会直接覆盖现有模板。

## 代理、历史与日志

```powershell
# 查看、注入或卸载游戏内驻留代理
.\AzurLaneWorkbook.exe agent status mumu12:0
.\AzurLaneWorkbook.exe agent inject mumu12:0
.\AzurLaneWorkbook.exe agent unload mumu12:0

# 历史记录查看
.\AzurLaneWorkbook.exe history
.\AzurLaneWorkbook.exe history show 001-check.json --fields report

# 运行日志查看
.\AzurLaneWorkbook.exe logs
.\AzurLaneWorkbook.exe logs show 001-app.log --tail 20 --status failed
```

- `agent inject`：连接游戏并在进程中加载数据通信代理，已有可用代理时会自动复用。
- `history show`：查看指定历史检查或执行记录详情（支持 `--fields` 筛选顶层字段）。
- `logs show`：查看日志，支持按 `--status` 筛选及 `--tail N` 查看末尾行。

## 离线维护与缓存

```powershell
# 环境与安装完整性诊断
.\AzurLaneWorkbook.exe doctor

# 发布版本文件清单与校验和验证
.\AzurLaneWorkbook.exe verify-release
```

- `doctor`：离线检查程序安装完整性、必要运行时依赖、配置文件及模板布局。
- `verify-release`：核验发布清单中登记文件的 SHA-256 校验和。
- **缓存与静态数据**：静态图鉴与元数据经过校验后缓存在 `data/temp/`。

//! 集中维护命令的用途、参数及可直接复制的 PowerShell 示例。

use azur_lane_workbook::application::GameQueryKind;

const COMMANDS: &[(&str, &str, &str, &str)] = &[
    (
        "mcp",
        "启动标准输入输出 MCP 服务",
        "mcp",
        "供 MCP 客户端启动，不是交互式命令行。客户端 command 指向程序绝对路径，args 为 [\"mcp\"]。工具通过 tools/list 发现；游戏操作不生成表格，执行需提供检查返回的 plan_hash。",
    ),
    (
        "catalog",
        "查询静态图鉴",
        "catalog <ships | equipment | skills> [参数]",
        "使用 catalog ships --help、catalog equipment --help 或 catalog skills --help 查看详情。",
    ),
    (
        "catalog ships",
        "查询舰船静态配置",
        "catalog ships [--ids ID,ID] [查询选项]",
        "ID 为舰船配置 ID。支持 --name、--type、--nation、--rarity；舰种与阵营接受名称或数字 ID，稀有度接受配置数字。",
    ),
    (
        "catalog equipment",
        "查询装备静态配置、武器及技能",
        "catalog equipment [--ids ID,ID] [查询选项]",
        "支持 --name、--type、--nation、--rarity、--family、--level-min、--level-max。weapons 与 skills 按需读取；--full 会读取全部详情。",
    ),
    (
        "catalog skills",
        "查询指定等级的技能效果",
        "catalog skills --ids ID,ID [--level LEVEL] [查询选项]",
        "必须指定技能 ID；等级默认为 1。输出显示配置、战斗技能与 Buff 配置。",
    ),
    (
        "recipes",
        "查询装备合成配方及可合成数量",
        "recipes [--ids ID,ID] [--equipment CONFIG_ID] [--available | --available-only] [查询选项]",
        "--available 增加 max_count；--available-only 只返回当前可合成的配方。数量结合材料、物资和仓库容量计算，执行前会重新检查。",
    ),
    (
        "items",
        "查询背包材料数量",
        "items [--ids ID,ID] [--name TEXT] [查询选项]",
        "item_id 为材料 ID。compose_recipe 字段包含材料关联的实时配方信息。",
    ),
    (
        "resources",
        "查询物资和装备仓库容量",
        "resources [--fields FIELD,FIELD]",
        "gold 为物资；equipment_capacity 为当前装备仓库占用；equipment_limit 为容量上限。",
    ),
    (
        "fleets",
        "查询有成员的编队及成员装备",
        "fleets [--ids ID,ID] [查询选项]",
        "从舰船编队归属汇总，未包含空编队。ships 包含成员的实例 ID、名称、等级、槽位、队伍与位置。",
    ),
    (
        "technology",
        "查询舰船科技奖励与达成状态",
        "technology [--ids GROUP_ID,GROUP_ID] [--ship-type TYPE_ID] [查询选项]",
        "ID 为舰船组 ID。summaries 依次为获得、满星突破和 120 级的科技点、奖励及达成状态。--ship-type 按受益舰种 ID 筛选；definition 为配置，history 为图鉴历史。",
    ),
    (
        "compose",
        "合成指定数量装备并放入仓库",
        "compose RECIPE_ID COUNT [--apply]",
        "RECIPE_ID 来自 recipes，COUNT 为合成件数。默认只检查，末尾 --apply 执行。示例：compose 1001 2",
    ),
    (
        "ships",
        "查询持有舰船",
        "ships [--ids ID,ID] [--fields FIELD,FIELD | --full]",
        "舰船使用账号内实例 ID（ship_id）。省略 --ids 查询全部；显式 ID 最多 128 个。\n示例：.\\AzurLaneWorkbook.exe ships --ids 9001 --fields name,level,slots",
    ),
    (
        "equipment",
        "查询持有装备及装载位置",
        "equipment [--ids ID,ID] [--fields FIELD,FIELD | --full]",
        "装备使用包含强化等级的配置 ID（config_id）；同配置的仓库装备合并为数量，不是单件 ID。\n示例：.\\AzurLaneWorkbook.exe equipment --ids 1001 --full",
    ),
    (
        "equip",
        "从仓库或另一舰船穿戴、更换装备",
        "equip SHIP_ID SLOT SOURCE [--apply]",
        "SHIP_ID 为目标舰船实例 ID，SLOT 为目标槽位。\n示例：.\\AzurLaneWorkbook.exe equip 9001 2 warehouse:1001\n执行：.\\AzurLaneWorkbook.exe equip 9001 2 ship:9002:2 --apply",
    ),
    (
        "unequip",
        "卸下指定舰船槽位的装备",
        "unequip SHIP_ID SLOT [--apply]",
        "SHIP_ID 为舰船实例 ID，SLOT 为要清空的槽位。\n示例：.\\AzurLaneWorkbook.exe unequip 9001 2 --apply",
    ),
    (
        "enhance",
        "将指定来源的装备强化到目标等级",
        "enhance SOURCE LEVEL QUANTITY [--apply]",
        "LEVEL 为目标强化等级，不是增加的等级；QUANTITY 为件数。\n示例：.\\AzurLaneWorkbook.exe enhance warehouse:1001 10 1\n示例：.\\AzurLaneWorkbook.exe enhance ship:9001:2 10 1 --apply",
    ),
    (
        "dismantle",
        "拆解指定来源和数量的装备",
        "dismantle SOURCE QUANTITY [--apply]",
        "QUANTITY 为拆解件数；执行会消耗装备。\n示例：.\\AzurLaneWorkbook.exe dismantle warehouse:1001 2",
    ),
    (
        "equipment-actions",
        "从 JSON 文件检查或执行批量装备操作",
        "equipment-actions FILE [--apply]",
        "FILE 为 JSON 文件路径，含空格时使用双引号。文件最多 1 MiB。\n示例：.\\AzurLaneWorkbook.exe equipment-actions actions.json\n执行：.\\AzurLaneWorkbook.exe equipment-actions actions.json --apply\n文件示例：\n{\n  \"actions\": [\n    {\"action\":\"equip\",\"target\":{\"ship_id\":9001,\"slot_index\":2},\"source\":{\"kind\":\"warehouse\",\"config_id\":1001}},\n    {\"action\":\"unequip\",\"target\":{\"ship_id\":9002,\"slot_index\":3}}\n  ]\n}\n其他动作：enhance 使用 source、target_level、quantity；dismantle 使用 source、quantity。\n舰上来源：{\"kind\":\"ship_slot\",\"ship_id\":9001,\"slot_index\":2}。\n数组表达同一最终计划，执行顺序由依赖关系决定；失败后停止，已完成操作不会自动回滚。",
    ),
    (
        "instances",
        "列出模拟器实例及其可用状态",
        "instances",
        "使用结果中的 instance_id 选择实例。\n示例：.\\AzurLaneWorkbook.exe instances",
    ),
    (
        "agent",
        "查询、加载或卸载游戏内的驻留代理",
        "agent <status | inject | unload> [INSTANCE]",
        "status：查询状态；inject：复用可用代理，缺少时加载；unload：卸载。\nstatus 和 unload 不会启动游戏或创建代理。INSTANCE 使用 instances 返回的 instance_id。\n示例：.\\AzurLaneWorkbook.exe agent status\n示例：.\\AzurLaneWorkbook.exe agent unload INSTANCE\n更换程序后若驻留资源版本不符，使用原安装卸载，或重启游戏并重新登录。",
    ),
    (
        "generate",
        "读取游戏并生成工作簿",
        "generate [WORKBOOK_NAME]",
        "省略名称时自动命名为 fleet-001.xlsx 等；输出生成路径与是否复用。\n示例：.\\AzurLaneWorkbook.exe generate\n示例：.\\AzurLaneWorkbook.exe generate my-fleet.xlsx",
    ),
    (
        "check",
        "检查工作簿装备计划，不执行游戏写操作",
        "check WORKBOOK_NAME",
        "读取当前游戏状态，检查资源和计划可行性，输出检查结果。\n示例：.\\AzurLaneWorkbook.exe check fleet-001.xlsx",
    ),
    (
        "check-save",
        "检查计划并将检查结果写回工作簿",
        "check-save WORKBOOK_NAME",
        "不执行游戏写操作，会写入工作簿的检查结果。\n示例：.\\AzurLaneWorkbook.exe check-save fleet-001.xlsx",
    ),
    (
        "execute",
        "执行工作簿中的装备计划",
        "execute WORKBOOK_NAME",
        "会修改游戏装备状态；复用预检、备份、执行与回读流程。此命令不使用 --apply。\n示例：.\\AzurLaneWorkbook.exe execute fleet-001.xlsx",
    ),
    (
        "open",
        "使用系统默认程序打开工作簿",
        "open WORKBOOK_NAME",
        "示例：.\\AzurLaneWorkbook.exe open fleet-001.xlsx",
    ),
    (
        "workbooks",
        "列出已有工作簿",
        "workbooks",
        "输出工作簿目录信息。其他工作簿命令使用这里返回的名称，不使用任意外部文件路径。\n示例：.\\AzurLaneWorkbook.exe workbooks",
    ),
    (
        "history",
        "列出操作历史",
        "history",
        "输出已保存的历史记录目录信息。\n示例：.\\AzurLaneWorkbook.exe history",
    ),
    (
        "logs",
        "列出诊断日志",
        "logs",
        "输出日志目录信息，便于定位操作、ADB 和运行态日志。\n示例：.\\AzurLaneWorkbook.exe logs",
    ),
    (
        "settings",
        "查看运行配置、偏好或保存偏好",
        "settings [preferences | set KEY VALUE]",
        "settings：脱敏运行配置摘要；preferences：当前偏好；set：保存一个偏好，GUI 共用。\n可设置项：\n  ship_acquisition_enabled true|false  是否启用获取方式列\n  acquisition_update_policy use_cache|refresh  获取方式更新策略\n  detailed_diagnostics true|false  详细诊断日志\n  unload_after_sync true|false  同步完成后是否卸载代理\n示例：.\\AzurLaneWorkbook.exe settings preferences\n示例：.\\AzurLaneWorkbook.exe settings set unload_after_sync false",
    ),
    (
        "update-acquisition",
        "根据工作簿更新舰船获取方式缓存",
        "update-acquisition WORKBOOK_NAME [--mode missing|refresh]",
        "默认 missing 补齐缺失或上次失败的资料；refresh 联网刷新全部缓存。不修改已交付的工作簿。\n串行请求默认间隔 2 秒；settings.json 的 workbook.acquisition_request_interval_seconds 可设为 2 至 3600 秒。同安装目录共享限速与冷却。\n示例：.\\AzurLaneWorkbook.exe update-acquisition fleet-001.xlsx\n示例：.\\AzurLaneWorkbook.exe update-acquisition fleet-001.xlsx --mode refresh",
    ),
    (
        "doctor",
        "离线检查发布文件、配置和布局",
        "doctor",
        "不启动模拟器、游戏或 ADB；通过不代表真机连接已验证。\n示例：.\\AzurLaneWorkbook.exe doctor",
    ),
    (
        "verify-release",
        "校验发布包清单和文件完整性",
        "verify-release",
        "输出文件校验结果及允许修改的配置项。\n示例：.\\AzurLaneWorkbook.exe verify-release",
    ),
    (
        "layout-check",
        "检查当前工作簿布局",
        "layout-check",
        "验证布局结构和字段配置，不连接游戏。\n示例：.\\AzurLaneWorkbook.exe layout-check",
    ),
    (
        "layout-upgrade",
        "升级当前布局到程序支持的格式",
        "layout-upgrade",
        "按布局迁移规则生成升级结果；以命令返回的输出路径为准。\n示例：.\\AzurLaneWorkbook.exe layout-upgrade",
    ),
    (
        "layout-preview",
        "根据当前布局生成预览",
        "layout-preview",
        "使用预览数据检查工作簿展示，不读取真实游戏数据。\n示例：.\\AzurLaneWorkbook.exe layout-preview",
    ),
];

pub(crate) fn render(topic: Option<&str>) -> Result<String, String> {
    let Some(topic) = topic else {
        let mut text = "AzurLaneWorkbook 命令帮助\n\n用法：.\\AzurLaneWorkbook.exe [--instance INSTANCE] COMMAND [参数]\n不带参数：打开桌面窗口。\n\n命令：\n".to_owned();
        for (name, description, _, _) in COMMANDS {
            text.push_str(&format!("  {name:<20} {description}\n"));
        }
        text.push_str("  help [COMMAND]       查看总帮助或指定命令详情\n\n帮助：--help 或 -h；命令详情：ships --help 或 help ships。\n实例：先运行 instances，再将返回的 instance_id 放在 --instance 后。\n--instance 适用于 generate、check、check-save、execute、全部游戏查询和直接装备操作；agent 在命令末尾指定实例。\n\n开始使用：\n  .\\AzurLaneWorkbook.exe ships\n  .\\AzurLaneWorkbook.exe ships --help\n  .\\AzurLaneWorkbook.exe equipment --help\n  .\\AzurLaneWorkbook.exe equip --help\n\n查询与操作结果为 JSON，帮助为文字。示例 ID 需替换为实际值。\n直接装备操作默认只检查，添加 --apply 才执行；execute 命令直接执行工作簿计划。\n退出码：0 成功；1 参数或运行失败；2 装备执行未成功完成。\n");
        return Ok(text);
    };
    if topic == "help" {
        return Ok(
            "用法：help [COMMAND]；--help 或 -h 查看总帮助；COMMAND --help 查看详细用法。".into(),
        );
    }
    let (_, description, usage, detail) = COMMANDS
        .iter()
        .find(|item| item.0 == topic)
        .ok_or_else(|| format!("未知帮助主题 {topic:?}；运行 --help 查看全部命令。"))?;
    let mut text =
        format!("{topic} — {description}\n\n用法：.\\AzurLaneWorkbook.exe {usage}\n\n{detail}\n");
    if let Some(kind) = match topic {
        "ships" => Some(GameQueryKind::Ships),
        "equipment" => Some(GameQueryKind::Equipment),
        "catalog ships" => Some(GameQueryKind::CatalogShips),
        "catalog equipment" => Some(GameQueryKind::CatalogEquipment),
        "catalog skills" => Some(GameQueryKind::CatalogSkills),
        "recipes" => Some(GameQueryKind::Recipes),
        "items" => Some(GameQueryKind::Items),
        "resources" => Some(GameQueryKind::Resources),
        "fleets" => Some(GameQueryKind::Fleets),
        "technology" => Some(GameQueryKind::Technology),
        _ => None,
    } {
        text.push_str(&format!("\n字段：{}\n默认字段：{}\n--full 与 --fields 不能同时使用；对象标识始终保留。未找到的指定 ID 在 missing_ids 中返回。\n", kind.fields().join(","), kind.default_fields().join(",")));
        text.push_str("结果为 JSON，不生成工作簿。列表跨游戏帧采集，不保证同一瞬间的原子快照。\n");
        text.push_str("通用查询选项：--fields FIELD,FIELD 或 --full；--sort FIELD [--desc]；--offset N；--limit N。\n字段可用点号选择对象或对象数组中的子字段，例如 slots.slot_index、stats.durability；完整字段与其子字段不能同时选择。\n筛选与排序先于分页；missing_ids 仅表示指定对象不存在，不包含被筛选或分页排除的对象。\n");
    }
    match topic {
        "ships" => text.push_str("筛选：--name TEXT、--type 名称或ID、--nation 名称或ID、--rarity 数字、--level-min N、--level-max N、--locked true|false、--fleet ID。--slot 1..5 只保留指定槽位。\n"),
        "equipment" => text.push_str("筛选：--name TEXT、--type 名称或ID、--nation 名称或ID、--rarity 数字、--level-min N、--level-max N、--family ID、--location warehouse|equipped、--ship SHIP_ID [--slot 1..5]。等级指强化等级。\n"),
        "equip" => text.push_str("批量：equip --ships 9001,9002 --slot 2 --source warehouse:1001 [--apply]\n按装备族选择：equip --ships 9001,9002 --slot 2 --family 1000 --policy warehouse-compose [--level 3] [--apply]\n策略：warehouse-only 仅仓库；warehouse-compose 仓库不足时合成；compose-only 仅合成。--level 默认 0。\n"),
        "unequip" => text.push_str("批量：unequip --ships 9001,9002 --slots 1,2 [--apply]；--all-slots 表示全部槽位，与 --slots 互斥。\n"),
        "enhance" => text.push_str("批量：enhance --ships 9001,9002 --slot 2 --level 10 [--apply]\n"),
        "equipment-actions" => text.push_str("合成动作：{\"action\":\"compose\",\"recipe_id\":1001,\"count\":2}\n装备族动作：{\"action\":\"equip_family\",\"target\":{\"ship_id\":9001,\"slot_index\":2},\"family_id\":1000,\"policy\":\"warehouse-compose\",\"target_level\":3}\n"),
        "history" => text.push_str("详情：history show FILE [--fields FIELD,FIELD]。FILE 使用列表返回的文件名；只选择顶层字段。\n"),
        "logs" => text.push_str("详情：logs show FILE [--tail N] [--status STATUS]。先按顶层 status 精确筛选，再取末尾 N 条；tail 0 返回空列表。纯文本 ADB 日志不支持 status 筛选。\n"),
        _ => {},
    }
    if matches!(
        topic,
        "equip" | "unequip" | "enhance" | "dismantle" | "equipment-actions"
    ) {
        text.push_str("\n省略 --apply 只检查，末尾加 --apply 才修改游戏。槽位 SLOT 为 1～5。\nSOURCE：warehouse:CONFIG_ID 表示仓库；ship:SHIP_ID:SLOT 表示舰船槽位。\n数量必须大于零，舰船槽位来源数量只能为 1。示例 ID 需替换为实际值。\n");
    }
    if matches!(
        topic,
        "ships"
            | "equipment"
            | "equip"
            | "unequip"
            | "enhance"
            | "dismantle"
            | "equipment-actions"
            | "generate"
            | "check"
            | "check-save"
            | "execute"
    ) {
        text.push_str("\n先登录游戏进入港区；可在命令前加 --instance INSTANCE 选择实例（由 instances 查询）。\n");
    }
    Ok(text)
}

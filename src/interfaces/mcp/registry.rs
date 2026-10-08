//! 工具目录与参数说明；执行前使用同一目录拒绝缺失和多余参数。

use crate::application::GameQueryKind;
use rmcp::model::{Tool, ToolAnnotations};
use serde_json::{Map, Value, json};

pub(super) const INSTRUCTIONS: &str = "AzurLaneWorkbook 提供游戏查询、装备操作和本地工作簿管理。以下工具名均指本服务的 tools/list 名称。\n\
1. 游戏操作先调用 instances 获取 instance_id，再将它作为 instance 显式传入。需模拟器已启动、游戏已登录；游戏查询可能建立或复用进程内代理，但不修改装备。静态图鉴也需要游戏运行环境。\n\
2. 查看当前数据直接用 ships、equipment 等查询工具；生成 Excel 才用 workbook_generate。舰船实例 ship_id、装备配置 config_id、装备族 family_id 和配方 recipe_id 不可混用，应从查询结果取值。\n\
3. 装备操作先用 equipment_actions_check 检查 actions；核对 result.check 中的实际步骤和消耗后，将同一 actions、instance 与 result.plan_hash 交给 equipment_actions_apply。actions 表达最终要求，数组顺序不是执行顺序。工作簿计划用 workbook_check → workbook_execute，沿用同一 workbook、instance 和返回的 plan_hash。计划或状态变化后重新检查。\n\
4. 只有 workbook_generate、acquisition_update 是异步提交：每次新操作提供新的 request_id；提交返回 task_id 后，至少间隔 5 秒用 task_get 查询。响应丢失时可按原 request_id 查询；重发同一次提交须保持 request_id 和其他参数完全相同。task_cancel 只请求取消，继续查询终态。其他工具直接返回结果。\n\
5. 普通业务结果先看 status：ok 为成功；incomplete、failed、cancelled、unknown 需查看已有结果。长任务的 queued/running/cancelling 不是终态；succeeded、incomplete、failed、cancelled、unknown、interrupted 是终态，结合 result、diagnostic、persistence_error 判断。incomplete 可能已有可用文件，不能当作未执行。\n\
6. response_mode=summary 时，用 details.result_id 调用 result_get；长任务完成后的引用位于 result.details.result_id。先读 summary 和 fields，再用 field 选字段；数组用 offset/limit 分页。工作簿是快照，直接装备操作不会更新已有表格。\n\
7. 参数错误按工具名、字段路径和原因修正。业务错误保留 message、causes，以及存在时的 code/stage/context；也检查 result 内各阶段错误和 cleanup。写操作报错或超时不代表没有写入，先查回执、实际状态及 logs_list/logs_show，再只处理未完成部分；不要直接重放整批操作。";

pub(super) fn query_kind(name: &str) -> Option<GameQueryKind> {
    use GameQueryKind::*;
    Some(match name {
        "ships" => Ships,
        "equipment" => Equipment,
        "catalog_ships" => CatalogShips,
        "catalog_equipment" => CatalogEquipment,
        "catalog_skills" => CatalogSkills,
        "recipes" => Recipes,
        "items" => Items,
        "resources" => Resources,
        "fleets" => Fleets,
        "technology" => Technology,
        _ => return None,
    })
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object", "properties":properties, "required":required, "additionalProperties":false})
}
fn text(description: &str) -> Value {
    json!({"type":"string", "minLength":1, "description":description})
}
fn number(description: &str) -> Value {
    json!({"type":"integer", "minimum":0, "description":description})
}
fn flag(description: &str) -> Value {
    json!({"type":"boolean", "description":description})
}
fn strings(description: &str) -> Value {
    json!({"type":"array", "items":{"type":"string","minLength":1}, "description":description})
}

fn actions() -> Value {
    let mut target = object(
        json!({"ship_id":{"type":"integer","minimum":1,"description":"账号内舰船实例 ID，取自 ships.ship_id；不是图鉴 config_id"}, "slot_index":{"type":"integer","minimum":1,"maximum":5,"description":"装备槽位编号 1 至 5，取自 ships 的 slots.slot_index"}}),
        &["ship_id", "slot_index"],
    );
    target["description"] =
        json!("目标舰船装备槽；同时提供 ship_id 和 slot_index，使用 ships 查询的真实值");
    let source = json!({"oneOf":[
        object(json!({"kind":{"const":"warehouse","description":"从仓库取装备"},"config_id":{"type":"integer","minimum":1,"description":"equipment 返回的精确配置 ID，包含强化等级；不是 family_id"}}), &["kind","config_id"]),
        object(json!({"kind":{"const":"ship_slot","description":"使用指定舰船槽位当前的装备"},"ship_id":target["properties"]["ship_id"],"slot_index":target["properties"]["slot_index"]}), &["kind","ship_id","slot_index"])
    ],"description":"精确装备来源。warehouse 使用 config_id；ship_slot 使用 ship_id 和 slot_index。舰上来源的 quantity 只能为 1。"});
    let positive = json!({"type":"integer","minimum":1});
    let level = json!({"type":"integer","minimum":0,"maximum":255,"description":"目标强化等级，如 10 表示最终 +10，不是增加 10 级；实际允许等级由装备规则检查"});
    let mut variants = Vec::new();
    for (name, mut props) in [
        (
            "compose",
            json!({"recipe_id":{"type":"integer","minimum":1,"description":"recipes 返回的 recipe_id"},"count":{"type":"integer","minimum":1,"description":"本次合成件数"}}),
        ),
        (
            "equip_family",
            json!({"target":target,"family_id":{"type":"integer","minimum":1,"description":"equipment 或 catalog_equipment 返回的 family_id；不是具体强化配置 config_id"},"policy":{"type":"string","enum":["warehouse-only","warehouse-compose","compose-only"],"description":"warehouse-only 只取仓库；warehouse-compose 优先仓库、不足时合成；compose-only 只合成。强化步骤由 target_level 和检查结果决定。"},"target_level":level}),
        ),
        ("equip", json!({"target":target,"source":source})),
        ("unequip", json!({"target":target})),
        (
            "enhance",
            json!({"source":source,"target_level":level,"quantity":positive}),
        ),
        ("dismantle", json!({"source":source,"quantity":positive})),
    ] {
        let props = props.as_object_mut().expect("固定对象");
        let description = match name {
            "compose" => "按配方合成指定件数，产物进入仓库。",
            "equip_family" => {
                "按装备族和来源策略为目标槽位配装，可自动合成或强化；先核对检查返回的步骤。"
            }
            "equip" => "将精确来源装备装到目标槽位；舰上来源可用于跨船调拨。",
            "unequip" => "清空目标槽位，装备返回仓库。",
            "enhance" => "将来源装备强化到目标等级；quantity 为件数，舰上来源只能为 1。",
            "dismantle" => "拆解来源装备并消耗指定件数；舰上来源只能为 1，实际限制由检查阶段核验。",
            _ => unreachable!(),
        };
        if let Some(quantity) = props.get_mut("quantity") {
            quantity["description"] = json!("本次操作件数；仓库来源可批量，舰上来源必须为 1");
        }
        props.insert(
            "action".into(),
            json!({"const":name,"description":description}),
        );
        let keys = props.keys().map(String::as_str).collect::<Vec<_>>();
        variants.push(object(json!(props), &keys));
    }
    json!({"type":"array","minItems":1,"items":{"oneOf":variants},"description":"至少一项最终要求，实际执行顺序由依赖决定。每项仅填写所选 action 分支的必填字段。示例：[{\"action\":\"unequip\",\"target\":{\"ship_id\":123,\"slot_index\":1}}]；示例 ID 必须替换为查询到的真实 ID。"})
}

pub(super) fn tools() -> Vec<Tool> {
    let mut result = Vec::new();
    for (name, description, read_only) in [
        (
            "ships",
            "查询账号持有舰船；用 name 查名称、ids 查 ship_id。需要槽位装备时指定 fields:[\"name\",\"slots\"]。返回 entries 和 missing_ids，不生成工作簿。",
            true,
        ),
        (
            "equipment",
            "查询账号持有装备，按 config_id 汇总仓库数量和舰上位置。ship + slot 可筛选指定舰船槽位；family 筛选同族的不同强化配置，各配置仍分别返回。返回 entries 和 missing_ids。",
            true,
        ),
        (
            "catalog_ships",
            "查询舰船静态图鉴，ids 为 config_id；用于基础属性和分类资料，账号持有情况用 ships。仍需游戏运行环境。",
            true,
        ),
        (
            "catalog_equipment",
            "查询装备静态图鉴，包括未持有装备；ids 为 config_id，family 为 family_id。武器和技能用 fields:[\"weapons\",\"skills\"]。仍需游戏运行环境。",
            true,
        ),
        (
            "catalog_skills",
            "查询技能在指定等级的静态效果；必填非空 ids（skill_id），skill_level 默认 1。检查每条结果的 complete 判断资料完整性；仍需游戏运行环境。",
            true,
        ),
        (
            "recipes",
            "查询装备合成配方，ids 为 recipe_id。available:true 计算 max_count；available_only:true 只保留 max_count>0 的配方。使用自定义 fields 时需显式选择 max_count。配方编号可用于 compose 动作。",
            true,
        ),
        (
            "items",
            "查询账号背包材料与数量；ids 为 item_id，name 按名称包含筛选。",
            true,
        ),
        (
            "resources",
            "查询账号物资 gold、装备仓库容量 equipment_capacity 和上限 equipment_limit。",
            true,
        ),
        (
            "fleets",
            "查询有成员的编队及成员装备；ids 为 fleet_id。先查看返回记录的 name、kind、ships 确认目标编队，再使用成员 ship_id，不能把编队序号当舰船 ID。",
            true,
        ),
        (
            "technology",
            "查询舰船科技奖励和达成情况；ids 为 group_id，ship_type 筛选科技奖励适用的舰种。",
            true,
        ),
        (
            "equipment_actions_check",
            "执行单项或批量装备操作前的模拟检查，不修改装备、不生成工作簿。返回 result.check（实际步骤、消耗与警告）及 result.plan_hash；确认后将相同 actions、instance 和该摘要传给 equipment_actions_apply。",
            true,
        ),
        (
            "equipment_actions_apply",
            "执行 equipment_actions_check 已检查并确认的计划；actions、instance 必须与检查时相同，plan_hash 使用检查原值。会消耗材料、物资或装备。返回执行与清理摘要，details.result_id 可读逐步证据。部分失败后先核对实际状态，再重新检查剩余动作，不能重放整批。",
            false,
        ),
        (
            "instances",
            "首次游戏查询或操作前调用；列出模拟器实例及就绪信息，将选定记录的 instance_id 原样传给其他工具的 instance。",
            true,
        ),
        (
            "agent_status",
            "查询指定实例的游戏内驻留代理状态，不触发注入；需要手动加载或卸载时分别使用 agent_inject、agent_unload。",
            true,
        ),
        (
            "agent_inject",
            "在指定实例加载或复用游戏进程内的通信代理，改变运行时状态；普通游戏查询会按需准备通信，通常无需手动调用。",
            false,
        ),
        (
            "agent_unload",
            "卸载指定实例的游戏进程内通信代理，改变运行时状态；用于结束驻留或处理代理异常。",
            false,
        ),
        (
            "workbooks_list",
            "列出本地 Excel 文件及 kind。执行计划或更新获取途径时选 kind=workbook；layout_preview、layout_upgrade 是布局产物。workbook 参数使用文件名，不使用绝对路径。",
            true,
        ),
        (
            "workbook_generate",
            "从游戏读取当前状态并生成新的 Excel 快照。必填新 request_id；name 省略时自动编号。立即返回 task_id，之后用 task_get 取文件路径和结果；incomplete 可能已有文件但存在数据警告或清理失败。重发同一次提交须保持 request_id 和参数不变。",
            false,
        ),
        (
            "workbook_check",
            "读取本地工作簿计划并结合当前游戏状态检查，不修改装备、不写回工作簿。返回 result.check 和 result.plan_hash；核对实际步骤、消耗后可调用 workbook_execute。",
            true,
        ),
        (
            "workbook_check_save",
            "检查本地工作簿计划，写回检查结果并保存历史；不修改游戏装备。需要只检查而不改文件时用 workbook_check。返回各阶段结果及完整证据引用。",
            false,
        ),
        (
            "workbook_execute",
            "执行 workbook_check 已检查并确认的工作簿计划。使用相同 workbook、instance 和检查返回的 plan_hash；执行前备份，执行后保存历史并写回结果。会修改游戏装备；结果异常时先查逐步回执及实际状态，不直接重试。",
            false,
        ),
        (
            "workbook_open",
            "用系统默认程序打开 workbooks_list 中选定的 Excel 文件；不读取或执行其中的配装计划。",
            false,
        ),
        (
            "layout_check",
            "检查当前安装目录的 workbook-layout.xlsx 模板结构与版本；检查用户配装计划应使用 workbook_check。",
            true,
        ),
        (
            "layout_upgrade",
            "升级当前 workbook-layout.xlsx 模板并输出独立升级文件；不自动替换当前模板。返回产物位置。",
            false,
        ),
        (
            "layout_preview",
            "用固定示例数据按当前模板生成 Excel 预览，刷新固定预览路径并返回产物位置；不读取真实游戏数据。",
            false,
        ),
        (
            "settings_get",
            "读取当前安装目录的运行配置摘要，用于核对实例选择、通信和诊断设置；更新四项用户偏好请先调用 preferences_get。",
            true,
        ),
        (
            "preferences_get",
            "读取四项持久化用户偏好；将 result 对象原样作为 preferences_update.original，以修改后的完整副本作为 preferences。",
            true,
        ),
        (
            "preferences_update",
            "更新四项用户偏好。先调用 preferences_get，将原对象放入 original，修改后的完整对象放入 preferences；两者均需全部四项。发生冲突时重新读取并合并，不能沿用过期 original。",
            false,
        ),
        (
            "acquisition_update",
            "根据指定工作簿中的舰船更新 BWiki 获取途径缓存；不会同步游戏，也不会重写该工作簿。必填 workbook、request_id，mode 默认 missing。立即返回 task_id，用 task_get 查询终态；同一次提交重发须保持 request_id 和参数不变。",
            false,
        ),
        (
            "history_list",
            "列出当前安装目录的操作历史文件；读取内容用 history_show，filename 取返回路径最后一段。",
            true,
        ),
        (
            "history_show",
            "读取历史文件；fields 只接受顶层字段，例如 report，不支持 report.message。",
            true,
        ),
        (
            "result_get",
            "读取已保存的完整操作证据，不重新执行操作。result_id 取自 details.result_id；首次省略 field 获取 summary 和 fields，再按实际结构选 field。对象返回 value；数组返回 entries、total、next_offset。只对数组使用 offset/limit，单次响应上限 48 KiB。",
            true,
        ),
        (
            "logs_list",
            "列出当前安装目录的诊断日志文件；读取内容用 logs_show，filename 取返回路径最后一段。",
            true,
        ),
        (
            "logs_show",
            "读取 logs_list 返回的日志文件；JSONL 日志先按 status 筛选再取 tail 条，省略筛选时读取全部。用于定位操作、阶段和原始失败原因。",
            true,
        ),
        (
            "diagnostic_open",
            "用系统默认程序打开历史或日志文件；kind=history 时从 history_list 选文件，kind=log 时从 logs_list 选文件。需要直接读取内容时用 history_show 或 logs_show。",
            false,
        ),
        (
            "doctor",
            "离线检查当前安装的发布文件、配置和布局，返回各检查项状态；offline_ready 只表示离线条件就绪，不证明模拟器或游戏已连接。",
            true,
        ),
        (
            "release_verify",
            "核验当前安装的发布清单和文件完整性；不连接游戏。需要同时检查设置和布局时用 doctor。",
            true,
        ),
        (
            "task_get",
            "查询 workbook_generate 或 acquisition_update 的任务。task_id、request_id 恰好填一个，必须来自原提交；至少间隔 5 秒查询。queued/running/cancelling 继续等待，其余状态查看 result、diagnostic 和 persistence_error；成功终态为 succeeded。",
            true,
        ),
        (
            "task_cancel",
            "请求协作式取消长任务；继续 task_get 查询实际终态及已完成的操作。task_id 或 request_id 二选一。",
            false,
        ),
    ] {
        let mut props = Map::new();
        let mut required = Vec::new();
        if name == "result_get" {
            props.insert("result_id".into(), text("复制操作回执的 details.result_id（64 位小写十六进制）；长任务中位于 result.details.result_id，不是 task_id"));
            required.push("result_id");
            props.insert(
                "field".into(),
                text("点号字段路径，如 result.execution.steps；省略时返回摘要及顶层字段"),
            );
            props.insert("offset".into(), number("数组起始位置，默认 0"));
            props.insert("limit".into(), json!({"type":"integer","minimum":1,"maximum":100,"default":20,"description":"数组页大小；响应过大时减小"}));
        }
        if super::tasks::is_long(name) {
            props.insert("request_id".into(), json!({"type":"string","minLength":1,"maxLength":128,"description":"本次操作的唯一编号；超时或响应丢失后重试必须复用原编号及参数"}));
            required.push("request_id");
        }
        if matches!(name, "task_get" | "task_cancel") {
            props.insert(
                "task_id".into(),
                text("提交返回的完整 64 位小写十六进制 task_id；与 request_id 恰好选一个，不使用 plan_hash 或 result_id"),
            );
            props.insert("request_id".into(), json!({"type":"string","minLength":1,"maxLength":128,"description":"原提交编号；与 task_id 二选一"}));
        }
        if let Some(kind) = query_kind(name) {
            props.insert(
                "fields".into(),
                strings(&format!(
                    "省略或 [] 使用默认字段：{}。主键始终保留。可用点号选子字段，但不能同时选父字段和子字段。可用顶层字段：{}",
                    kind.default_fields().join(", "), kind.fields().join(", ")
                )),
            );
            props.insert("full".into(), flag("返回全部字段；不可与 fields 同用"));
            props.insert("sort".into(), text("按一个返回字段排序，支持对象的点号路径；省略时按主键升序。数组内部字段不可直接排序"));
            props.insert(
                "descending".into(),
                flag("默认 false；true 为降序且必须提供 sort"),
            );
            props.insert(
                "limit".into(),
                number("筛选和排序后最多返回的条数；省略时不限制，0 返回空列表。建议先用 20"),
            );
            props.insert(
                "offset".into(),
                number("筛选和排序后跳过的条数，默认 0；不是页码"),
            );
            if name != "resources" {
                props.insert("ids".into(), json!({"type":"array","items":{"type":"integer","minimum":1},"uniqueItems":true,"description":format!("按 {} 选择，必须使用该工具返回的主键，不能混用其他 ID；正整数且不重复。{}",kind.identity_field(), if name == "catalog_skills" { "必填且不能为空。" } else { "省略或 [] 表示不按 ID 限制。" })}));
            }
            if name == "catalog_skills" {
                required.push("ids");
                props.insert(
                    "skill_level".into(),
                    json!({"type":"integer","minimum":1,"description":"技能等级，默认 1"}),
                );
            }
            if matches!(
                name,
                "ships" | "equipment" | "catalog_ships" | "catalog_equipment" | "items"
            ) {
                props.insert(
                    "name".into(),
                    text("名称包含此文本时匹配，区分大小写；不是正则表达式"),
                );
            }
            if matches!(
                name,
                "ships" | "equipment" | "catalog_ships" | "catalog_equipment"
            ) {
                props.insert("object_type".into(), text("精确匹配查询结果中的舰种或装备类型值；数字 ID 需传字符串，不按名称包含匹配"));
                props.insert(
                    "nation".into(),
                    text("精确匹配查询结果中的阵营值；数字 ID 需传字符串，不按名称包含匹配"),
                );
                props.insert("rarity".into(), number("精确匹配游戏内部 rarity 数值；先查询确认数值与装备或舰船的对应关系，不直接按显示颜色推算"));
            }
            if matches!(name, "ships" | "equipment" | "catalog_equipment") {
                let level_name = if name == "ships" {
                    "舰船等级"
                } else {
                    "装备强化等级"
                };
                props.insert(
                    "level_min".into(),
                    number(&format!("{level_name}下限，含边界；不得大于 level_max")),
                );
                props.insert(
                    "level_max".into(),
                    number(&format!("{level_name}上限，含边界")),
                );
            }
            if matches!(name, "equipment" | "catalog_equipment") {
                props.insert(
                    "family".into(),
                    number("查询结果中的 family_id；筛选同族各强化配置，不是 config_id"),
                );
            }
            if name == "ships" {
                props.insert("locked".into(), flag("锁定状态"));
                props.insert(
                    "fleet".into(),
                    number("fleets 返回的 fleet_id，仅返回该编队成员；不是舰船 ID"),
                );
            }
            if matches!(name, "ships" | "equipment") {
                props.insert(
                    "slot".into(),
                    json!({"type":"integer","minimum":1,"maximum":5,"description":if name == "ships" { "只保留指定编号的 slots；需在 fields 中选择 slots 或其子字段才能看到槽位" } else { "按舰上槽位编号筛选装备，必须同时提供 ship；取值 1 至 5" }}),
                );
            }
            if name == "equipment" {
                props.insert(
                    "ship".into(),
                    number("ships 返回的 ship_id，筛选该船挂载的装备；不是图鉴 config_id"),
                );
                props.insert(
                    "location".into(),
                    json!({"type":"string","enum":["warehouse","equipped"],"description":"warehouse 筛选仓库数量大于 0 的配置；equipped 筛选有舰上挂载的配置。只筛选配置记录，不裁剪其中的 equipped 位置列表"}),
                );
            }
            if name == "recipes" {
                props.insert("equipment".into(), number("产物装备配置 ID"));
                props.insert("available".into(), flag("默认 false；true 计算 max_count，使用默认 fields 时自动返回该字段；自定义 fields 时需显式包含 max_count"));
                props.insert("available_only".into(), flag("只返回当前可合成的配方"));
            }
            if name == "technology" {
                props.insert("ship_type".into(), number("科技舰种 ID"));
            }
        }
        if query_kind(name).is_some()
            || name.starts_with("equipment_actions_")
            || name.starts_with("agent_")
            || matches!(
                name,
                "workbook_generate" | "workbook_check" | "workbook_check_save" | "workbook_execute"
            )
        {
            props.insert(
                "instance".into(),
                text("instances 返回的 instance_id 原值，例如 mumu12:0。建议显式填写；省略时按运行配置解析，目标不明确会报错。检查和执行必须使用同一实例"),
            );
        }
        if name.starts_with("equipment_actions_") {
            props.insert("actions".into(), actions());
            required.push("actions");
        }
        if matches!(name, "equipment_actions_apply" | "workbook_execute") {
            props.insert("plan_hash".into(),json!({"type":"string","pattern":"^[0-9a-f]{64}$","description":"复制对应检查工具 result.plan_hash 原值；不是任务或证据 ID。检查参数、工作簿计划或游戏状态变化后重新检查，不能自行计算或复用旧摘要"}));
            required.push("plan_hash");
        }
        if matches!(
            name,
            "workbook_check"
                | "workbook_check_save"
                | "workbook_execute"
                | "workbook_open"
                | "acquisition_update"
        ) {
            props.insert("workbook".into(), text("workbooks_list 中 kind=workbook 的文件名，含 .xlsx；取路径最后一段，不传目录或绝对路径"));
            required.push("workbook");
        }
        if name == "workbook_generate" {
            props.insert(
                "name".into(),
                text("单个工作簿文件名，省略时自动编号；无扩展名时补 .xlsx。不得含目录或首尾空白，同名不同内容不会覆盖"),
            );
        }
        if name == "acquisition_update" {
            props.insert("mode".into(), json!({"type":"string","enum":["missing","refresh"],"default":"missing","description":"missing 复用有效缓存，只补齐缺失及上次失败项；refresh 全量在线刷新。默认 missing。明确未收录页面保留未收录状态。"}));
        }
        if matches!(name, "history_show" | "logs_show" | "diagnostic_open") {
            props.insert("filename".into(), text("目录查询返回路径的最后一段文件名"));
            required.push("filename");
        }
        if name == "history_show" {
            props.insert(
                "fields".into(),
                strings("只选择历史顶层字段，例如 report；不支持点号子字段"),
            );
        }
        if name == "logs_show" {
            props.insert(
                "tail".into(),
                number(
                    "取最后 N 条记录或文本行；JSONL 先筛选再截断，省略时不截断，0 返回空记录列表",
                ),
            );
            props.insert("status".into(), text("精确匹配 JSONL 记录中的 status，例如 failed；以日志实际值为准，省略时不筛选。纯文本日志不支持此参数"));
        }
        if name == "diagnostic_open" {
            props.insert(
                "kind".into(),
                json!({"type":"string","enum":["history","log"],"description":"history 对应 history_list；log 对应 logs_list"}),
            );
            required.push("kind");
        }
        if name == "preferences_update" {
            let preferences = object(
                json!({"ship_acquisition_enabled":flag("生成工作簿时补充舰船获取途径"),"acquisition_update_policy":{"type":"string","enum":["use_cache","refresh"],"description":"use_cache 复用获取途径缓存；refresh 在线刷新。是否启用由 ship_acquisition_enabled 决定"},"detailed_diagnostics":flag("启用详细诊断日志"),"unload_after_sync":flag("操作结束后卸载代理")}),
                &[
                    "ship_acquisition_enabled",
                    "acquisition_update_policy",
                    "detailed_diagnostics",
                    "unload_after_sync",
                ],
            );
            let mut original = preferences.clone();
            original["description"] =
                json!("preferences_get 返回的四项偏好对象原值；不要修改此基线");
            let mut preferences = preferences;
            preferences["description"] = json!("修改后的完整四项偏好对象；未修改项也必须保留");
            props.insert("original".into(), original);
            props.insert("preferences".into(), preferences);
            required.extend(["original", "preferences"]);
        }
        let mut schema = object(Value::Object(props), &required)
            .as_object()
            .unwrap()
            .clone();
        if matches!(name, "task_get" | "task_cancel") {
            schema.insert(
                "oneOf".into(),
                json!([
                    {"required":["task_id"],"not":{"required":["request_id"]}},
                    {"required":["request_id"],"not":{"required":["task_id"]}}
                ]),
            );
        }
        result.push(Tool::new(name, description, schema).with_annotations(
            ToolAnnotations::from_raw(
                None,
                Some(read_only),
                Some(!read_only),
                Some(read_only || super::tasks::is_long(name) || name == "task_cancel"),
                Some(
                    query_kind(name).is_some()
                        || name == "acquisition_update"
                        || name.starts_with("agent_")
                        || name.starts_with("equipment_actions_")
                        || name.starts_with("workbook_"),
                ),
            ),
        ));
    }
    result
}

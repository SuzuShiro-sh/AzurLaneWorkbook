//! 将直接装备命令和批量 JSON 输入转换为同一应用请求。

use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;

use azur_lane_workbook::application::{
    DirectAction, DirectActionBatch, DirectEquipmentSource, DirectShipSlot,
};

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ActionInput {
    Inline(DirectActionBatch),
    File(PathBuf),
}

impl ActionInput {
    pub(crate) fn load(self) -> Result<DirectActionBatch, String> {
        match self {
            Self::Inline(batch) => Ok(batch),
            Self::File(path) => {
                const MAXIMUM_BYTES: u64 = 1024 * 1024;
                let mut bytes = Vec::new();
                std::fs::File::open(&path)
                    .map_err(|error| format!("打开装备操作文件 {} 失败: {error}", path.display()))?
                    .take(MAXIMUM_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| {
                        format!("读取装备操作文件 {} 失败: {error}", path.display())
                    })?;
                if bytes.len() as u64 > MAXIMUM_BYTES {
                    return Err("装备操作文件超过 1 MiB".to_owned());
                }
                serde_json::from_slice(&bytes)
                    .map_err(|error| format!("装备操作文件 {} 无效: {error}", path.display()))
            }
        }
    }
}

fn number<T: std::str::FromStr>(value: &OsString, label: &str) -> Result<T, String> {
    value
        .to_str()
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| format!("{label}必须是有效整数"))
}

fn source(value: &OsString) -> Result<DirectEquipmentSource, String> {
    let text = value.to_str().ok_or("装备来源必须是有效文本")?;
    let parts: Vec<_> = text.split(':').collect();
    match parts.as_slice() {
        ["warehouse", id] => Ok(DirectEquipmentSource::Warehouse {
            config_id: number(&OsString::from(id), "装备配置 ID")?,
        }),
        ["ship", id, slot] => Ok(DirectEquipmentSource::ShipSlot {
            ship_id: number(&OsString::from(id), "舰船实例 ID")?,
            slot_index: number(&OsString::from(slot), "槽位")?,
        }),
        _ => Err("装备来源格式: warehouse:CONFIG_ID 或 ship:SHIP_ID:SLOT".to_owned()),
    }
}

pub(crate) fn parse(command: &str, arguments: &[OsString]) -> Result<(ActionInput, bool), String> {
    let (arguments, apply) = if arguments.last().is_some_and(|value| value == "--apply") {
        (&arguments[..arguments.len() - 1], true)
    } else {
        (arguments, false)
    };
    if arguments
        .first()
        .is_some_and(|value| value.to_str().is_some_and(|text| text.starts_with("--")))
    {
        return parse_ships(command, arguments).map(|batch| (ActionInput::Inline(batch), apply));
    }
    let action = match (command, arguments) {
        ("compose", [recipe, count]) => DirectAction::Compose { recipe_id: number(recipe, "配方 ID")?, count: number(count, "合成数量")? },
        ("equipment-actions", [path]) if !path.is_empty() => return Ok((ActionInput::File(path.into()), apply)),
        ("equip", [ship, slot, from]) => DirectAction::Equip { target: DirectShipSlot { ship_id: number(ship, "舰船实例 ID")?, slot_index: number(slot, "槽位")? }, source: source(from)? },
        ("unequip", [ship, slot]) => DirectAction::Unequip { target: DirectShipSlot { ship_id: number(ship, "舰船实例 ID")?, slot_index: number(slot, "槽位")? } },
        ("enhance", [from, level, quantity]) => DirectAction::Enhance { source: source(from)?, target_level: number(level, "目标强化等级")?, quantity: number(quantity, "数量")? },
        ("dismantle", [from, quantity]) => DirectAction::Dismantle { source: source(from)?, quantity: number(quantity, "数量")? },
        _ => return Err("用法: equip SHIP_ID SLOT SOURCE | unequip SHIP_ID SLOT | enhance SOURCE LEVEL QUANTITY | dismantle SOURCE QUANTITY | compose RECIPE_ID COUNT | equipment-actions FILE；末尾添加 --apply 执行，省略时只检查".to_owned()),
    };
    Ok((
        ActionInput::Inline(DirectActionBatch {
            actions: vec![action],
        }),
        apply,
    ))
}

fn parse_ships(command: &str, arguments: &[OsString]) -> Result<DirectActionBatch, String> {
    use azur_lane_workbook::domain::SourcePolicy;
    use std::collections::BTreeMap;
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let key = arguments[index].to_str().ok_or("选项必须是有效文本")?;
        let value = if key == "--all-slots" {
            None
        } else {
            index += 1;
            Some(
                arguments
                    .get(index)
                    .ok_or_else(|| format!("{key} 缺少值"))?,
            )
        };
        if options.insert(key, value).is_some() {
            return Err(format!("重复选项 {key}"));
        }
        index += 1;
    }
    let allowed: &[&str] = match command {
        "equip" => &[
            "--ships", "--slot", "--source", "--family", "--policy", "--level",
        ],
        "unequip" => &["--ships", "--slots", "--all-slots"],
        "enhance" => &["--ships", "--slot", "--level"],
        _ => return Err("该命令不支持 --ships".into()),
    };
    for key in options.keys() {
        if !allowed.contains(key) {
            return Err(format!("未知选项 {key}"));
        }
    }
    let required = |key: &str| {
        options
            .get(key)
            .and_then(|value| *value)
            .ok_or_else(|| format!("缺少 {key}"))
    };
    let ships = number_list::<u64>(required("--ships")?, "舰船实例 ID")?;
    let mut actions = Vec::new();
    for ship_id in ships {
        match command {
            "unequip" => {
                if options.contains_key("--all-slots") == options.contains_key("--slots") {
                    return Err("必须指定 --slots 或 --all-slots 之一".into());
                }
                let slots = if options.contains_key("--all-slots") {
                    vec![1, 2, 3, 4, 5]
                } else {
                    number_list::<u8>(required("--slots")?, "槽位")?
                };
                for slot_index in slots {
                    actions.push(DirectAction::Unequip {
                        target: DirectShipSlot {
                            ship_id,
                            slot_index,
                        },
                    });
                }
            }
            "enhance" => actions.push(DirectAction::Enhance {
                source: DirectEquipmentSource::ShipSlot {
                    ship_id,
                    slot_index: number(required("--slot")?, "槽位")?,
                },
                target_level: number(required("--level")?, "目标等级")?,
                quantity: 1,
            }),
            "equip" => {
                let target = DirectShipSlot {
                    ship_id,
                    slot_index: number(required("--slot")?, "槽位")?,
                };
                if options.contains_key("--source") {
                    if ["--family", "--policy", "--level"]
                        .iter()
                        .any(|key| options.contains_key(key))
                    {
                        return Err("精确来源不能同时指定装备族、策略或等级".into());
                    }
                    actions.push(DirectAction::Equip {
                        target,
                        source: source(required("--source")?)?,
                    });
                } else {
                    let policy =
                        match required("--policy")?.to_str() {
                            Some("warehouse-only") => SourcePolicy::WarehouseOnly,
                            Some("warehouse-compose") => SourcePolicy::WarehouseThenCompose,
                            Some("compose-only") => SourcePolicy::ComposeOnly,
                            _ => return Err(
                                "来源策略必须是 warehouse-only、warehouse-compose 或 compose-only"
                                    .into(),
                            ),
                        };
                    actions.push(DirectAction::EquipFamily {
                        target,
                        family_id: number(required("--family")?, "装备族 ID")?,
                        policy,
                        target_level: options
                            .get("--level")
                            .and_then(|v| *v)
                            .map(|v| number(v, "目标等级"))
                            .transpose()?
                            .unwrap_or(0),
                    });
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(DirectActionBatch { actions })
}

fn number_list<T: std::str::FromStr + Ord>(
    value: &OsString,
    label: &str,
) -> Result<Vec<T>, String> {
    let mut seen = std::collections::BTreeSet::new();
    for part in value.to_str().ok_or("列表必须是有效文本")?.split(',') {
        let value = number(&OsString::from(part), label)?;
        if !seen.insert(value) {
            return Err(format!("{label}列表包含重复值"));
        }
    }
    Ok(seen.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_flags_expand_into_one_plan_and_reject_conflicts() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        let (input, apply) =
            parse("unequip", &args(&["--ships", "123,456", "--all-slots"])).unwrap();
        assert!(!apply);
        assert_eq!(input.load().unwrap().actions.len(), 10);
        let (input, apply) = parse(
            "equip",
            &args(&[
                "--ships",
                "123,456",
                "--slot",
                "2",
                "--family",
                "1000",
                "--policy",
                "warehouse-compose",
                "--level",
                "3",
                "--apply",
            ]),
        )
        .unwrap();
        assert!(apply);
        assert_eq!(input.load().unwrap().actions.len(), 2);
        assert!(parse("compose", &args(&["5001", "2"])).is_ok());
        for values in [
            vec!["--ships", "123,123", "--all-slots"],
            vec!["--ships", "123", "--all-slots", "--slots", "1"],
            vec!["--ships", "123", "--unknown", "1"],
        ] {
            assert!(parse("unequip", &args(&values)).is_err());
        }
    }

    #[test]
    fn explicit_execution_and_precise_sources_are_preserved() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        let (input, apply) = parse("equip", &args(&["123", "2", "warehouse:456"])).unwrap();
        assert!(!apply);
        assert_eq!(
            input.load().unwrap().actions,
            vec![DirectAction::Equip {
                target: DirectShipSlot {
                    ship_id: 123,
                    slot_index: 2
                },
                source: DirectEquipmentSource::Warehouse { config_id: 456 }
            }]
        );
        assert!(
            parse("enhance", &args(&["ship:123:2", "10", "1", "--apply"]))
                .unwrap()
                .1
        );
        assert!(
            parse(
                "dismantle",
                &args(&["warehouse:456", "2", "--apply", "--apply"])
            )
            .is_err()
        );
        assert!(parse("equip", &args(&["123", "2", "456"])).is_err());
    }
}

//! 将独立查询的严格选项转换为共享请求。
use azur_lane_workbook::application::{GameQuery, GameQueryKind as Kind, GameQueryOptions};
use std::{collections::BTreeSet, ffi::OsString};

pub(crate) fn parse(kind: Kind, args: &[OsString]) -> Result<GameQuery, String> {
    let mut ids = Vec::new();
    let mut fields = Vec::new();
    let mut o = GameQueryOptions::default();
    let mut seen = BTreeSet::new();
    let mut i = 0;
    let ship = kind == Kind::Ships;
    let equipment = matches!(kind, Kind::Equipment | Kind::CatalogEquipment);
    while i < args.len() {
        let key = args[i].to_str().ok_or("查询参数必须为有效文本")?;
        if !seen.insert(key) {
            return Err(format!("参数重复: {key}"));
        }
        i += 1;
        match key {
            "--full" => {
                fields = kind.fields().iter().map(|s| (*s).into()).collect();
                continue;
            }
            "--desc" => {
                o.descending = true;
                continue;
            }
            "--available" | "--available-only" if kind == Kind::Recipes => {
                o.available = true;
                o.available_only = key == "--available-only";
                continue;
            }
            _ => {}
        }
        let value = args
            .get(i)
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("{key} 缺少有效值"))?;
        i += 1;
        let number = || {
            value
                .parse::<u64>()
                .map_err(|_| format!("{key} 必须为非负整数"))
        };
        match key {
            "--ids" if kind != Kind::Resources => {
                ids = value
                    .split(',')
                    .map(|s| s.parse().map_err(|_| "ID 必须为正整数".to_owned()))
                    .collect::<Result<_, _>>()?
            }
            "--ship-type" if kind == Kind::Technology => o.ship_type = Some(number()?),
            "--fields" => fields = value.split(',').map(str::to_owned).collect(),
            "--name"
                if matches!(
                    kind,
                    Kind::Ships
                        | Kind::Equipment
                        | Kind::CatalogShips
                        | Kind::CatalogEquipment
                        | Kind::Items
                ) =>
            {
                o.name = Some(value.into())
            }
            "--type" if ship || equipment || kind == Kind::CatalogShips => {
                o.object_type = Some(value.into())
            }
            "--nation" if ship || equipment || kind == Kind::CatalogShips => {
                o.nation = Some(value.into())
            }
            "--rarity" if ship || equipment || kind == Kind::CatalogShips => {
                o.rarity = Some(number()?)
            }
            "--level-min" if ship || equipment => o.level_min = Some(number()?),
            "--level-max" if ship || equipment => o.level_max = Some(number()?),
            "--locked" if ship => {
                o.locked = Some(value.parse().map_err(|_| "--locked 只接受 true 或 false")?)
            }
            "--fleet" if ship => o.fleet = Some(number()?),
            "--ship" if kind == Kind::Equipment => o.ship = Some(number()?),
            "--slot" if ship || kind == Kind::Equipment => o.slot = Some(number()?),
            "--family" if equipment => o.family = Some(number()?),
            "--location"
                if kind == Kind::Equipment && matches!(value, "warehouse" | "equipped") =>
            {
                o.location = Some(value.into())
            }
            "--equipment" if kind == Kind::Recipes => o.equipment = Some(number()?),
            "--level" if kind == Kind::CatalogSkills => {
                o.skill_level = value.parse().map_err(|_| "技能等级无效")?
            }
            "--sort" => o.sort = Some(value.into()),
            "--limit" => o.limit = Some(value.parse().map_err(|_| "limit 超出范围")?),
            "--offset" => o.offset = value.parse().map_err(|_| "offset 超出范围")?,
            _ => {
                return Err(format!(
                    "此查询不支持参数或参数值: {key} {value}；使用命令 --help 查看用法"
                ));
            }
        }
    }
    if seen.contains("--fields") && seen.contains("--full") {
        return Err("--fields 与 --full 不能同时使用".into());
    }
    if seen.contains("--available") && seen.contains("--available-only") {
        return Err("--available 与 --available-only 不能同时使用".into());
    }
    if o.descending && o.sort.is_none() {
        return Err("--desc 需要 --sort".into());
    }
    if kind == Kind::Equipment && o.slot.is_some() && o.ship.is_none() {
        return Err("装备 --slot 需要 --ship".into());
    }
    if kind == Kind::CatalogSkills
        && (ids.is_empty() || (seen.contains("--level") && o.skill_level == 0))
    {
        return Err("技能查询需要 --ids，等级必须大于零".into());
    }
    if kind == Kind::Recipes
        && o.available
        && !seen.contains("--fields")
        && !seen.contains("--full")
    {
        fields = kind
            .default_fields()
            .iter()
            .map(|s| (*s).into())
            .chain(std::iter::once("max_count".into()))
            .collect();
    }
    GameQuery::new(kind, ids, fields)?.with_options(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query(kind: Kind, args: &[&str]) -> Result<GameQuery, String> {
        parse(kind, &args.iter().map(OsString::from).collect::<Vec<_>>())
    }

    #[test]
    fn query_flags_validate_combinations_before_connecting() {
        for (kind, args) in [
            (Kind::Ships, vec!["--full", "--fields", "level"]),
            (Kind::Ships, vec!["--desc"]),
            (Kind::Equipment, vec!["--slot", "2"]),
            (Kind::Ships, vec!["--level-min", "20", "--level-max", "10"]),
            (Kind::Ships, vec!["--locked", "yes"]),
            (Kind::Recipes, vec!["--name", "x"]),
            (Kind::Resources, vec!["--ids", "1"]),
            (Kind::CatalogSkills, vec![]),
            (Kind::CatalogSkills, vec!["--ids", "1", "--level", "0"]),
            (Kind::Ships, vec!["--limit", "1", "--limit", "2"]),
        ] {
            assert!(query(kind, &args).is_err(), "{kind:?} {args:?}");
        }
        let q = query(
            Kind::Equipment,
            &[
                "--ship",
                "1",
                "--slot",
                "2",
                "--fields",
                "name",
                "--sort",
                "enhance_level",
                "--desc",
            ],
        )
        .unwrap();
        assert_eq!(q.options().slot, Some(2));
        assert_eq!(q.fields(), &["name"]);
        assert!(
            query(Kind::Recipes, &["--available-only"])
                .unwrap()
                .fields()
                .contains(&"max_count".into())
        );
    }
}

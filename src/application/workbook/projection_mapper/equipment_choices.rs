//! 按实际来源展示适配装备及数量，并共享相同槽位的下拉字典。

use super::{
    ProjectionIndex, WorkbookProjectionBuilder, WorkbookProjectionError, blank,
    compose_availability, family_compose_recipe, integer, text,
};
use crate::domain::GameState;
use std::collections::BTreeMap;

type Choices = Vec<(u64, String, String)>;
type Targets = Vec<(u64, u8)>;

pub(super) fn project_equipment_choices(
    state: &GameState,
    index: &ProjectionIndex<'_>,
    builder: &mut WorkbookProjectionBuilder,
) -> Result<(), WorkbookProjectionError> {
    let composable = builder.directly_composable_equipment_configs();
    let mut sources: BTreeMap<_, Vec<(String, String)>> = BTreeMap::new();
    for stack in state
        .equipment_inventory()
        .warehouse()
        .iter()
        .filter(|stack| stack.quantity() > 0)
    {
        sources.entry(stack.config_id()).or_default().push((
            format!("warehouse:{}", stack.config_id().get()),
            format!("仓库×{}", stack.quantity()),
        ));
    }
    let mut ship_names = BTreeMap::new();
    for ship in state.ships().ships() {
        *ship_names.entry(ship.identity().name()).or_insert(0_usize) += 1;
    }
    for ship in state.ships().ships() {
        let ship_name = if ship_names[ship.identity().name()] > 1 {
            format!(
                "{}#{}",
                ship.identity().name(),
                ship.identity().instance_id().get()
            )
        } else {
            ship.identity().name().to_owned()
        };
        for slot in ship.slots() {
            if let Some(equipment) = slot.equipment() {
                sources.entry(equipment.config_id()).or_default().push((
                    format!(
                        "ship:{}:{}",
                        ship.identity().instance_id().get(),
                        slot.index().get()
                    ),
                    format!("{ship_name}·槽{}", slot.index().get()),
                ));
            }
        }
    }
    let mut equipment_names = BTreeMap::new();
    for family in state.equipment_catalog().families() {
        for config in family.configs() {
            *equipment_names
                .entry((
                    config.identity().name(),
                    config.classification().tech_level(),
                    config.enhancement().level().get(),
                ))
                .or_insert(0_usize) += 1;
        }
    }
    let mut available = Vec::new();
    for family in state.equipment_catalog().families() {
        for config in family.configs() {
            let id = config.identity().config_id();
            let level = config.enhancement().level().get();
            let enhancement = if level == 0 {
                String::new()
            } else {
                format!(" +{level}")
            };
            let mut name = format!(
                "{} T{}{enhancement}",
                config.identity().name(),
                config.classification().tech_level()
            );
            if equipment_names[&(
                config.identity().name(),
                config.classification().tech_level(),
                level,
            )] > 1
            {
                name.push_str(&format!(" #{}", id.get()));
            }
            for (source, description) in sources.get(&id).into_iter().flatten() {
                available.push((config, source.clone(), format!("{description}｜{name}")));
            }
            if composable.contains(&id.get().to_string()) {
                let recipe = family_compose_recipe(family, index)?;
                if let Some(count) =
                    compose_availability(state, &format!("equipment:{}", id.get()), recipe)?
                        .actual
                        .filter(|count| *count > 0)
                {
                    available.push((
                        config,
                        "compose".to_owned(),
                        format!("合成×{count}｜{name}"),
                    ));
                }
            }
        }
    }
    available.sort_by_key(|(config, source, _)| {
        (
            if source.starts_with("warehouse:") {
                0
            } else if source == "compose" {
                1
            } else {
                2
            },
            config.identity().family_id().get(),
            config.identity().config_id().get(),
            source.clone(),
        )
    });
    let mut by_rules: BTreeMap<(u64, Vec<u64>), Choices> = BTreeMap::new();
    let mut groups: BTreeMap<Choices, Targets> = BTreeMap::new();
    for ship in state.ships().ships() {
        let ship_type = ship.classification().ship_type().id();
        for slot in ship.slots() {
            let mut types = slot.allowed_equipment_type_ids().to_vec();
            types.sort_unstable();
            types.dedup();
            let choices = by_rules
                .entry((ship_type, types.clone()))
                .or_insert_with(|| {
                    available
                        .iter()
                        .filter(|(config, _, _)| config.can_be_equipped_by(ship_type, &types))
                        .map(|(config, source, label)| {
                            (
                                config.identity().family_id().get(),
                                source.clone(),
                                label.clone(),
                            )
                        })
                        .collect()
                });
            groups
                .entry(choices.clone())
                .or_default()
                .push((ship.identity().instance_id().get(), slot.index().get()));
        }
    }
    for (index, (choices, targets)) in groups.into_iter().enumerate() {
        let category = format!("equipment_choice_{index:010}");
        // 字典按行引用排序，操作组排在装备组之前。
        for (order, (value, label)) in [("dismantle", "拆解"), ("empty", "卸下")]
            .into_iter()
            .enumerate()
        {
            push_row(
                builder,
                format!("{category}:0:action:{value}"),
                &category,
                text(value),
                text(label),
                blank(),
                order as i64 + 1,
            )?;
        }
        for (order, (family, source, label)) in choices.into_iter().enumerate() {
            push_row(
                builder,
                format!("{category}:1:{order:020}"),
                &category,
                text(format!("{family}|{source}")),
                text(label),
                blank(),
                order as i64 + 3,
            )?;
        }
        for (ship, slot) in targets {
            push_row(
                builder,
                format!("equipment_target:{ship:020}:{slot}"),
                "equipment_target",
                text(slot),
                text(&category),
                text(format!("ship:{ship}")),
                i64::from(slot),
            )?;
        }
    }
    Ok(())
}

fn push_row(
    builder: &mut WorkbookProjectionBuilder,
    reference: String,
    category: &str,
    stable_value: super::ProjectionValue,
    label: super::ProjectionValue,
    object: super::ProjectionValue,
    order: i64,
) -> Result<(), WorkbookProjectionError> {
    builder.push_row(
        "dictionaries",
        reference,
        projection_values! {
            "category_key" => text(category), "stable_value" => stable_value,
            "display_label" => label, "object_ref" => object, "description" => blank(),
            "layout_hash" => blank(), "order" => integer(order),
        },
    )
}

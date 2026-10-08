//! 负责原始运行态记录的稳定分块与工作表投影。

use super::{
    RAW_JSON_CHUNK_UTF16_UNITS, WorkbookProjectionBuilder, WorkbookProjectionError, integer,
    integer_usize, json_fragment, text,
};
use crate::domain::{GameState, RawRecord, RawRecordKey};

pub(super) fn project_raw_data(
    state: &GameState,
    builder: &mut WorkbookProjectionBuilder,
) -> Result<(), WorkbookProjectionError> {
    let raw_records = state.raw_records();
    for record in raw_records.records() {
        let (entity_type, entity_id, source_ref) = raw_record_identity(record);
        let chunks = split_raw_json(record.canonical_json());
        let chunk_count = chunks.len();
        for (chunk_index, chunk) in chunks.into_iter().enumerate() {
            let one_based_index = chunk_index.checked_add(1).ok_or_else(|| {
                WorkbookProjectionError::ArithmeticOverflow {
                    sheet_key: "raw_data",
                    object_ref: source_ref.clone(),
                    operation: "原始数据分块序号递增",
                }
            })?;
            let object_ref = format!("{source_ref}:chunk:{one_based_index}");
            builder.push_row(
                "raw_data",
                &object_ref,
                projection_values![
                    "entity_type" => text(entity_type),
                    "entity_id" => text(&entity_id),
                    "content_sha256" => text(record.content_sha256()),
                    "source_content_sha256" => text(raw_records.source_content_sha256()),
                    "source_ref" => text(&source_ref),
                    "chunk_index" => integer_usize("raw_data", &object_ref, "chunk_index", one_based_index)?,
                    "chunk_count" => integer_usize("raw_data", &object_ref, "chunk_count", chunk_count)?,
                    "schema_version" => integer(i64::from(raw_records.schema_version())),
                    "canonical_json_chunk" => json_fragment(chunk),
                ],
            )?;
        }
    }
    Ok(())
}

fn raw_record_identity(record: &RawRecord) -> (&'static str, String, String) {
    match record.key() {
        RawRecordKey::EquipmentConfig(config_id) => (
            "equipment_config",
            config_id.get().to_string(),
            format!("equipment_config:{}", config_id.get()),
        ),
        RawRecordKey::EquipmentRecipe(recipe_id) => (
            "equipment_recipe",
            recipe_id.to_string(),
            format!("equipment_recipe:{recipe_id}"),
        ),
        RawRecordKey::EquipmentReferenceNames => (
            "equipment_reference_names",
            "all".to_owned(),
            "equipment_reference_names".to_owned(),
        ),
        RawRecordKey::EquipmentWeapon(weapon_id) => (
            "equipment_weapon",
            weapon_id.to_string(),
            format!("equipment_weapon:{weapon_id}"),
        ),
        RawRecordKey::EquipmentSkill { skill_id, level } => (
            "equipment_skill",
            format!("{skill_id}:{level}"),
            format!("equipment_skill:{skill_id}:{level}"),
        ),
        RawRecordKey::ShipCatalog {
            table_key,
            record_id,
        } => (
            "ship_catalog",
            format!("{table_key}:{record_id}"),
            format!("{table_key}:{record_id}"),
        ),
        RawRecordKey::ShipSkill { skill_id, level } => (
            "ship_skill",
            format!("{skill_id}:{level}"),
            format!("ship_skill:{skill_id}:{level}"),
        ),
    }
}

pub(super) fn split_raw_json(value: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut utf16_units = 0;
    for character in value.chars() {
        let character_units = character.len_utf16();
        if utf16_units + character_units > RAW_JSON_CHUNK_UTF16_UNITS && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            utf16_units = 0;
        }
        current.push(character);
        utf16_units += character_units;
        if utf16_units == RAW_JSON_CHUNK_UTF16_UNITS {
            chunks.push(std::mem::take(&mut current));
            utf16_units = 0;
        }
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }
    chunks
}

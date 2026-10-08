//! 覆盖工作簿投影写入、公式和结构化校验的单元测试。

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use calamine::{Data, Range};
use quick_xml::events::{BytesEnd, BytesText, Event};
use quick_xml::{Reader, Writer};
use zip::ZipArchive;

use crate::adapters::device::game_state_mapper::golden_fixture::{
    formula_like_text_game_state, golden_game_state,
    golden_game_state_with_only_composable_equipment, golden_game_state_with_technology,
    golden_game_state_with_unowned_materials,
};
use crate::adapters::workbook::load_workbook_layout;
use crate::adapters::workbook::package::{PackageSnapshot, parse_relationships, rewrite_package};
use crate::adapters::workbook::reference::inspection::{
    worksheet_part_name, worksheet_relationships_name,
};
use crate::adapters::workbook::ship_wiki::ship_wiki_url;
use crate::application::{WorkbookLayout, WorkbookProjectionV4, project_game_state_to_workbook};
use suzushiro_content_digest::sha256_bytes;

use super::{
    MAX_EXCEL_CELL_UTF16_UNITS, assert_datetime_cell, assert_number_cell,
    build_projection_workbook_bytes, excel_datetime_from_unix_millis, validate_cell_text,
    verify_headers_and_extent, verify_projection_workbook,
};

const GENERATED_AT_UNIX_MILLIS: i64 = 1_700_000_000_123;

#[test]
fn writes_and_reloads_every_golden_projection_value() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("data-workbook.xlsx");

    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let evidence = verify_projection_workbook(path, &build.bytes, &layout, &projection).unwrap();

    let inventory_sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "equipment_inventory")
        .unwrap();
    assert_eq!(inventory_sheet.freeze_cell(), Some("C2"));
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let mut archive = ZipArchive::new(Cursor::new(&build.bytes)).unwrap();
    let mut comments = String::new();
    for index in 0..archive.len() {
        let mut part = archive.by_index(index).unwrap();
        if part.name().starts_with("xl/comments") && part.name().ends_with(".xml") {
            std::io::Read::read_to_string(&mut part, &mut comments).unwrap();
        }
    }
    assert!(comments.contains("此表为游戏数据快照，不会实时同步"));
    assert!(comments.contains(&GENERATED_AT_UNIX_MILLIS.to_string()));
    let part = worksheet_part_name(&package, inventory_sheet.display_name()).unwrap();
    let xml = std::str::from_utf8(package.part(&part).unwrap()).unwrap();
    assert!(xml.contains("xSplit=\"2\""));
    assert!(xml.contains("ySplit=\"1\""));
    assert!(xml.contains("topLeftCell=\"C2\""));
    assert_eq!(build.generated_sheets, 8);
    super::super::rendering::assert_fixed_row_heights(&build.bytes, build.generated_sheets);
    assert_eq!(build.generated_fields, 180);
    assert_eq!(build.projected_rows, 23);
    assert_eq!(build.dictionary_rows, 67);
    assert_eq!(build.schema_rows, 403);
    assert_eq!(build.generated_at_unix_millis, GENERATED_AT_UNIX_MILLIS);
    assert_eq!(evidence.projected_rows, build.projected_rows);
    assert_eq!(evidence.dictionary_rows, build.dictionary_rows);
    assert_eq!(evidence.schema_rows, build.schema_rows);
    assert_eq!(evidence.generated_at_unix_millis, GENERATED_AT_UNIX_MILLIS);
    assert_eq!(sha256_bytes(&build.bytes).len(), 64);
}

#[test]
fn generated_columns_fit_content_and_all_cell_alignments_are_centered() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("column-layout.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    assert!(fields.iter().all(|field| field.stable_key() != "group_id"));
    assert_eq!(
        fields
            .iter()
            .take(38)
            .map(|field| field.display_name())
            .collect::<Vec<_>>(),
        [
            "舰船实例ID",
            "舰船名称",
            "获取方式",
            "阵营",
            "舰船类型",
            "装甲类型",
            "是否锁定",
            "编队状态",
            "当前星级",
            "星级上限",
            "当前等级",
            "等级上限",
            "本级已获经验",
            "升级所需经验",
            "累计经验",
            "科技加成",
            "获得科技",
            "满星科技",
            "120级科技",
            "心情",
            "当前好感",
            "好感上限",
            "是否誓约",
            "获取时间（UTC）",
            "誓约时间（UTC）",
            "综合性能",
            "总油耗",
            "耐久（基础/装备/其他/最终）",
            "炮击（基础/装备/其他/最终）",
            "航空（基础/装备/其他/最终）",
            "雷击（基础/装备/其他/最终）",
            "装填（基础/装备/其他/最终）",
            "命中（基础/装备/其他/最终）",
            "机动（基础/装备/其他/最终）",
            "防空（基础/装备/其他/最终）",
            "幸运（基础/装备/其他/最终）",
            "航速（基础/装备/其他/最终）",
            "反潜（基础/装备/其他/最终）",
        ]
    );
    let part = worksheet_part_name(&package, sheet.display_name()).unwrap();
    let mut reader = Reader::from_reader(package.part(&part).unwrap());
    let mut fitted = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Empty(element) if element.local_name().as_ref() == b"col" => {
                if attribute_text(&element, b"bestFit").as_deref() == Some("1") {
                    for index in attribute_u32(&element, b"min")..=attribute_u32(&element, b"max") {
                        fitted.push(fields[index as usize - 1].stable_key());
                    }
                    assert!(
                        attribute_text(&element, b"width")
                            .unwrap()
                            .parse::<f64>()
                            .unwrap()
                            > 0.0
                    );
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(fitted.len(), 28);
    for key in [
        "instance_id",
        "ship_type",
        "armor_type",
        "current_stars",
        "maximum_stars",
        "level",
        "maximum_level",
        "experience_in_level",
        "total_experience",
        "next_level_experience",
        "energy",
        "intimacy",
        "intimacy_maximum",
        "combat_power",
        "oil_total",
        "locked",
        "proposed",
    ] {
        assert!(fitted.contains(&key), "{key}");
    }
    assert_eq!(
        fitted.iter().filter(|key| key.starts_with("stat_")).count(),
        11
    );
    let mut reader = Reader::from_reader(package.part("xl/styles.xml").unwrap());
    let mut in_cell_xfs = false;
    let mut alignments = 0;
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = false;
            }
            Event::Empty(element)
                if in_cell_xfs && element.local_name().as_ref() == b"alignment" =>
            {
                assert_eq!(
                    attribute_text(&element, b"horizontal").as_deref(),
                    Some("center")
                );
                assert_eq!(
                    attribute_text(&element, b"vertical").as_deref(),
                    Some("center")
                );
                alignments += 1;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert!(alignments > 0);
}

#[test]
fn distinguishes_read_only_and_editable_cells_in_owned_ship_rows() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("ship-colors.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap();
    let row = projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .position(|row| row.object_ref().starts_with("ship:"))
        .unwrap() as u32
        + 2;
    let part = worksheet_part_name(&package, sheet.display_name()).unwrap();
    let indexes = worksheet_row_style_indexes(package.part(&part).unwrap(), row);
    let xml = package.part("xl/styles.xml").unwrap();
    let records = cell_style_records(xml);
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    for (key, color, locked) in [
        ("name", "FFF2F2F2", true),
        ("level", "FFF2F2F2", true),
        ("technology_bonus", "FFF2F2F2", false),
        ("slot_1_target_equipment_family", "FFE2F0D9", false),
    ] {
        let column = fields
            .iter()
            .position(|field| field.stable_key() == key)
            .unwrap();
        let record = &records[indexes[column] as usize];
        assert_eq!(record.fill_id, fill_index(xml, color), "{key}");
        assert_eq!(record.locked, locked, "{key}");
    }
    for (field, index) in fields.iter().zip(indexes) {
        let record = &records[index as usize];
        let color = if record.locked || field.stable_key() == "technology_bonus" {
            "FFF2F2F2"
        } else {
            "FFE2F0D9"
        };
        assert_eq!(record.fill_id, fill_index(xml, color));
    }
}

#[test]
fn writes_unowned_rows_with_the_configured_fill_and_locked_cells() {
    assert_unowned_inventory_row_fill(
        &golden_game_state_with_unowned_materials(0),
        true,
        "FFF2F2F2",
    );
    assert_unowned_inventory_row_fill(
        &golden_game_state_with_unowned_materials(30),
        true,
        "FFF2F2F2",
    );
    assert_unowned_inventory_row_fill(
        &golden_game_state_with_only_composable_equipment(0),
        false,
        "FFDDEBF7",
    );
    assert_unowned_inventory_row_fill(
        &golden_game_state_with_only_composable_equipment(30),
        false,
        "FFF2F2F2",
    );
}

fn assert_unowned_inventory_row_fill(
    state: &crate::domain::GameState,
    has_family_owned_enhance_distribution: bool,
    fill: &str,
) {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(state).unwrap();
    let path = Path::new("unowned-row.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    verify_projection_workbook(path, &build.bytes, &layout, &projection).unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "equipment_inventory")
        .unwrap();
    let projection_sheet = projection.sheet("equipment_inventory").unwrap();
    let (row_index, unowned) = projection_sheet
        .rows()
        .iter()
        .enumerate()
        .find(|(_, row)| row.object_ref().starts_with("unowned:"))
        .unwrap();
    let distribution = match unowned.value("family_owned_enhance_distribution") {
        Some(crate::application::WorkbookProjectionValue::Text(value)) => value.as_str(),
        _ => "",
    };
    assert_eq!(
        !distribution.is_empty(),
        has_family_owned_enhance_distribution
    );
    let row = u32::try_from(row_index + 2).unwrap();
    let worksheet_part = worksheet_part_name(&package, sheet.display_name()).unwrap();
    let worksheet_xml = package.part(&worksheet_part).unwrap();
    let style_indexes = worksheet_row_style_indexes(worksheet_xml, row);
    let style_records = cell_style_records(package.part("xl/styles.xml").unwrap());
    let styles = package.part("xl/styles.xml").unwrap();
    let expected_fill = fill_index(styles, fill);

    assert_eq!(
        style_indexes.len(),
        layout.generated_fields_for_sheet(sheet.stable_key()).len(),
        "零库存行的全部物理单元格都必须带格式"
    );
    assert!(style_indexes.iter().all(|index| {
        style_records
            .get(*index as usize)
            .is_some_and(|record| record.fill_id == expected_fill && record.locked)
    }));
    assert!(!String::from_utf8_lossy(worksheet_xml).contains("<sheetProtection"));
}

#[test]
fn produces_identical_bytes_for_the_same_semantics_and_time() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();

    let first = build_projection_workbook_bytes(
        Path::new("first.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
    let second = build_projection_workbook_bytes(
        Path::new("second.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();

    assert_eq!(first.bytes, second.bytes);
    assert_eq!(
        first.workbook_semantic_sha256,
        second.workbook_semantic_sha256
    );
}

#[test]
fn writes_valid_utc_timestamps_to_core_document_properties() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("core-properties-timestamp.xlsx");

    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let core_properties = std::str::from_utf8(package.part("docProps/core.xml").unwrap()).unwrap();

    assert_eq!(core_properties.matches("2023-11-14T22:13:20Z").count(), 2);
    assert!(!core_properties.contains("0-00-00T00:00:00Z"));
}

#[test]
fn follows_layout_column_order_instead_of_projection_key_order() {
    let base = default_layout();
    let mut fields = base.fields().to_vec();
    let indexes: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.sheet_key() == "equipment_inventory")
        .map(|(index, _)| index)
        .take(2)
        .collect();
    fields.swap(indexes[0], indexes[1]);
    let layout = WorkbookLayout::new(
        base.schema_version(),
        base.template_name().to_owned(),
        base.purpose().to_owned(),
        base.sheets().to_vec(),
        fields,
        base.enum_options().to_vec(),
        base.styles().to_vec(),
        "0".repeat(64),
    )
    .unwrap();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();

    let build = build_projection_workbook_bytes(
        Path::new("reordered.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();

    verify_projection_workbook(
        Path::new("reordered.xlsx"),
        &build.bytes,
        &layout,
        &projection,
    )
    .unwrap();
}

#[test]
fn writes_formula_like_external_text_without_formula_nodes() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&formula_like_text_game_state()).unwrap();

    let build = build_projection_workbook_bytes(
        Path::new("formula-like-text.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();

    verify_projection_workbook(
        Path::new("formula-like-text.xlsx"),
        &build.bytes,
        &layout,
        &projection,
    )
    .unwrap();
}

#[test]
fn enforces_excel_cell_length_in_utf16_units() {
    let accepted = "a".repeat(MAX_EXCEL_CELL_UTF16_UNITS);
    let rejected = "a".repeat(MAX_EXCEL_CELL_UTF16_UNITS + 1);
    let supplementary = "\u{1f600}".repeat(MAX_EXCEL_CELL_UTF16_UNITS / 2 + 1);

    validate_cell_text("accepted", &accepted).unwrap();
    assert!(validate_cell_text("rejected", &rejected).is_err());
    assert!(validate_cell_text("supplementary", &supplementary).is_err());
}

#[test]
fn rejects_non_blank_values_in_an_empty_table_placeholder_row() {
    let layout = default_layout();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "check_results")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let last_column = u32::try_from(fields.len() - 1).unwrap();
    let mut range = Range::new((0, 0), (1, last_column));
    for (column, field) in fields.iter().enumerate() {
        range.set_value(
            (0, u32::try_from(column).unwrap()),
            Data::String(field.display_name().to_owned()),
        );
    }
    range.set_value((1, 0), Data::String("polluted".to_owned()));

    let error = verify_headers_and_extent(sheet, &fields, 0, &range).unwrap_err();
    assert!(error.to_string().contains("empty_placeholder"));
}

#[test]
fn compares_large_numbers_and_dates_without_relative_tolerance() {
    let actual = Data::Float(9_007_199_254_740_990.0);
    assert!(assert_number_cell(Some(&actual), 9_007_199_254_740_991.0, "large_integer").is_err());

    let actual_datetime = Data::Float(
        excel_datetime_from_unix_millis(GENERATED_AT_UNIX_MILLIS + 1_000)
            .unwrap()
            .to_excel(),
    );
    assert!(
        assert_datetime_cell(
            Some(&actual_datetime),
            GENERATED_AT_UNIX_MILLIS,
            "created_at"
        )
        .is_err()
    );
}

#[test]
fn rejects_tampered_dictionary_names_and_data_validation_formulas() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("tampered-validation.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();

    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let workbook_xml = replace_first_element_text(
        package.part("xl/workbook.xml").unwrap(),
        b"definedName",
        "BROKEN_RANGE",
    );
    let mutated_names = replace_package_part(&build.bytes, "xl/workbook.xml", workbook_xml, path);
    assert!(verify_projection_workbook(path, &mutated_names, &layout, &projection).is_err());

    let validation_sheet = layout
        .sheets()
        .iter()
        .find(|sheet| {
            layout
                .generated_fields_for_sheet(sheet.stable_key())
                .iter()
                .any(|field| {
                    matches!(
                        field.editor(),
                        crate::application::LayoutEditor::Boolean
                            | crate::application::LayoutEditor::Enumeration
                    )
                })
        })
        .unwrap();
    let worksheet_part = worksheet_part_name(&package, validation_sheet.display_name()).unwrap();
    let worksheet_xml = replace_first_element_text(
        package.part(&worksheet_part).unwrap(),
        b"formula1",
        "BROKEN_VALIDATION",
    );
    let mutated_validation =
        replace_package_part(&build.bytes, &worksheet_part, worksheet_xml, path);
    assert!(verify_projection_workbook(path, &mutated_validation, &layout, &projection).is_err());
}

#[test]
fn rejects_missing_physical_blank_placeholder_row() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("missing-physical-row.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "check_results")
        .unwrap();
    let worksheet_part = worksheet_part_name(&package, sheet.display_name()).unwrap();
    let worksheet_xml = remove_physical_row(package.part(&worksheet_part).unwrap(), 2);
    let mutated = replace_package_part(&build.bytes, &worksheet_part, worksheet_xml, path);

    let error = verify_projection_workbook(path, &mutated, &layout, &projection)
        .err()
        .expect("缺失物理行必须被拒绝");

    assert!(error.to_string().contains("物理行"));
}

#[test]
fn rejects_a_cell_reference_that_does_not_match_its_parent_row() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("parent-row-mismatch.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "check_results")
        .unwrap();
    let worksheet_part = worksheet_part_name(&package, sheet.display_name()).unwrap();
    let worksheet_xml =
        replace_cell_reference_in_row(package.part(&worksheet_part).unwrap(), 2, "A2", "A1");
    let mutated = replace_package_part(&build.bytes, &worksheet_part, worksheet_xml, path);

    let error = verify_projection_workbook(path, &mutated, &layout, &projection)
        .err()
        .expect("父行不匹配的物理单元格必须被拒绝");

    assert!(error.to_string().contains("父行"));
}

#[test]
fn rejects_very_hidden_workbook_sheet_state() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let path = Path::new("very-hidden-sheet.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let workbook_xml =
        String::from_utf8(package.part("xl/workbook.xml").unwrap().to_vec()).unwrap();
    assert!(workbook_xml.contains("state=\"hidden\""));
    let workbook_xml = workbook_xml
        .replacen("state=\"hidden\"", "state=\"veryHidden\"", 1)
        .into_bytes();
    let mutated = replace_package_part(&build.bytes, "xl/workbook.xml", workbook_xml, path);

    let error = verify_projection_workbook(path, &mutated, &layout, &projection)
        .err()
        .expect("veryHidden 状态必须被拒绝");

    assert!(error.to_string().contains("veryHidden"));
}

fn replace_first_element_text(source: &[u8], local_name: &[u8], replacement: &str) -> Vec<u8> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut replaced = false;
    loop {
        let event = reader.read_event().unwrap();
        match event {
            Event::Start(element) if !replaced && element.local_name().as_ref() == local_name => {
                let qualified_name = String::from_utf8(element.name().as_ref().to_vec()).unwrap();
                writer.write_event(Event::Start(element.clone())).unwrap();
                reader.read_to_end(element.name()).unwrap();
                writer
                    .write_event(Event::Text(BytesText::new(replacement)))
                    .unwrap();
                writer
                    .write_event(Event::End(BytesEnd::new(qualified_name)))
                    .unwrap();
                replaced = true;
            }
            Event::Eof => break,
            event => writer.write_event(event).unwrap(),
        }
    }
    assert!(replaced, "测试源部件必须包含目标元素");
    writer.into_inner()
}

fn remove_physical_row(source: &[u8], target_row: u32) -> Vec<u8> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut removed = false;
    loop {
        let event = reader.read_event().unwrap();
        match event {
            Event::Start(element)
                if !removed
                    && element.local_name().as_ref() == b"row"
                    && test_row_number(&element) == target_row =>
            {
                reader.read_to_end(element.name()).unwrap();
                removed = true;
            }
            Event::Empty(element)
                if !removed
                    && element.local_name().as_ref() == b"row"
                    && test_row_number(&element) == target_row =>
            {
                removed = true;
            }
            Event::Eof => break,
            event => writer.write_event(event).unwrap(),
        }
    }
    assert!(removed, "测试工作表必须包含目标物理行");
    writer.into_inner()
}

fn replace_cell_reference_in_row(
    source: &[u8],
    target_row: u32,
    old_reference: &str,
    new_reference: &str,
) -> Vec<u8> {
    let text = String::from_utf8(source.to_vec()).unwrap();
    let row_marker = format!("<row r=\"{target_row}\"");
    let row_start = text
        .find(&row_marker)
        .expect("测试源部件必须包含目标物理行");
    let row_end = text[row_start..]
        .find("</row>")
        .map(|offset| row_start + offset)
        .expect("测试源部件必须包含目标行结束标签");
    let row = &text[row_start..row_end];
    let old = format!("r=\"{old_reference}\"");
    let new = format!("r=\"{new_reference}\"");
    let cell_offset = row.find(&old).expect("目标物理行必须包含目标单元格");
    let absolute_offset = row_start + cell_offset;
    let mut mutated = text;
    mutated.replace_range(absolute_offset..absolute_offset + old.len(), &new);
    mutated.into_bytes()
}

fn test_row_number(element: &quick_xml::events::BytesStart<'_>) -> u32 {
    element
        .attributes()
        .with_checks(false)
        .map(|attribute| attribute.unwrap())
        .find(|attribute| attribute.key.local_name().as_ref() == b"r")
        .map(|attribute| {
            std::str::from_utf8(attribute.value.as_ref())
                .unwrap()
                .parse::<u32>()
                .unwrap()
        })
        .expect("测试工作表行必须包含 r 属性")
}

fn worksheet_row_style_indexes(source: &[u8], target_row: u32) -> Vec<u32> {
    let mut reader = Reader::from_reader(source);
    let mut current_row = None;
    let mut styles = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"row" => {
                current_row = Some(test_row_number(&element));
            }
            Event::End(element) if element.local_name().as_ref() == b"row" => {
                current_row = None;
            }
            Event::Start(element) | Event::Empty(element)
                if current_row == Some(target_row) && element.local_name().as_ref() == b"c" =>
            {
                styles.push(attribute_u32(&element, b"s"));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    styles
}

#[derive(Clone, Copy)]
struct CellStyleRecord {
    fill_id: u32,
    locked: bool,
}

fn cell_style_records(source: &[u8]) -> Vec<CellStyleRecord> {
    let mut reader = Reader::from_reader(source);
    let mut in_cell_xfs = false;
    let mut current = None;
    let mut records = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = false;
            }
            Event::Start(element) if in_cell_xfs && element.local_name().as_ref() == b"xf" => {
                current = Some(CellStyleRecord {
                    fill_id: attribute_u32(&element, b"fillId"),
                    locked: true,
                });
            }
            Event::Empty(element) if in_cell_xfs && element.local_name().as_ref() == b"xf" => {
                records.push(CellStyleRecord {
                    fill_id: attribute_u32(&element, b"fillId"),
                    locked: true,
                });
            }
            Event::Empty(element)
                if current.is_some() && element.local_name().as_ref() == b"protection" =>
            {
                if attribute_text(&element, b"locked").as_deref() == Some("0") {
                    current.as_mut().unwrap().locked = false;
                }
            }
            Event::End(element) if element.local_name().as_ref() == b"xf" => {
                if let Some(record) = current.take() {
                    records.push(record);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    records
}

fn fill_index(source: &[u8], expected_rgb: &str) -> u32 {
    let mut reader = Reader::from_reader(source);
    let mut in_fills = false;
    let mut current = None;
    let mut next_index = 0_u32;
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"fills" => {
                in_fills = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"fills" => {
                in_fills = false;
            }
            Event::Start(element) if in_fills && element.local_name().as_ref() == b"fill" => {
                current = Some(next_index);
                next_index += 1;
            }
            Event::Empty(element)
                if current.is_some() && element.local_name().as_ref() == b"fgColor" =>
            {
                if attribute_text(&element, b"rgb").as_deref() == Some(expected_rgb) {
                    return current.unwrap();
                }
            }
            Event::End(element) if element.local_name().as_ref() == b"fill" => {
                current = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    panic!("样式表缺少背景色 {expected_rgb}")
}

fn attribute_u32(element: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> u32 {
    attribute_text(element, name).unwrap().parse().unwrap()
}

fn attribute_text(element: &quick_xml::events::BytesStart<'_>, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .with_checks(false)
        .map(|attribute| attribute.unwrap())
        .find(|attribute| attribute.key.local_name().as_ref() == name)
        .map(|attribute| String::from_utf8(attribute.value.into_owned()).unwrap())
}

fn replace_package_part(
    source: &[u8],
    part_name: &str,
    replacement: Vec<u8>,
    path: &Path,
) -> Vec<u8> {
    let archive = ZipArchive::new(Cursor::new(source)).unwrap();
    let replacements = BTreeMap::from([(part_name.to_owned(), replacement)]);
    rewrite_package(
        archive,
        Cursor::new(Vec::new()),
        path,
        path,
        &replacements,
        &[],
    )
    .unwrap()
    .into_inner()
}

fn default_layout() -> WorkbookLayout {
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &registry,
    )
    .unwrap()
}

fn worksheet_hyperlinks(package: &PackageSnapshot, sheet_name: &str) -> BTreeMap<String, String> {
    let part = worksheet_part_name(package, sheet_name).unwrap();
    let rels_name = worksheet_relationships_name(&part).unwrap();
    let Ok(rels) = package.part(&rels_name) else {
        return BTreeMap::new();
    };
    let targets: BTreeMap<String, String> = parse_relationships(&rels_name, rels)
        .unwrap()
        .into_iter()
        .map(|relationship| (relationship.id, relationship.target))
        .collect();
    let mut links = BTreeMap::new();
    let mut reader = Reader::from_reader(package.part(&part).unwrap());
    loop {
        match reader.read_event().unwrap() {
            Event::Empty(element) | Event::Start(element)
                if element.local_name().as_ref() == b"hyperlink" =>
            {
                let cell = attribute_text(&element, b"ref").unwrap();
                let relationship_id = attribute_text(&element, b"id").unwrap();
                links.insert(cell, targets[&relationship_id].clone());
            }
            Event::Eof => break,
            _ => {}
        }
    }
    links
}

fn xf_uses_wiki_link_font(styles: &[u8], xf_index: u32) -> bool {
    let font_ids = xf_font_ids(styles);
    let fonts = style_fonts(styles);
    font_ids
        .get(xf_index as usize)
        .and_then(|font_id| fonts.get(*font_id as usize))
        .is_some_and(|font| font.underline && font.rgb.as_deref() == Some("FF1F4E79"))
}

struct StyleFont {
    underline: bool,
    rgb: Option<String>,
}

fn style_fonts(source: &[u8]) -> Vec<StyleFont> {
    let mut reader = Reader::from_reader(source);
    let mut in_fonts = false;
    let mut current = None;
    let mut fonts = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"fonts" => {
                in_fonts = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"fonts" => {
                in_fonts = false;
            }
            Event::Start(element) if in_fonts && element.local_name().as_ref() == b"font" => {
                current = Some(StyleFont {
                    underline: false,
                    rgb: None,
                });
            }
            Event::Empty(element) if current.is_some() && element.local_name().as_ref() == b"u" => {
                current.as_mut().unwrap().underline = true;
            }
            Event::Empty(element)
                if current.is_some() && element.local_name().as_ref() == b"color" =>
            {
                current.as_mut().unwrap().rgb = attribute_text(&element, b"rgb");
            }
            Event::End(element) if element.local_name().as_ref() == b"font" => {
                if let Some(font) = current.take() {
                    fonts.push(font);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    fonts
}

fn xf_font_ids(source: &[u8]) -> Vec<u32> {
    let mut reader = Reader::from_reader(source);
    let mut in_cell_xfs = false;
    let mut font_ids = Vec::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = true;
            }
            Event::End(element) if element.local_name().as_ref() == b"cellXfs" => {
                in_cell_xfs = false;
            }
            Event::Start(element) | Event::Empty(element)
                if in_cell_xfs && element.local_name().as_ref() == b"xf" =>
            {
                font_ids.push(attribute_u32(&element, b"fontId"));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    font_ids
}

fn excel_column_name(index: u16) -> String {
    let mut number = u32::from(index) + 1;
    let mut letters = Vec::new();
    while number > 0 {
        number -= 1;
        letters.push(char::from(b'A' + (number % 26) as u8));
        number /= 26;
    }
    letters.into_iter().rev().collect()
}

#[test]
fn progress_counts_completed_sheets_then_switches_to_unknown_compression() {
    let layout = default_layout();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let mut events = Vec::new();
    let build = super::build_projection_workbook_bytes_with_progress(
        Path::new("progress.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
        &mut |event| events.push(event),
    )
    .unwrap();
    let units: Vec<_> = events.iter().filter_map(|event| event.units).collect();
    let total = build.generated_sheets;
    assert_eq!(
        units,
        (0..=total).map(|done| (done, total)).collect::<Vec<_>>()
    );
    let complete = events
        .iter()
        .position(|event| event.units == Some((total, total)))
        .unwrap();
    assert!(
        events[complete + 1..]
            .iter()
            .all(|event| event.units.is_none())
    );
    assert!(
        events[complete + 1..]
            .iter()
            .any(|event| event.message.contains("压缩"))
    );
    let plain = build_projection_workbook_bytes(
        Path::new("progress.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
    assert_eq!(build.bytes, plain.bytes);
}

#[test]
fn equipment_dropdowns_bind_owned_slots_and_follow_reordered_columns() {
    let base = default_layout();
    let mut fields = base.fields().to_vec();
    let a = fields
        .iter()
        .position(|field| {
            field.sheet_key() == "loadout_plan"
                && field.stable_key() == "slot_1_target_equipment_family"
        })
        .unwrap();
    let b = fields
        .iter()
        .position(|field| {
            field.sheet_key() == "loadout_plan"
                && field.stable_key() == "slot_3_target_equipment_family"
        })
        .unwrap();
    fields.swap(a, b);
    let mut sheets = base.sheets().to_vec();
    let dictionary_index = sheets
        .iter()
        .position(|sheet| sheet.stable_key() == "dictionaries")
        .unwrap();
    let old = &sheets[dictionary_index];
    sheets[dictionary_index] = crate::application::WorkbookSheetLayout::new(
        old.stable_key().to_owned(),
        old.generation(),
        "装备'候选\"字典".to_owned(),
        old.order(),
        old.freeze_cell().map(str::to_owned),
        old.default_filter(),
        old.description().to_owned(),
        old.required(),
    );
    let layout = WorkbookLayout::new(
        base.schema_version(),
        base.template_name().to_owned(),
        base.purpose().to_owned(),
        sheets,
        fields,
        base.enum_options().to_vec(),
        base.styles().to_vec(),
        "0".repeat(64),
    )
    .unwrap();
    let projection = project_game_state_to_workbook(&golden_game_state()).unwrap();
    let sheet = layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "loadout_plan")
        .unwrap();
    let fields = layout.generated_fields_for_sheet(sheet.stable_key());
    let validations =
        super::equipment_choices::equipment_validations(&layout, &projection, &fields).unwrap();
    assert_eq!(validations.len(), 5);
    let dictionary = projection.sheet("dictionaries").unwrap();
    let mut choices = BTreeMap::<String, Vec<String>>::new();
    for row in dictionary.rows() {
        if let (
            Some(crate::application::WorkbookProjectionValue::Text(category)),
            Some(crate::application::WorkbookProjectionValue::Text(label)),
        ) = (row.value("category_key"), row.value("display_label"))
            && category.starts_with("equipment_choice_")
        {
            choices
                .entry(category.clone())
                .or_default()
                .push(label.clone());
        }
    }
    assert!(!choices.is_empty());
    assert!(choices.values().all(
        |labels| labels.iter().filter(|label| *label == "卸下").count() == 1
            && labels.iter().filter(|label| *label == "拆解").count() == 1
    ));
    assert!(choices.values().any(|labels| labels.len() == 2));
    assert!(
        choices
            .values()
            .all(|labels| { labels.starts_with(&["拆解".to_owned(), "卸下".to_owned()]) })
    );

    let col = |slot| {
        fields
            .iter()
            .position(|field| field.stable_key() == format!("slot_{slot}_target_equipment_family"))
            .unwrap() as u16
    };
    assert_eq!(validations[&(1, col(1))], validations[&(1, col(2))]);
    assert_ne!(validations[&(1, col(1))], validations[&(1, col(3))]);
    let path = Path::new("equipment-dropdowns.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    verify_projection_workbook(path, &build.bytes, &layout, &projection).unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let part = worksheet_part_name(&package, sheet.display_name()).unwrap();
    let xml = String::from_utf8(package.part(&part).unwrap().to_vec()).unwrap();
    assert_eq!(xml.matches("INDIRECT(").count(), 5);
    assert!(xml.contains("更换装备或处理当前装备"));
    assert!(xml.contains("拆解：消耗装备"));

    let mutated = replace_package_part(
        &build.bytes,
        &part,
        xml.replacen("INDIRECT(", "INVALID(", 1).into_bytes(),
        path,
    );
    assert!(verify_projection_workbook(path, &mutated, &layout, &projection).is_err());
}

#[test]
fn rejects_missing_skill_coverage_only_when_layout_requests_it() {
    let state = golden_game_state();
    let state = crate::domain::GameState::new(
        state
            .source()
            .clone()
            .with_read_scope(crate::domain::GameReadScope::with_ship_skill_effects(false)),
        state.ships().clone(),
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );
    let projection = project_game_state_to_workbook(&state).unwrap();
    super::validate_layout_projection(&default_layout(), &projection).unwrap();
    let full_layout = crate::adapters::workbook::layout::template::full_test_layout();
    let error = super::validate_layout_projection(&full_layout, &projection).unwrap_err();
    assert!(error.to_string().contains("本次状态未读取"));
}

#[test]
fn technology_cells_round_trip_with_fixed_row_height() {
    let layout = default_layout();
    let state = crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology();
    let projection = project_game_state_to_workbook(&state).unwrap();
    let path = Path::new("technology-workbook.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    verify_projection_workbook(path, &build.bytes, &layout, &projection).unwrap();
    super::super::rendering::assert_fixed_row_heights(&build.bytes, build.generated_sheets);
    use calamine::Reader as _;
    let mut workbook: calamine::Xlsx<Cursor<&[u8]>> =
        calamine::open_workbook_from_rs(Cursor::new(build.bytes.as_slice())).unwrap();
    assert_eq!(build.generated_sheets, 8);
    assert_eq!(build.generated_fields, 180);
    assert!(
        !workbook
            .sheet_names()
            .iter()
            .any(|name| name.contains("耐久") || name.contains("炮击"))
    );
    let range = workbook.worksheet_range("配装计划").unwrap();
    let headers: Vec<_> = range
        .rows()
        .next()
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    let bonus_column = headers.iter().position(|name| name == "科技加成").unwrap();
    assert_eq!(headers[bonus_column + 1], "获得科技");
    assert_eq!(
        range.get((1, bonus_column)).unwrap().to_string(),
        "驱逐、导驱-炮击；驱逐、导驱-耐久"
    );
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let part = worksheet_part_name(&package, "配装计划").unwrap();
    let indexes = worksheet_row_style_indexes(package.part(&part).unwrap(), 2);
    let styles = cell_style_records(package.part("xl/styles.xml").unwrap());
    assert!(!styles[indexes[bonus_column] as usize].locked);
    let name_column = headers.iter().position(|name| name == "舰船名称").unwrap();
    assert_eq!(
        styles[indexes[bonus_column] as usize].fill_id,
        styles[indexes[name_column] as usize].fill_id,
    );
    let table = crate::adapters::workbook::reference::inspection::worksheet_table_part_name(
        &package, &part,
    )
    .unwrap();
    assert!(
        std::str::from_utf8(package.part(&table).unwrap())
            .unwrap()
            .contains("<autoFilter")
    );
    let row = &projection.sheet("loadout_plan").unwrap().rows()[0];
    assert_eq!(
        row.value("technology_get"),
        Some(&crate::application::WorkbookProjectionValue::text(
            "已达成\n科技点 +8\n驱逐／导驱：耐久 +1"
        ))
    );
}

#[test]
fn technology_categories_keep_instances_unique_and_unowned_rows_last() {
    use crate::application::WorkbookProjectionValue as V;
    let state = crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology();
    let projection = project_game_state_to_workbook(&state).unwrap();
    let source = &projection.sheet("loadout_plan").unwrap().rows()[0];
    let owned_ref = source.object_ref().to_owned();
    let mut owned = source.values().clone();
    owned.insert(
        "technology_level".to_owned(),
        V::text("未达成\n科技点 +12\n驱逐／导驱：耐久 +2"),
    );
    let mut unowned = owned.clone();
    unowned.insert("source_type".to_owned(), V::text("unowned"));
    unowned.insert("group_id".to_owned(), V::text("20202"));
    for field in ["instance_id", "level", "maximum_level"] {
        unowned.insert(field.to_owned(), V::Blank);
    }
    let projection = projection
        .with_replaced_rows(BTreeMap::from([(
            "loadout_plan".to_owned(),
            vec![
                ("a-unowned".to_owned(), unowned),
                (owned_ref.clone(), owned),
            ],
        )]))
        .unwrap();
    let layout = crate::adapters::workbook::layout::template::technology_test_layout();
    let outputs = super::technology_views::output_sheets(&layout, &projection).unwrap();
    let groups: Vec<_> = outputs
        .iter()
        .filter(|output| output.layout.stable_key().starts_with("ship_technology:"))
        .collect();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].projection.rows().len(), 2);
    assert_eq!(groups[0].projection.rows()[0].object_ref(), owned_ref);
    assert_eq!(
        groups[0].projection.rows()[1].value("maximum_level"),
        Some(&V::Blank)
    );
    build_projection_workbook_bytes(
        Path::new("technology-unowned.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
}

#[test]
fn technology_selects_highest_stars_then_level_and_keeps_main_instances() {
    use crate::application::WorkbookProjectionValue as V;
    let state = crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology();
    let projection = project_game_state_to_workbook(&state).unwrap();
    let source = &projection.sheet("loadout_plan").unwrap().rows()[0];
    let mut rows = Vec::new();
    for (id, stars, level) in [
        (9001, 4, 125),
        (9002, 5, 100),
        (9003, 5, 120),
        (9004, 5, 120),
    ] {
        let mut values = source.values().clone();
        values.insert("instance_id".to_owned(), V::text(id.to_string()));
        values.insert("current_stars".to_owned(), V::Integer(stars));
        values.insert("level".to_owned(), V::Integer(level));
        rows.push((
            if id == 9001 {
                source.object_ref().to_owned()
            } else {
                format!("test:{id}")
            },
            values,
        ));
    }
    let projection = projection
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    let layout = crate::adapters::workbook::layout::template::technology_test_layout();
    let outputs = super::technology_views::output_sheets(&layout, &projection).unwrap();
    assert_eq!(projection.sheet("loadout_plan").unwrap().rows().len(), 4);
    let groups: Vec<_> = outputs
        .iter()
        .filter(|output| output.layout.stable_key().starts_with("ship_technology:"))
        .collect();
    assert_eq!(groups.len(), 2);
    for group in groups {
        assert_eq!(group.projection.rows().len(), 1);
        assert_eq!(
            group.projection.rows()[0].value("instance_id"),
            Some(&V::text("9003"))
        );
    }
    build_projection_workbook_bytes(
        Path::new("technology-representatives.xlsx"),
        &layout,
        &projection,
        GENERATED_AT_UNIX_MILLIS,
    )
    .unwrap();
}

#[test]
fn technology_status_colors_are_independent_and_read_only() {
    use crate::application::WorkbookProjectionValue as V;
    let state = crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state_with_technology();
    let projection = project_game_state_to_workbook(&state).unwrap();
    let mut rows: Vec<_> = projection
        .sheet("loadout_plan")
        .unwrap()
        .rows()
        .iter()
        .map(|row| (row.object_ref().to_owned(), row.values().clone()))
        .collect();
    rows[0].1.insert(
        "technology_upgrade".to_owned(),
        V::text("未达成\n科技点 +16"),
    );
    rows[0].1.insert(
        "technology_level".to_owned(),
        V::text("状态未获取\n科技点 +12\n驱逐／导驱：炮击 +1"),
    );
    let projection = projection
        .with_replaced_rows(BTreeMap::from([("loadout_plan".to_owned(), rows)]))
        .unwrap();
    let layout = crate::adapters::workbook::layout::template::technology_test_layout();
    let path = Path::new("technology-colors.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let styles = package.part("xl/styles.xml").unwrap();
    let records = cell_style_records(styles);
    for name in ["配装计划", "驱逐、导驱-耐久", "驱逐、导驱-炮击"] {
        let part = worksheet_part_name(&package, name).unwrap();
        let indexes = worksheet_row_style_indexes(package.part(&part).unwrap(), 2);
        let first = if name == "配装计划" { 16 } else { 10 };
        for (column, color) in [
            (first, "FFE2F0D9"),
            (first + 1, "FFFFC7CE"),
            (first + 2, "FFF2F2F2"),
        ] {
            let record = &records[indexes[column] as usize];
            assert_eq!(record.fill_id, fill_index(styles, color));
            assert!(record.locked);
        }
    }
}

#[test]
fn ship_names_preserve_instance_names_and_link_to_original_names() {
    use crate::adapters::device::game_state_mapper::golden_fixture::named_ship_game_state;
    use crate::application::WorkbookProjectionValue as V;
    use calamine::Reader as _;

    let layout = default_layout();
    for (name, proposed, expected) in [
        ("拉菲", false, "拉菲"),
        ("拉菲", true, "拉菲"),
        ("自定义名字", true, "自定义名字(拉菲)"),
        ("昵称(一号)", true, "昵称(一号)(拉菲)"),
        ("实例名字", false, "实例名字"),
    ] {
        let state = named_ship_game_state(name, proposed);
        let projection = project_game_state_to_workbook(&state).unwrap();
        let row = &projection.sheet("loadout_plan").unwrap().rows()[0];
        assert_eq!(row.value("name"), Some(&V::text(expected)));
        assert_eq!(row.value("original_name"), Some(&V::text("拉菲")));
        let path = Path::new("ship-display-name.xlsx");
        let build =
            build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
                .unwrap();
        verify_projection_workbook(path, &build.bytes, &layout, &projection).unwrap();
        let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
        let mut workbook: calamine::Xlsx<Cursor<&[u8]>> =
            calamine::open_workbook_from_rs(Cursor::new(build.bytes.as_slice())).unwrap();
        let range = workbook.worksheet_range("配装计划").unwrap();
        let headers: Vec<_> = range
            .rows()
            .next()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(!headers.iter().any(|header| header == "舰船原名"));
        let column = headers
            .iter()
            .position(|header| header == "舰船名称")
            .unwrap();
        assert_eq!(range.get((1, column)).unwrap().to_string(), expected);
        let cell = format!("{}2", excel_column_name(column as u16));
        assert_eq!(
            worksheet_hyperlinks(&package, "配装计划").get(&cell),
            ship_wiki_url("拉菲").as_ref()
        );
    }
}

#[test]
fn writes_wiki_hyperlinks_on_ship_names_in_loadout_and_technology_sheets() {
    use crate::application::WorkbookProjectionValue as V;

    let layout = crate::adapters::workbook::layout::template::technology_test_layout();
    let projection = project_game_state_to_workbook(&golden_game_state_with_technology()).unwrap();
    let path = Path::new("ship-wiki-links.xlsx");
    let build =
        build_projection_workbook_bytes(path, &layout, &projection, GENERATED_AT_UNIX_MILLIS)
            .unwrap();
    verify_projection_workbook(path, &build.bytes, &layout, &projection).unwrap();
    let package = PackageSnapshot::from_bytes(&build.bytes, path).unwrap();
    let outputs = super::technology_views::output_sheets(&layout, &projection).unwrap();
    let mut linked_sheets = 0_usize;
    for output in outputs {
        let sheet_name = output.layout.display_name();
        let links = worksheet_hyperlinks(&package, sheet_name);
        match crate::application::technology_template_key(output.layout.stable_key()) {
            "loadout_plan" | "ship_technology" => {
                linked_sheets += 1;
                let name_column = layout
                    .generated_fields_for_sheet(output.layout.stable_key())
                    .iter()
                    .position(|field| field.stable_key() == "name")
                    .unwrap() as u16;
                let mut expected = 0_usize;
                for (row_index, row) in output.projection.rows().iter().enumerate() {
                    let Some(V::Text(name)) = row.value("original_name") else {
                        continue;
                    };
                    let Some(url) = ship_wiki_url(name) else {
                        continue;
                    };
                    expected += 1;
                    let cell = format!("{}{}", excel_column_name(name_column), row_index + 2);
                    assert_eq!(
                        links.get(&cell).map(String::as_str),
                        Some(url.as_str()),
                        "{sheet_name} {cell}"
                    );
                }
                assert!(expected > 0, "{sheet_name} 应写入舰船名称超链接");
                assert_eq!(links.len(), expected, "{sheet_name}");
                let part = worksheet_part_name(&package, sheet_name).unwrap();
                let indexes = worksheet_row_style_indexes(package.part(&part).unwrap(), 2);
                assert!(
                    xf_uses_wiki_link_font(
                        package.part("xl/styles.xml").unwrap(),
                        indexes[name_column as usize]
                    ),
                    "{sheet_name} 舰船名称应为深蓝下划线链接"
                );
            }
            _ => {
                assert!(
                    links.is_empty(),
                    "{sheet_name} 不应包含图鉴超链接: {links:?}"
                );
            }
        }
    }
    assert!(linked_sheets >= 2);
}

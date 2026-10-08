//! 使用动态生成的真实 XLSX 样本验证布局加载器。

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rust_xlsxwriter::{Table, TableColumn, Workbook, Worksheet, XlsxError};
use zip::ZipArchive;

use crate::adapters::tool_root::ToolRoot;
#[cfg(unix)]
use crate::adapters::workbook::WorkbookProbeError;
use crate::adapters::workbook::layout::template::build_default_layout_without_field;
use crate::adapters::workbook::package::{
    PackageAddition, PackageRelationship, PackageSnapshot, rewrite_package,
};
use crate::application::{
    AppError, AppErrorCode, LAYOUT_SCHEMA_VERSION, LayoutEditor, LayoutGenerationMode,
    LayoutUpgradePort, LayoutValueFormat, RegisteredLayoutEnumOption, RegisteredLayoutField,
    RegisteredLayoutSheet, WorkbookLayoutRegistry, WorkbookProjectionV4,
};

#[cfg(unix)]
use super::WorkbookLayoutError;
use super::XlsxLayoutUpgradePort;
use super::upgrade::merge_layout;
use super::{
    ENUM_HEADERS, FIELD_HEADERS, FIELD_SETTINGS, FORMAT_SETTINGS, INFO_HEADERS, LayoutTable,
    SHEET_HEADERS, SHEET_SETTINGS, STYLE_HEADERS, load_layout_snapshot, load_workbook_layout,
    parse_layout_snapshot, parse_table_metadata, read_layout_bytes, validate_layout_table_geometry,
    validate_layout_worksheet_cells, validate_table_relationship,
};

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(1);

#[test]
fn loads_real_layout_tables_into_a_stable_immutable_model() {
    let directory = TestDirectory::new("valid-layout");
    let first_path = directory.path().join("first.xlsx");
    let second_path = directory.path().join("second.xlsx");
    let first_fixture = LayoutFixture::valid();
    let mut reordered_fixture = first_fixture.clone();
    reordered_fixture.sheets.reverse();
    reordered_fixture.fields.reverse();
    reordered_fixture.enum_options.reverse();
    reordered_fixture.styles.reverse();
    write_fixture(&first_path, &first_fixture).expect("应建立第一份布局样本");
    write_fixture(&second_path, &reordered_fixture).expect("应建立重排后的布局样本");
    let registry = registry();

    let first = load_workbook_layout(&first_path, &registry).expect("有效布局应加载成功");
    let second = load_workbook_layout(&second_path, &registry).expect("表格行顺序不应改变布局语义");

    assert_eq!(first, second);
    assert_eq!(first.schema_version(), LAYOUT_SCHEMA_VERSION);
    assert_eq!(first.template_name(), "标准布局");
    assert_eq!(first.purpose(), "生成只读快照和可编辑配装计划");
    assert_eq!(first.sheets().len(), 2);
    assert_eq!(first.sheets()[0].stable_key(), "loadout_plan");
    assert_eq!(
        first.sheets()[0].generation(),
        LayoutGenerationMode::Visible
    );
    assert_eq!(first.sheets()[1].stable_key(), "ships");
    assert_eq!(first.fields().len(), 3);
    let final_state = first
        .fields()
        .iter()
        .find(|field| field.stable_key() == "final_state")
        .expect("应保留最终状态字段");
    assert_eq!(final_state.editor(), LayoutEditor::Enumeration);
    assert_eq!(final_state.enum_category(), Some("final_state"));
    assert_eq!(final_state.width().hundredths(), 1_600);
    assert_eq!(first.enum_options().len(), 12);
    assert_eq!(first.styles().len(), 4);
    assert_eq!(first.styles()[1].background_color(), "FFF2CC");
    assert_eq!(first.content_sha256().len(), 64);
    assert!(
        first
            .content_sha256()
            .bytes()
            .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
    );
}

#[test]
fn resolves_editable_control_labels_through_locked_stable_values() {
    let directory = TestDirectory::new("editable-control-labels");
    let path = directory.path().join("custom-labels.xlsx");
    let mut fixture = LayoutFixture::valid();
    for (stable_value, label) in [
        ("visible", "展示"),
        ("hidden", "收起"),
        ("omitted", "跳过"),
        ("text", "字符"),
        ("integer", "整型"),
    ] {
        row_by_key_mut(&mut fixture.enum_options, 1, stable_value)[2] = text(label);
    }
    for row in &mut fixture.sheets {
        row[1] = match text_value(&row[1]) {
            Some("显示") => text("展示"),
            Some("隐藏") => text("收起"),
            actual => panic!("测试工作表包含未登记的生成方式: {actual:?}"),
        };
    }
    for row in &mut fixture.fields {
        row[2] = match text_value(&row[2]) {
            Some("显示") => text("展示"),
            Some("隐藏") => text("收起"),
            actual => panic!("测试字段包含未登记的生成方式: {actual:?}"),
        };
        row[6] = match text_value(&row[6]) {
            Some("文本") => text("字符"),
            Some("整数") => text("整型"),
            actual => panic!("测试字段包含未登记的值格式: {actual:?}"),
        };
    }
    write_fixture(&path, &fixture).expect("应建立自定义控制标签样本");

    let layout = load_workbook_layout(&path, &registry()).expect("可编辑标签不应改变稳定枚举语义");

    assert_eq!(
        layout.sheets()[0].generation(),
        LayoutGenerationMode::Visible
    );
    let level = layout
        .fields()
        .iter()
        .find(|field| field.stable_key() == "level")
        .expect("应保留等级字段");
    assert_eq!(level.generation(), LayoutGenerationMode::Hidden);
    assert_eq!(level.value_format(), LayoutValueFormat::Integer);
    let visible = layout
        .enum_options()
        .iter()
        .find(|option| {
            option.category_key() == "generation_mode" && option.stable_value() == "visible"
        })
        .expect("应保留显示方式稳定枚举");
    assert_eq!(visible.label(), "展示");
}

#[test]
fn reports_missing_registered_items_as_an_upgrade_requirement() {
    let directory = TestDirectory::new("missing-field");
    let path = directory.path().join("missing-field.xlsx");
    let mut fixture = LayoutFixture::valid();
    fixture
        .fields
        .retain(|row| text_value(&row[1]) != Some("level"));
    write_fixture(&path, &fixture).expect("应建立缺字段布局样本");

    let error = load_workbook_layout(&path, &registry()).unwrap_err();

    assert_eq!(error.stage(), "workbook.layout.load");
    assert_eq!(error.code(), AppErrorCode::LayoutUpgradeRequired);
    assert_eq!(
        error.context().get("missing").map(String::as_str),
        Some("field:ships.level")
    );
    assert!(source_message(&error).contains("field:ships.level"));
}

#[test]
fn upgrade_merge_preserves_existing_settings_and_only_defaults_missing_items() {
    let directory = TestDirectory::new("upgrade-merge");
    let default_path = directory.path().join("default.xlsx");
    let source_path = directory.path().join("source.xlsx");
    let defaults = LayoutFixture::valid();
    let mut source = defaults.clone();

    source.info[1][1] = text("用户自定义布局");
    source.info[2][1] = text("保留全部稳定键设置");
    for (stable_value, label) in [
        ("visible", "展示"),
        ("hidden", "收起"),
        ("omitted", "跳过"),
        ("text", "字符"),
        ("integer", "整型"),
    ] {
        row_by_key_mut(&mut source.enum_options, 1, stable_value)[2] = text(label);
    }
    for row in &mut source.sheets {
        row[1] = match text_value(&row[1]) {
            Some("显示") => text("展示"),
            Some("隐藏") => text("收起"),
            actual => panic!("测试工作表包含未登记的生成方式: {actual:?}"),
        };
    }
    for row in &mut source.fields {
        row[2] = match text_value(&row[2]) {
            Some("显示") => text("展示"),
            Some("隐藏") => text("收起"),
            actual => panic!("测试字段包含未登记的生成方式: {actual:?}"),
        };
        row[6] = match text_value(&row[6]) {
            Some("文本") => text("字符"),
            Some("整数") => text("整型"),
            actual => panic!("测试字段包含未登记的值格式: {actual:?}"),
        };
    }

    let ships = row_by_key_mut(&mut source.sheets, 0, "ships");
    ships[2] = text("船坞");
    ships[3] = number(2.0);
    ships[4] = text("B3");
    ships[5] = FixtureCell::Boolean(false);
    ships[6] = text("用户修改后的舰船说明");
    let instance_id = source
        .fields
        .iter_mut()
        .find(|row| text_value(&row[1]) == Some("instance_id"))
        .unwrap();
    instance_id[3] = text("唯一编号");
    instance_id[4] = number(2.0);
    instance_id[5] = number(23.5);
    instance_id[7] = FixtureCell::Boolean(true);
    instance_id[8] = text("用户修改后的字段说明");
    let input_style = row_by_key_mut(&mut source.styles, 0, "input");
    input_style[1] = text("ABCDEF");
    input_style[2] = text("123456");
    input_style[3] = FixtureCell::Boolean(true);
    input_style[4] = text("右");
    input_style[5] = text("下");
    input_style[6] = FixtureCell::Boolean(false);
    input_style[7] = text("用户修改后的样式说明");

    source
        .fields
        .retain(|row| text_value(&row[1]) != Some("level"));
    source.enum_options.retain(|row| {
        !(text_value(&row[0]) == Some("final_state") && text_value(&row[1]) == Some("equip"))
    });
    source
        .styles
        .retain(|row| text_value(&row[0]) != Some("warning"));
    write_fixture(&default_path, &defaults).unwrap();
    write_fixture(&source_path, &source).unwrap();

    let default_bytes = read_layout_bytes(&default_path).unwrap();
    let source_bytes = read_layout_bytes(&source_path).unwrap();
    let default_parsed = parse_layout_snapshot(&default_path, &default_bytes, false).unwrap();
    let source_parsed = parse_layout_snapshot(&source_path, &source_bytes, true).unwrap();

    let merged = merge_layout(source_parsed, default_parsed, &registry()).unwrap();

    assert_eq!(merged.preserved.sheets(), 2);
    assert_eq!(merged.preserved.fields(), 2);
    assert_eq!(merged.preserved.enum_options(), 11);
    assert_eq!(merged.preserved.styles(), 3);
    assert_eq!(merged.added.sheets(), 0);
    assert_eq!(merged.added.fields(), 1);
    assert_eq!(merged.added.enum_options(), 1);
    assert_eq!(merged.added.styles(), 1);
    assert_eq!(
        merged.added_items,
        [
            "enum:final_state.equip",
            "field:ships.level",
            "style:warning"
        ]
    );
    assert_eq!(merged.layout.template_name(), "用户自定义布局");
    assert_eq!(merged.layout.purpose(), "保留全部稳定键设置");
    let ships = merged
        .layout
        .sheets()
        .iter()
        .find(|sheet| sheet.stable_key() == "ships")
        .unwrap();
    assert_eq!(ships.display_name(), "船坞");
    assert_eq!(ships.freeze_cell(), Some("B3"));
    assert!(!ships.default_filter());
    assert_eq!(ships.description(), "用户修改后的舰船说明");
    let instance_id = merged
        .layout
        .fields()
        .iter()
        .find(|field| field.stable_key() == "instance_id")
        .unwrap();
    assert_eq!(instance_id.display_name(), "唯一编号");
    assert_eq!(instance_id.order(), 2);
    assert_eq!(instance_id.width().hundredths(), 2_350);
    assert!(instance_id.wrap());
    assert_eq!(instance_id.description(), "用户修改后的字段说明");
    let added_level = merged
        .layout
        .fields()
        .iter()
        .find(|field| field.stable_key() == "level")
        .unwrap();
    assert_eq!(added_level.order(), 1, "顺序冲突只能调整新增字段");
    let visible = merged
        .layout
        .enum_options()
        .iter()
        .find(|option| {
            option.category_key() == "generation_mode" && option.stable_value() == "visible"
        })
        .unwrap();
    assert_eq!(visible.label(), "展示");
    let input = merged
        .layout
        .styles()
        .iter()
        .find(|style| style.stable_key() == "input")
        .unwrap();
    assert_eq!(input.background_color(), "ABCDEF");
    assert_eq!(input.font_color(), "123456");
    assert!(input.bold());
    assert!(!input.wrap());
    assert_eq!(input.description(), "用户修改后的样式说明");
}

#[test]
fn upgrade_merge_rejects_unmapped_schema_and_unknown_stable_items() {
    let directory = TestDirectory::new("unsupported-upgrade");
    let default_path = directory.path().join("default.xlsx");
    let old_schema_path = directory.path().join("old-schema.xlsx");
    let future_schema_path = directory.path().join("future-schema.xlsx");
    let unknown_path = directory.path().join("unknown.xlsx");
    let defaults = LayoutFixture::valid();
    write_fixture(&default_path, &defaults).unwrap();
    let default_bytes = read_layout_bytes(&default_path).unwrap();

    let mut old_schema = defaults.clone();
    old_schema.info[0][1] = number(0.0);
    write_fixture(&old_schema_path, &old_schema).unwrap();
    let old_schema_bytes = read_layout_bytes(&old_schema_path).unwrap();
    let error = merge_layout(
        parse_layout_snapshot(&old_schema_path, &old_schema_bytes, true).unwrap(),
        parse_layout_snapshot(&default_path, &default_bytes, false).unwrap(),
        &registry(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("没有从布局 schema 0 到 1"));

    let mut future_schema = defaults.clone();
    future_schema.info[0][1] = number(2.0);
    write_fixture(&future_schema_path, &future_schema).unwrap();
    let future_schema_bytes = read_layout_bytes(&future_schema_path).unwrap();
    let error = merge_layout(
        parse_layout_snapshot(&future_schema_path, &future_schema_bytes, true).unwrap(),
        parse_layout_snapshot(&default_path, &default_bytes, false).unwrap(),
        &registry(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("没有从布局 schema 2 到 1"));

    let mut unknown = defaults.clone();
    unknown.fields[0][1] = text("retired_level");
    write_fixture(&unknown_path, &unknown).unwrap();
    let unknown_bytes = read_layout_bytes(&unknown_path).unwrap();
    let error = merge_layout(
        parse_layout_snapshot(&unknown_path, &unknown_bytes, true).unwrap(),
        parse_layout_snapshot(&default_path, &default_bytes, false).unwrap(),
        &registry(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("field:ships.retired_level"));
}

#[test]
fn upgrade_parser_refuses_to_guess_a_missing_control_enum_in_use() {
    let directory = TestDirectory::new("missing-used-control-enum");
    let source_path = directory.path().join("source.xlsx");
    let mut source = LayoutFixture::valid();
    source.enum_options.retain(|row| {
        !(text_value(&row[0]) == Some("generation_mode") && text_value(&row[1]) == Some("visible"))
    });
    write_fixture(&source_path, &source).unwrap();
    let source_bytes = read_layout_bytes(&source_path).unwrap();

    let error = match parse_layout_snapshot(&source_path, &source_bytes, true) {
        Ok(_) => panic!("缺少正在使用的控制枚举时不得猜测稳定值"),
        Err(error) => error,
    };

    assert_eq!(
        error.missing(),
        Some(["enum:generation_mode.visible".to_owned()].as_slice())
    );
}

#[test]
fn upgrade_port_adds_a_missing_production_field_and_preserves_the_source() {
    let directory = TestDirectory::new("upgrade-port");
    let source_path = directory.path().join("workbook-layout.xlsx");
    let source_bytes =
        build_default_layout_without_field(&source_path, "loadout_plan", "instance_id").unwrap();
    std::fs::write(&source_path, &source_bytes).unwrap();
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let port =
        XlsxLayoutUpgradePort::new(ToolRoot::open(directory.path()).unwrap(), registry.clone());

    let report = port.upgrade_layout().unwrap();

    assert_eq!(std::fs::read(&source_path).unwrap(), source_bytes);
    assert_eq!(report.added().fields(), 1);
    assert_eq!(report.added_items(), ["field:loadout_plan.instance_id"]);
    let output_path = directory
        .path()
        .join("data/workbooks/workbook-layout.updated.xlsx");
    let output = load_workbook_layout(&output_path, &registry).unwrap();
    assert!(
        output
            .fields()
            .iter()
            .any(|field| field.sheet_key() == "loadout_plan" && field.stable_key() == "instance_id")
    );
    let output_entries: Vec<PathBuf> = std::fs::read_dir(output_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(output_entries, [output_path]);
}

#[test]
fn rejects_missing_configuration_sheet_and_changed_fixed_header() {
    let directory = TestDirectory::new("invalid-structure");

    let missing_path = directory.path().join("missing-sheet.xlsx");
    let mut missing_fixture = LayoutFixture::valid();
    missing_fixture.include_format_sheet = false;
    write_fixture(&missing_path, &missing_fixture).expect("应建立缺工作表样本");
    let missing_error = load_workbook_layout(&missing_path, &registry()).unwrap_err();
    assert_eq!(missing_error.code(), AppErrorCode::LayoutInvalid);
    assert!(source_message(&missing_error).contains(FORMAT_SETTINGS));

    let header_path = directory.path().join("changed-header.xlsx");
    let mut header_fixture = LayoutFixture::valid();
    header_fixture.field_headers[1] = "字段键".to_owned();
    write_fixture(&header_path, &header_fixture).expect("应建立表头篡改样本");
    let header_error = load_workbook_layout(&header_path, &registry()).unwrap_err();
    assert_invalid_context(&header_error, FIELD_SETTINGS, Some("1"));
    assert!(source_message(&header_error).contains("固定契约"));
}

#[test]
fn rejects_duplicate_and_unknown_stable_keys_with_row_context() {
    let directory = TestDirectory::new("invalid-stable-keys");

    let duplicate_path = directory.path().join("duplicate-field.xlsx");
    let mut duplicate_fixture = LayoutFixture::valid();
    duplicate_fixture.fields[1][1] = text("level");
    write_fixture(&duplicate_path, &duplicate_fixture).expect("应建立重复字段样本");
    let duplicate_error = load_workbook_layout(&duplicate_path, &registry()).unwrap_err();
    assert_invalid_context(&duplicate_error, FIELD_SETTINGS, Some("3"));
    assert_eq!(
        duplicate_error.context().get("key").map(String::as_str),
        Some("ships.level")
    );
    assert!(source_message(&duplicate_error).contains("首次出现"));

    let unknown_path = directory.path().join("unknown-sheet.xlsx");
    let mut unknown_fixture = LayoutFixture::valid();
    unknown_fixture.sheets[0][0] = text("unknown_sheet");
    write_fixture(&unknown_path, &unknown_fixture).expect("应建立未知工作表样本");
    let unknown_error = load_workbook_layout(&unknown_path, &registry()).unwrap_err();
    assert_invalid_context(&unknown_error, SHEET_SETTINGS, Some("2"));
    assert_eq!(
        unknown_error.context().get("key").map(String::as_str),
        Some("unknown_sheet")
    );
}

#[test]
fn rejects_required_omission_and_duplicate_final_names() {
    let directory = TestDirectory::new("invalid-final-layout");

    let omitted_path = directory.path().join("required-omitted.xlsx");
    let mut omitted_fixture = LayoutFixture::valid();
    row_by_key_mut(&mut omitted_fixture.sheets, 0, "loadout_plan")[1] = text("不生成");
    write_fixture(&omitted_path, &omitted_fixture).expect("应建立省略必需表样本");
    let omitted_error = load_workbook_layout(&omitted_path, &registry()).unwrap_err();
    assert_invalid_context(&omitted_error, SHEET_SETTINGS, Some("3"));
    assert!(source_message(&omitted_error).contains("必需工作表"));

    let omitted_field_path = directory.path().join("required-field-omitted.xlsx");
    let mut omitted_field_fixture = LayoutFixture::valid();
    row_by_key_mut(&mut omitted_field_fixture.fields, 1, "final_state")[2] = text("不生成");
    write_fixture(&omitted_field_path, &omitted_field_fixture).expect("应建立省略必需字段样本");
    let omitted_field_error = load_workbook_layout(&omitted_field_path, &registry()).unwrap_err();
    assert_invalid_context(&omitted_field_error, FIELD_SETTINGS, Some("4"));
    assert!(source_message(&omitted_field_error).contains("必需字段"));

    let duplicate_name_path = directory.path().join("duplicate-name.xlsx");
    let mut duplicate_name_fixture = LayoutFixture::valid();
    let loadout_name =
        row_by_key_mut(&mut duplicate_name_fixture.sheets, 0, "loadout_plan")[2].clone();
    row_by_key_mut(&mut duplicate_name_fixture.sheets, 0, "ships")[2] = loadout_name;
    write_fixture(&duplicate_name_path, &duplicate_name_fixture).expect("应建立重复最终表名样本");
    let duplicate_name_error = load_workbook_layout(&duplicate_name_path, &registry()).unwrap_err();
    assert_invalid_context(&duplicate_name_error, SHEET_SETTINGS, Some("3"));
    assert_actual_expected(&duplicate_name_error, "配装计划", "工作表内唯一");
    assert!(source_message(&duplicate_name_error).contains("最终表名"));

    let duplicate_order_path = directory.path().join("duplicate-field-order.xlsx");
    let mut duplicate_order_fixture = LayoutFixture::valid();
    row_by_key_mut(&mut duplicate_order_fixture.fields, 1, "level")[4] = number(1.0);
    write_fixture(&duplicate_order_path, &duplicate_order_fixture).expect("应建立重复字段顺序样本");
    let duplicate_order_error =
        load_workbook_layout(&duplicate_order_path, &registry()).unwrap_err();
    assert_invalid_context(&duplicate_order_error, FIELD_SETTINGS, Some("3"));
    assert_actual_expected(&duplicate_order_error, "1", "同一工作表内唯一");
    assert!(source_message(&duplicate_order_error).contains("最终字段顺序"));
}

#[test]
fn rejects_formulas_external_relationships_and_macro_parts() {
    let directory = TestDirectory::new("unsafe-content");

    let formula_path = directory.path().join("formula.xlsx");
    let mut formula_fixture = LayoutFixture::valid();
    formula_fixture.fields[0][3] = FixtureCell::Formula("=1+1".to_owned());
    write_fixture(&formula_path, &formula_fixture).expect("应建立公式样本");
    let formula_error = load_workbook_layout(&formula_path, &registry()).unwrap_err();
    assert_invalid_context(&formula_error, FIELD_SETTINGS, Some("2"));
    assert!(source_message(&formula_error).contains("不允许公式"));

    let empty_formula_path = directory.path().join("empty-formula.xlsx");
    rewrite_first_formula_as_empty(&formula_path, &empty_formula_path)
        .expect("应建立空公式节点样本");
    let empty_formula_error = load_workbook_layout(&empty_formula_path, &registry()).unwrap_err();
    assert_invalid_context(&empty_formula_error, FIELD_SETTINGS, Some("2"));
    assert!(source_message(&empty_formula_error).contains("不允许公式"));

    let external_path = directory.path().join("external.xlsx");
    let mut external_fixture = LayoutFixture::valid();
    external_fixture.external_url = true;
    write_fixture(&external_path, &external_fixture).expect("应建立外链样本");
    let external_error = load_workbook_layout(&external_path, &registry()).unwrap_err();
    assert_eq!(external_error.code(), AppErrorCode::LayoutInvalid);
    assert!(external_error.context().contains_key("part"));
    assert!(external_error.context().contains_key("relationship_id"));
    assert_eq!(
        external_error.context().get("target").map(String::as_str),
        Some("https://example.invalid/layout.xlsx")
    );
    assert!(source_message(&external_error).contains("外部目标"));

    let source_path = directory.path().join("macro-source.xlsx");
    let macro_path = directory.path().join("macro.xlsx");
    write_fixture(&source_path, &LayoutFixture::valid()).expect("应建立宏注入源样本");
    add_macro_part(&source_path, &macro_path).expect("应向复制样本加入宏部件");
    let macro_error = load_workbook_layout(&macro_path, &registry()).unwrap_err();
    assert_eq!(macro_error.code(), AppErrorCode::LayoutInvalid);
    assert_eq!(
        macro_error.context().get("part").map(String::as_str),
        Some("xl/vbaProject.bin")
    );
    assert!(source_message(&macro_error).contains("xl/vbaProject.bin"));

    let macro_content_type_path = directory.path().join("macro-content-type.xlsx");
    rewrite_main_content_type_as_macro(&source_path, &macro_content_type_path)
        .expect("应建立宏内容类型样本");
    let macro_content_type_error =
        load_workbook_layout(&macro_content_type_path, &registry()).unwrap_err();
    assert_eq!(macro_content_type_error.code(), AppErrorCode::LayoutInvalid);
    assert_eq!(
        macro_content_type_error
            .context()
            .get("part")
            .map(String::as_str),
        Some("[Content_Types].xml#macro")
    );
}

#[test]
fn rejects_invalid_editable_and_locked_cell_values() {
    let directory = TestDirectory::new("invalid-cell-values");

    let mut cases: Vec<(&str, LayoutFixture, &str, &str, &str)> = Vec::new();

    let mut invalid_format = LayoutFixture::valid();
    invalid_format.fields[0][6] = text("货币");
    cases.push(("format", invalid_format, FIELD_SETTINGS, "2", "未知值格式"));

    let mut invalid_freeze = LayoutFixture::valid();
    invalid_freeze.sheets[0][4] = text("a2");
    cases.push(("freeze", invalid_freeze, SHEET_SETTINGS, "2", "单元格引用"));

    let mut invalid_color = LayoutFixture::valid();
    invalid_color.styles[0][1] = text("GG00FF");
    cases.push(("color", invalid_color, FORMAT_SETTINGS, "21", "六位 RGB"));

    let mut wildcard_model = LayoutFixture::valid();
    wildcard_model.fields[0][9] = text("GameState.ships[*].growth.level");
    cases.push((
        "wildcard",
        wildcard_model,
        FIELD_SETTINGS,
        "2",
        "不允许通配符",
    ));

    let mut unknown_enum = LayoutFixture::valid();
    unknown_enum.enum_options[0][1] = text("renamed");
    cases.push(("enum", unknown_enum, FORMAT_SETTINGS, "7", "不支持的稳定值"));

    let mut duplicate_enum_order = LayoutFixture::valid();
    duplicate_enum_order.enum_options[1][3] = number(1.0);
    cases.push((
        "enum-order",
        duplicate_enum_order,
        FORMAT_SETTINGS,
        "8",
        "枚举顺序",
    ));

    let mut invalid_info = LayoutFixture::valid();
    invalid_info.info[1][1] = text("标准\n布局");
    cases.push((
        "info-control",
        invalid_info,
        FORMAT_SETTINGS,
        "3",
        "控制字符",
    ));

    let mut reserved_sheet_name = LayoutFixture::valid();
    reserved_sheet_name.sheets[0][2] = text("History");
    cases.push((
        "reserved-sheet-name",
        reserved_sheet_name,
        SHEET_SETTINGS,
        "2",
        "Excel 工作表名称约束",
    ));

    for (label, fixture, expected_sheet, expected_row, expected_message) in cases {
        let path = directory.path().join(format!("{label}.xlsx"));
        write_fixture(&path, &fixture).expect("应建立非法单元格样本");
        let error = load_workbook_layout(&path, &registry()).unwrap_err();
        assert_invalid_context(&error, expected_sheet, Some(expected_row));
        assert!(
            source_message(&error).contains(expected_message),
            "{label} 实际错误: {error:?}"
        );
    }
}

#[test]
fn reports_locked_registry_mismatch_with_actual_and_expected_values() {
    let directory = TestDirectory::new("registry-mismatch");
    let path = directory.path().join("registry-mismatch.xlsx");
    let mut fixture = LayoutFixture::valid();
    row_by_key_mut(&mut fixture.fields, 1, "level")[9] =
        text("GameState.ships[].growth.display_level");
    write_fixture(&path, &fixture).expect("应建立锁定字段不一致样本");

    let error = load_workbook_layout(&path, &registry()).unwrap_err();

    assert_invalid_context(&error, FIELD_SETTINGS, Some("2"));
    assert_eq!(
        error.context().get("key").map(String::as_str),
        Some("ships.level")
    );
    assert_actual_expected(
        &error,
        "GameState.ships[].growth.display_level",
        "GameState.ships[].growth.level",
    );
}

#[test]
fn distinguishes_old_and_future_schema_with_version_row_context() {
    let directory = TestDirectory::new("schema-versions");
    let old_path = directory.path().join("old-schema.xlsx");
    let mut old_fixture = LayoutFixture::valid();
    old_fixture.info[0][1] = number(0.0);
    write_fixture(&old_path, &old_fixture).expect("应建立旧 schema 样本");

    let old_error = load_workbook_layout(&old_path, &registry()).unwrap_err();

    assert_eq!(old_error.code(), AppErrorCode::LayoutUpgradeRequired);
    assert_eq!(
        old_error.context().get("sheet").map(String::as_str),
        Some(FORMAT_SETTINGS)
    );
    assert_eq!(
        old_error.context().get("row").map(String::as_str),
        Some("2")
    );
    assert_actual_expected(&old_error, "0", &LAYOUT_SCHEMA_VERSION.to_string());

    let path = directory.path().join("future-schema.xlsx");
    let mut fixture = LayoutFixture::valid();
    fixture.info[0][1] = number(f64::from(LAYOUT_SCHEMA_VERSION + 1));
    write_fixture(&path, &fixture).expect("应建立未来 schema 样本");

    let error = load_workbook_layout(&path, &registry()).unwrap_err();

    assert_invalid_context(&error, FORMAT_SETTINGS, Some("2"));
    assert_actual_expected(
        &error,
        &(LAYOUT_SCHEMA_VERSION + 1).to_string(),
        &LAYOUT_SCHEMA_VERSION.to_string(),
    );
}

#[test]
fn rejects_sparse_far_cell_before_calamine_allocates_a_dense_range() {
    let directory = TestDirectory::new("far-cell");
    let path = directory.path().join("far-cell.xlsx");
    let mut fixture = LayoutFixture::valid();
    fixture.far_cell = true;
    write_fixture(&path, &fixture).expect("应建立极远稀疏单元格样本");

    let error = load_workbook_layout(&path, &registry()).unwrap_err();

    assert_invalid_context(&error, FIELD_SETTINGS, Some("1048576"));
    assert_actual_expected(&error, "XFD1048576", "行不超过 10000、列不超过 64");
    assert!(source_message(&error).contains("坐标超出受控范围"));
}

#[test]
fn package_preflight_and_semantic_parser_share_one_byte_snapshot() {
    let directory = TestDirectory::new("single-snapshot");
    let path = directory.path().join("layout.xlsx");
    write_fixture(&path, &LayoutFixture::valid()).expect("应建立初始有效布局");
    let bytes = read_layout_bytes(&path).expect("应读取固定布局字节");

    let mut replacement = LayoutFixture::valid();
    replacement.info[0][1] = number(f64::from(LAYOUT_SCHEMA_VERSION + 1));
    write_fixture(&path, &replacement).expect("应替换路径上的布局文件");

    let layout =
        load_layout_snapshot(&path, &bytes, &registry()).expect("固定快照不应受路径后续替换影响");
    assert_eq!(layout.schema_version(), LAYOUT_SCHEMA_VERSION);
    assert_eq!(
        load_workbook_layout(&path, &registry()).unwrap_err().code(),
        AppErrorCode::LayoutInvalid
    );
}

#[cfg(unix)]
#[test]
fn rejects_final_symlink_without_following_it() {
    use std::os::unix::fs::symlink;

    let directory = TestDirectory::new("layout-symlink");
    let target = directory.path().join("target.xlsx");
    let link = directory.path().join("link.xlsx");
    write_fixture(&target, &LayoutFixture::valid()).expect("应建立链接目标样本");
    symlink(&target, &link).expect("应建立布局符号链接");

    let direct_error = read_layout_bytes(&link).unwrap_err();
    assert!(matches!(
        direct_error,
        WorkbookLayoutError::Workbook(WorkbookProbeError::InvalidPath { .. })
    ));
    let error = load_workbook_layout(&link, &registry()).unwrap_err();

    assert_eq!(error.code(), AppErrorCode::LayoutInvalid);
    assert!(source_message(&error).contains("不能是符号链接"));
}

#[test]
fn bounds_scan_accepts_cells_whose_references_are_inferred() {
    let xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData><row r="1"><c r="A1"><v>1</v></c><c><v>2</v></c></row></sheetData>
</worksheet>"#;

    validate_layout_worksheet_cells(SHEET_SETTINGS, "xl/worksheets/sheet1.xml", xml)
        .expect("省略连续单元格引用时应按 Calamine 游标规则推断");
}

#[test]
fn rejects_oversized_table_range_before_iterating_config_rows() {
    let columns = SHEET_HEADERS
        .iter()
        .enumerate()
        .map(|(index, header)| format!(r#"<tableColumn id="{}" name="{}"/>"#, index + 1, header))
        .collect::<String>();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="LayoutSheets" displayName="LayoutSheets" ref="A1:H10001">
  <tableColumns count="8">{columns}</tableColumns>
</table>"#
    );

    let error =
        parse_table_metadata(SHEET_SETTINGS, "xl/tables/table1.xml", xml.as_bytes()).unwrap_err();

    assert!(error.to_string().contains("表格范围超出受控大小"));
}

#[test]
fn rejects_table_total_rows_before_parsing_configuration_records() {
    let columns = SHEET_HEADERS
        .iter()
        .enumerate()
        .map(|(index, header)| format!(r#"<tableColumn id="{}" name="{}"/>"#, index + 1, header))
        .collect::<String>();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<table xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" name="LayoutSheets" displayName="LayoutSheets" ref="A1:H3" totalsRowCount="1" totalsRowShown="1">
  <tableColumns count="8">{columns}</tableColumns>
</table>"#
    );

    let error =
        parse_table_metadata(SHEET_SETTINGS, "xl/tables/table1.xml", xml.as_bytes()).unwrap_err();

    assert!(error.to_string().contains("不允许汇总行"));
}

#[test]
fn rejects_overlapping_tables_and_nonstandard_relationship_uri() {
    let first = LayoutTable {
        name: "LayoutInfo".to_owned(),
        sheet_name: FORMAT_SETTINGS,
        first: super::CellCoordinate { row: 0, column: 0 },
        last: super::CellCoordinate { row: 3, column: 2 },
        headers: strings(&INFO_HEADERS),
    };
    let second = LayoutTable {
        name: "LayoutEnums".to_owned(),
        sheet_name: FORMAT_SETTINGS,
        first: super::CellCoordinate { row: 3, column: 1 },
        last: super::CellCoordinate { row: 6, column: 5 },
        headers: strings(&ENUM_HEADERS),
    };

    let overlap_error = validate_layout_table_geometry(&[first, second]).unwrap_err();
    assert!(overlap_error.to_string().contains("配置表格范围重叠"));

    let relationship = PackageRelationship {
        id: "rId1".to_owned(),
        relationship_type: "https://example.invalid/custom/table".to_owned(),
        target: "../tables/table1.xml".to_owned(),
        external: false,
    };
    let relationship_error =
        validate_table_relationship("xl/worksheets/_rels/sheet1.xml.rels", &relationship)
            .unwrap_err();
    assert!(
        relationship_error
            .to_string()
            .contains("不是受支持的 Excel 表格关系")
    );
}

fn registry() -> WorkbookLayoutRegistry {
    WorkbookLayoutRegistry::new(
        vec![
            RegisteredLayoutSheet::new("loadout_plan", true),
            RegisteredLayoutSheet::new("ships", false),
        ],
        vec![
            RegisteredLayoutField::new(
                "loadout_plan",
                "final_state",
                "DesiredState.slots[].final_state",
                [LayoutValueFormat::Text],
                LayoutEditor::Enumeration,
                true,
                Some("final_state".to_owned()),
            ),
            RegisteredLayoutField::new(
                "ships",
                "instance_id",
                "GameState.ships[].instance_id",
                [LayoutValueFormat::Text],
                LayoutEditor::ReadOnly,
                false,
                None,
            ),
            RegisteredLayoutField::new(
                "ships",
                "level",
                "GameState.ships[].growth.level",
                [LayoutValueFormat::Integer],
                LayoutEditor::ReadOnly,
                false,
                None,
            ),
        ],
        vec![
            enum_registration("generation_mode", "visible"),
            enum_registration("generation_mode", "hidden"),
            enum_registration("generation_mode", "omitted"),
            enum_registration("value_format", "text"),
            enum_registration("value_format", "integer"),
            enum_registration("value_format", "decimal"),
            enum_registration("value_format", "percentage"),
            enum_registration("value_format", "date_time"),
            enum_registration("value_format", "json"),
            enum_registration("final_state", "keep"),
            enum_registration("final_state", "empty"),
            enum_registration("final_state", "equip"),
        ],
        ["read_only", "input", "warning", "error"]
            .map(str::to_owned)
            .to_vec(),
    )
    .expect("测试注册表必须有效")
}

fn enum_registration(category: &str, value: &str) -> RegisteredLayoutEnumOption {
    RegisteredLayoutEnumOption::new(category, value)
}

/// 可以按失败场景定点修改的布局工作簿语义夹具。
#[derive(Clone)]
struct LayoutFixture {
    sheet_headers: Vec<String>,
    field_headers: Vec<String>,
    info_headers: Vec<String>,
    enum_headers: Vec<String>,
    style_headers: Vec<String>,
    sheets: Vec<Vec<FixtureCell>>,
    fields: Vec<Vec<FixtureCell>>,
    info: Vec<Vec<FixtureCell>>,
    enum_options: Vec<Vec<FixtureCell>>,
    styles: Vec<Vec<FixtureCell>>,
    include_format_sheet: bool,
    external_url: bool,
    far_cell: bool,
}

impl LayoutFixture {
    /// 建立覆盖两张业务表、三类字段、完整枚举和四种样式的有效配置。
    fn valid() -> Self {
        Self {
            sheet_headers: strings(&SHEET_HEADERS),
            field_headers: strings(&FIELD_HEADERS),
            info_headers: strings(&INFO_HEADERS),
            enum_headers: strings(&ENUM_HEADERS),
            style_headers: strings(&STYLE_HEADERS),
            sheets: vec![
                sheet_cells(
                    "ships",
                    "隐藏",
                    "舰船",
                    2.0,
                    "A2",
                    true,
                    "当前舰船快照",
                    false,
                ),
                sheet_cells(
                    "loadout_plan",
                    "显示",
                    "配装计划",
                    1.0,
                    "A2",
                    true,
                    "用户期望的最终配装",
                    true,
                ),
            ],
            fields: vec![
                field_cells(
                    "ships",
                    "level",
                    "隐藏",
                    "等级",
                    2.0,
                    10.0,
                    "整数",
                    false,
                    "当前等级",
                    "GameState.ships[].growth.level",
                    "只读",
                    false,
                ),
                field_cells(
                    "ships",
                    "instance_id",
                    "显示",
                    "舰船实例ID",
                    1.0,
                    18.0,
                    "文本",
                    false,
                    "真实舰船实例 ID",
                    "GameState.ships[].instance_id",
                    "只读",
                    false,
                ),
                field_cells(
                    "loadout_plan",
                    "final_state",
                    "显示",
                    "最终状态",
                    1.0,
                    16.0,
                    "文本",
                    false,
                    "保持、空槽或装备",
                    "DesiredState.slots[].final_state",
                    "枚举",
                    true,
                ),
            ],
            info: vec![
                vec![
                    text("layout_schema_version"),
                    number(f64::from(LAYOUT_SCHEMA_VERSION)),
                    text("布局配置 schema"),
                ],
                cells(&["template_name", "标准布局", "模板显示名称"]),
                cells(&["layout_purpose", "生成只读快照和可编辑配装计划", "布局用途"]),
            ],
            enum_options: vec![
                enum_cells("generation_mode", "visible", "显示", 1.0),
                enum_cells("generation_mode", "hidden", "隐藏", 2.0),
                enum_cells("generation_mode", "omitted", "不生成", 3.0),
                enum_cells("value_format", "text", "文本", 1.0),
                enum_cells("value_format", "integer", "整数", 2.0),
                enum_cells("value_format", "decimal", "小数", 3.0),
                enum_cells("value_format", "percentage", "百分比", 4.0),
                enum_cells("value_format", "date_time", "日期时间", 5.0),
                enum_cells("value_format", "json", "JSON", 6.0),
                enum_cells("final_state", "keep", "保持", 1.0),
                enum_cells("final_state", "empty", "空槽", 2.0),
                enum_cells("final_state", "equip", "装备", 3.0),
            ],
            styles: vec![
                style_cells("read_only", "f2f2f2", "000000", false, "左", "中", false),
                style_cells("input", "fff2cc", "000000", false, "左", "中", true),
                style_cells("warning", "FCE4D6", "9C5700", true, "中", "中", true),
                style_cells("error", "FFC7CE", "9C0006", true, "中", "中", true),
            ],
            include_format_sheet: true,
            external_url: false,
            far_cell: false,
        }
    }
}

/// 写入测试工作簿时保留单元格类型差异。
#[derive(Clone)]
enum FixtureCell {
    Text(String),
    Number(f64),
    Boolean(bool),
    Formula(String),
}

fn text(value: &str) -> FixtureCell {
    FixtureCell::Text(value.to_owned())
}

const fn number(value: f64) -> FixtureCell {
    FixtureCell::Number(value)
}

fn strings<const N: usize>(values: &[&str; N]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn cells<const N: usize>(values: &[&str; N]) -> Vec<FixtureCell> {
    values.iter().map(|value| text(value)).collect()
}

#[allow(clippy::too_many_arguments)]
fn sheet_cells(
    stable_key: &str,
    generation: &str,
    display_name: &str,
    order: f64,
    freeze_cell: &str,
    default_filter: bool,
    description: &str,
    required: bool,
) -> Vec<FixtureCell> {
    vec![
        text(stable_key),
        text(generation),
        text(display_name),
        number(order),
        text(freeze_cell),
        FixtureCell::Boolean(default_filter),
        text(description),
        FixtureCell::Boolean(required),
    ]
}

#[allow(clippy::too_many_arguments)]
fn field_cells(
    sheet_key: &str,
    stable_key: &str,
    generation: &str,
    display_name: &str,
    order: f64,
    width: f64,
    value_format: &str,
    wrap: bool,
    description: &str,
    model_path: &str,
    editor: &str,
    required: bool,
) -> Vec<FixtureCell> {
    vec![
        text(sheet_key),
        text(stable_key),
        text(generation),
        text(display_name),
        number(order),
        number(width),
        text(value_format),
        FixtureCell::Boolean(wrap),
        text(description),
        text(model_path),
        text(editor),
        FixtureCell::Boolean(required),
    ]
}

fn enum_cells(category: &str, value: &str, label: &str, order: f64) -> Vec<FixtureCell> {
    vec![
        text(category),
        text(value),
        text(label),
        number(order),
        text("稳定枚举值"),
    ]
}

#[allow(clippy::too_many_arguments)]
fn style_cells(
    key: &str,
    background: &str,
    font: &str,
    bold: bool,
    horizontal: &str,
    vertical: &str,
    wrap: bool,
) -> Vec<FixtureCell> {
    vec![
        text(key),
        text(background),
        text(font),
        FixtureCell::Boolean(bold),
        text(horizontal),
        text(vertical),
        FixtureCell::Boolean(wrap),
        text("稳定样式"),
    ]
}

/// 把语义夹具写成包含五个真实 Excel 表格的 XLSX 文件。
fn write_fixture(path: &Path, fixture: &LayoutFixture) -> Result<(), XlsxError> {
    let mut workbook = Workbook::new();
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name(SHEET_SETTINGS)?;
        write_table(
            sheet,
            0,
            0,
            "LayoutSheets",
            &fixture.sheet_headers,
            &fixture.sheets,
        )?;
        if fixture.external_url {
            sheet.write_url(10, 10, "https://example.invalid/layout.xlsx")?;
        }
    }
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name(FIELD_SETTINGS)?;
        write_table(
            sheet,
            0,
            0,
            "LayoutFields",
            &fixture.field_headers,
            &fixture.fields,
        )?;
        if fixture.far_cell {
            sheet.write_string(1_048_575, 16_383, "far-cell")?;
        }
    }
    if fixture.include_format_sheet {
        let sheet = workbook.add_worksheet();
        sheet.set_name(FORMAT_SETTINGS)?;
        write_table(
            sheet,
            0,
            1,
            "LayoutInfo",
            &fixture.info_headers,
            &fixture.info,
        )?;
        write_table(
            sheet,
            5,
            0,
            "LayoutEnums",
            &fixture.enum_headers,
            &fixture.enum_options,
        )?;
        let style_start = 7_u32
            .checked_add(u32::try_from(fixture.enum_options.len()).expect("测试枚举行数应可表示"))
            .expect("测试样式起始行不应溢出");
        write_table(
            sheet,
            style_start,
            0,
            "LayoutStyles",
            &fixture.style_headers,
            &fixture.styles,
        )?;
    }
    workbook.save(path)
}

/// 写入表格数据和固定 TableColumn 定义，避免把普通单元格误当表格测试。
fn write_table(
    sheet: &mut Worksheet,
    first_row: u32,
    first_column: u16,
    name: &str,
    headers: &[String],
    rows: &[Vec<FixtureCell>],
) -> Result<(), XlsxError> {
    assert!(!headers.is_empty(), "测试表格必须有列");
    assert!(!rows.is_empty(), "测试表格必须有数据行");
    for (row_offset, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), headers.len(), "测试表格行宽必须固定");
        let row_number = first_row + 1 + u32::try_from(row_offset).expect("测试行偏移应可表示");
        for (column_offset, value) in row.iter().enumerate() {
            let column_number =
                first_column + u16::try_from(column_offset).expect("测试列偏移应可表示");
            match value {
                FixtureCell::Text(value) => {
                    sheet.write_string(row_number, column_number, value)?;
                }
                FixtureCell::Number(value) => {
                    sheet.write_number(row_number, column_number, *value)?;
                }
                FixtureCell::Boolean(value) => {
                    sheet.write_boolean(row_number, column_number, *value)?;
                }
                FixtureCell::Formula(value) => {
                    sheet.write_formula(row_number, column_number, value.as_str())?;
                }
            }
        }
    }
    let columns: Vec<TableColumn> = headers
        .iter()
        .map(|header| TableColumn::new().set_header(header))
        .collect();
    let table = Table::new().set_name(name).set_columns(&columns);
    let last_row = first_row + u32::try_from(rows.len()).expect("测试数据行数应可表示");
    let last_column = first_column + u16::try_from(headers.len() - 1).expect("测试列数应可表示");
    sheet.add_table(first_row, first_column, last_row, last_column, &table)?;
    Ok(())
}

/// 保留源包全部条目并追加宏部件，建立扩展名伪装的拒绝样本。
fn add_macro_part(source: &Path, destination: &Path) -> Result<(), super::WorkbookProbeError> {
    let source_file = File::open(source).map_err(|source_error| super::WorkbookProbeError::Io {
        stage: "打开宏注入源样本",
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let archive =
        ZipArchive::new(source_file).map_err(|source_error| super::WorkbookProbeError::Zip {
            stage: "打开宏注入源 ZIP",
            path: source.to_path_buf(),
            source: source_error,
        })?;
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source_error| super::WorkbookProbeError::Io {
            stage: "建立宏注入样本",
            path: destination.to_path_buf(),
            source: source_error,
        })?;
    let file = rewrite_package(
        archive,
        destination_file,
        source,
        destination,
        &BTreeMap::new(),
        &[PackageAddition {
            name: "xl/vbaProject.bin".to_owned(),
            bytes: b"layout-macro-probe".to_vec(),
        }],
    )?;
    file.sync_all()
        .map_err(|source_error| super::WorkbookProbeError::Io {
            stage: "同步宏注入样本",
            path: destination.to_path_buf(),
            source: source_error,
        })
}

/// 只改写工作簿主部件的内容类型，覆盖没有 `vbaProject.bin` 的宏标记样本。
fn rewrite_main_content_type_as_macro(
    source: &Path,
    destination: &Path,
) -> Result<(), Box<dyn Error>> {
    let package = PackageSnapshot::read(source)?;
    let original = String::from_utf8(package.part("[Content_Types].xml")?.to_vec())?;
    let ordinary = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml";
    let macro_enabled = "application/vnd.ms-excel.sheet.macroEnabled.main+xml";
    let replacement = original.replace(ordinary, macro_enabled);
    assert_ne!(replacement, original, "源样本必须包含普通工作簿内容类型");

    let source_file = File::open(source)?;
    let archive = ZipArchive::new(source_file)?;
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut replacements = BTreeMap::new();
    replacements.insert("[Content_Types].xml".to_owned(), replacement.into_bytes());
    let file = rewrite_package(
        archive,
        destination_file,
        source,
        destination,
        &replacements,
        &[],
    )?;
    file.sync_all()?;
    Ok(())
}

/// 把真实工作表中的首个公式改成空节点，覆盖没有公式文本但仍有公式语义的文件。
fn rewrite_first_formula_as_empty(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    let package = PackageSnapshot::read(source)?;
    let part_name = crate::adapters::workbook::reference::inspection::worksheet_part_name(
        &package,
        FIELD_SETTINGS,
    )?;
    let mut reader = quick_xml::Reader::from_reader(package.part(&part_name)?);
    reader.config_mut().trim_text(false);
    let mut writer = quick_xml::Writer::new(Vec::new());
    let mut replaced = false;
    loop {
        let event = reader.read_event()?;
        match event {
            quick_xml::events::Event::Start(element)
                if !replaced && element.local_name().as_ref() == b"f" =>
            {
                reader.read_to_end(element.name())?;
                writer.write_event(quick_xml::events::Event::Empty(element))?;
                replaced = true;
            }
            quick_xml::events::Event::Eof => break,
            event => writer.write_event(event)?,
        }
    }
    assert!(replaced, "源样本必须包含公式节点");

    let source_file = File::open(source)?;
    let archive = ZipArchive::new(source_file)?;
    let destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut replacements = BTreeMap::new();
    replacements.insert(part_name, writer.into_inner());
    let file = rewrite_package(
        archive,
        destination_file,
        source,
        destination,
        &replacements,
        &[],
    )?;
    file.sync_all()?;
    Ok(())
}

fn row_by_key_mut<'a>(
    rows: &'a mut [Vec<FixtureCell>],
    key_column: usize,
    key: &str,
) -> &'a mut Vec<FixtureCell> {
    rows.iter_mut()
        .find(|row| text_value(&row[key_column]) == Some(key))
        .expect("测试样本必须包含目标稳定键")
}

fn text_value(value: &FixtureCell) -> Option<&str> {
    match value {
        FixtureCell::Text(value) => Some(value),
        FixtureCell::Number(_) | FixtureCell::Boolean(_) | FixtureCell::Formula(_) => None,
    }
}

fn assert_invalid_context(error: &AppError, sheet: &str, row: Option<&str>) {
    assert_eq!(error.code(), AppErrorCode::LayoutInvalid);
    assert_eq!(
        error.context().get("sheet").map(String::as_str),
        Some(sheet)
    );
    if let Some(row) = row {
        assert_eq!(error.context().get("row").map(String::as_str), Some(row));
    }
}

fn assert_actual_expected(error: &AppError, actual: &str, expected_contains: &str) {
    assert_eq!(
        error.context().get("actual").map(String::as_str),
        Some(actual)
    );
    assert!(
        error
            .context()
            .get("expected")
            .is_some_and(|expected| expected.contains(expected_contains)),
        "期望值上下文不包含 {expected_contains:?}: {:?}",
        error.context()
    );
}

fn source_message(error: &AppError) -> String {
    error
        .source()
        .expect("布局应用错误必须保留底层原因")
        .to_string()
}

/// 每个测试独占并在结束时清理的用户 scratch 目录。
struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    /// 建立进程内唯一目录，同名残留会明确导致测试失败。
    fn new(label: &str) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let identifier = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let path = home
            .join("suzushiro")
            .join("scratch")
            .join("azlw-layout-tests")
            .join(format!("{label}-{}-{identifier}", std::process::id()));
        std::fs::create_dir_all(path.parent().expect("测试目录必须包含父目录"))
            .expect("应建立布局测试根目录");
        std::fs::create_dir(&path).expect("布局测试目录不得与残留目录重名");
        Self { path }
    }

    /// 返回测试专用目录。
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path).expect("应清理布局测试目录");
        }
    }
}

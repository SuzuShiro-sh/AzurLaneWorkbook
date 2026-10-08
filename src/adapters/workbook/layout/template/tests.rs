//! 覆盖默认布局模板、展示元数据和工作表结构的单元测试。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::adapters::workbook::{edit_text_cell_to_new_file, load_workbook_layout};
use crate::application::{LayoutGenerationMode, LayoutValueFormat, WorkbookProjectionV4};

use super::super::super::package::{PackageSnapshot, optional_attribute, write_new_file_bytes};
use super::super::super::reference::inspection::worksheet_part_name;
use super::{
    FIELD_SETTINGS, FORMAT_SETTINGS, SHEET_SETTINGS, build_layout_workbook_bytes,
    create_default_layout_workbook,
};

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn default_layout_is_reproducible_strictly_loadable_and_matches_root_file() {
    let directory = TestDirectory::new("default-layout");
    let first = directory.path().join("first.xlsx");
    let second = directory.path().join("second.xlsx");
    create_default_layout_workbook(&first).unwrap();
    create_default_layout_workbook(&second).unwrap();

    let first_bytes = std::fs::read(&first).unwrap();
    let second_bytes = std::fs::read(&second).unwrap();
    let root_bytes =
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx")).unwrap();
    assert_eq!(first_bytes, second_bytes);
    assert_eq!(first_bytes, root_bytes);

    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let layout = load_workbook_layout(&first, &registry).unwrap();
    assert_eq!(layout.schema_version(), 1);
    assert_eq!(layout.template_name(), "碧蓝航线标准工作簿布局");
    assert_eq!(layout.sheets().len(), 10);
    assert_eq!(layout.fields().len(), 403);
    assert_eq!(layout.enum_options().len(), 55);
    assert_eq!(layout.styles().len(), 6);
    assert!(
        layout
            .sheets()
            .iter()
            .find(|sheet| sheet.stable_key() == "schema")
            .is_some_and(|sheet| sheet.generation() == LayoutGenerationMode::Hidden)
    );
    assert!(
        layout
            .fields()
            .iter()
            .any(|field| field.stable_key() == "weapons_json")
    );
    assert!(
        layout
            .fields()
            .iter()
            .any(|field| field.stable_key() == "field_value_format")
    );
}

#[test]
fn validated_user_text_values_survive_a_complete_template_rebuild() {
    let directory = TestDirectory::new("layout-rebuild");
    let original = directory.path().join("original.xlsx");
    let template_name = directory.path().join("template-name.xlsx");
    let purpose = directory.path().join("purpose.xlsx");
    let sheet_name = directory.path().join("sheet-name.xlsx");
    let field_name = directory.path().join("field-name.xlsx");
    let enum_label = directory.path().join("enum-label.xlsx");
    let style_color = directory.path().join("style-color.xlsx");
    let rebuilt = directory.path().join("rebuilt.xlsx");
    create_default_layout_workbook(&original).unwrap();
    edit_text_cell_to_new_file(
        &original,
        &template_name,
        FORMAT_SETTINGS,
        "B3",
        "用户自定义布局",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &template_name,
        &purpose,
        FORMAT_SETTINGS,
        "B4",
        "验证完整重建保留设置",
    )
    .unwrap();
    edit_text_cell_to_new_file(&purpose, &sheet_name, SHEET_SETTINGS, "C2", "计划输入").unwrap();
    edit_text_cell_to_new_file(
        &sheet_name,
        &field_name,
        FIELD_SETTINGS,
        "D2",
        "计划是否启用",
    )
    .unwrap();
    edit_text_cell_to_new_file(&field_name, &enum_label, FORMAT_SETTINGS, "C16", "保持原样")
        .unwrap();
    edit_text_cell_to_new_file(&enum_label, &style_color, FORMAT_SETTINGS, "B64", "112233")
        .unwrap();
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let source = load_workbook_layout(&style_color, &registry).unwrap();

    let bytes = build_layout_workbook_bytes(&rebuilt, &source).unwrap();
    write_new_file_bytes(&rebuilt, &bytes).unwrap();
    let output = load_workbook_layout(&rebuilt, &registry).unwrap();

    assert_eq!(output, source);
    assert_eq!(output.template_name(), "用户自定义布局");
    assert_eq!(output.purpose(), "验证完整重建保留设置");
    assert!(
        output
            .sheets()
            .iter()
            .any(|sheet| sheet.display_name() == "计划输入")
    );
    assert!(
        output
            .fields()
            .iter()
            .any(|field| field.display_name() == "计划是否启用")
    );
    assert!(
        output
            .enum_options()
            .iter()
            .any(|option| option.label() == "保持原样")
    );
    assert!(
        output
            .styles()
            .iter()
            .any(|style| style.background_color() == "112233")
    );
}

#[test]
fn boolean_dropdown_choices_round_trip_through_the_strict_loader() {
    let directory = TestDirectory::new("boolean-dropdowns");
    let original = directory.path().join("original.xlsx");
    let sheet_edited = directory.path().join("sheet-edited.xlsx");
    let field_edited = directory.path().join("field-edited.xlsx");
    let bold_edited = directory.path().join("bold-edited.xlsx");
    let wrap_edited = directory.path().join("wrap-edited.xlsx");
    create_default_layout_workbook(&original).unwrap();

    edit_text_cell_to_new_file(&original, &sheet_edited, "工作表设置", "F2", "否").unwrap();
    edit_text_cell_to_new_file(&sheet_edited, &field_edited, "字段设置", "H2", "是").unwrap();
    edit_text_cell_to_new_file(&field_edited, &bold_edited, "格式与下拉", "D64", "是").unwrap();
    edit_text_cell_to_new_file(&bold_edited, &wrap_edited, "格式与下拉", "G64", "是").unwrap();

    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let layout = load_workbook_layout(&wrap_edited, &registry).unwrap();
    assert!(!layout.sheets()[0].default_filter());
    assert!(layout.fields()[0].wrap());
    assert!(
        layout
            .styles()
            .iter()
            .find(|style| style.stable_key() == "read_only")
            .unwrap()
            .bold()
    );
    assert!(
        layout
            .styles()
            .iter()
            .find(|style| style.stable_key() == "read_only")
            .unwrap()
            .wrap()
    );
}

#[test]
fn control_dropdowns_follow_editable_labels_and_registered_constraints() {
    let directory = TestDirectory::new("control-dropdowns");
    let original = directory.path().join("original.xlsx");
    let generation_label = directory.path().join("generation-label.xlsx");
    let sheet_generation = directory.path().join("sheet-generation.xlsx");
    let field_generation = directory.path().join("field-generation.xlsx");
    let format_label = directory.path().join("format-label.xlsx");
    let propose_field_format = directory.path().join("propose-field-format.xlsx");
    let create_field_format = directory.path().join("create-field-format.xlsx");
    let first_field_format = directory.path().join("first-field-format.xlsx");
    let second_field_format = directory.path().join("second-field-format.xlsx");
    let third_field_format = directory.path().join("third-field-format.xlsx");
    let final_field_format = directory.path().join("final-field-format.xlsx");
    let bytes = build_layout_workbook_bytes(&original, &super::full_test_layout()).unwrap();
    write_new_file_bytes(&original, &bytes).unwrap();

    let package = PackageSnapshot::read(&original).unwrap();
    let names = defined_names(&package);
    assert_eq!(
        names.get("AZLW_GenerationRequired").map(String::as_str),
        Some("'格式与下拉'!$C$7:$C$8")
    );
    assert_eq!(
        names.get("AZLW_GenerationOptional").map(String::as_str),
        Some("'格式与下拉'!$C$7:$C$9")
    );
    assert_eq!(
        names.get("AZLW_FormatText").map(String::as_str),
        Some("'格式与下拉'!$C$10")
    );
    for (name, cell) in [
        ("AZLW_FormatInteger", "$C$11"),
        ("AZLW_FormatDecimal", "$C$12"),
        ("AZLW_FormatPercentage", "$C$13"),
        ("AZLW_FormatDateTime", "$C$14"),
        ("AZLW_FormatJson", "$C$15"),
    ] {
        let expected = format!("'格式与下拉'!{cell}");
        assert_eq!(names.get(name).map(String::as_str), Some(expected.as_str()));
    }

    let sheet_validations = validation_targets(&package, "工作表设置");
    assert!(has_exact_target(
        &sheet_validations["AZLW_GenerationRequired"],
        "B2:B7"
    ));
    assert!(has_exact_target(
        &sheet_validations["AZLW_GenerationOptional"],
        "B8"
    ));
    let field_validations = validation_targets(&package, "字段设置");
    assert!(has_exact_target(
        &field_validations["AZLW_GenerationOptional"],
        "C2:C179"
    ));
    assert!(has_exact_target(
        &field_validations["AZLW_FormatText"],
        "G2:G21"
    ));
    assert!(!field_validations.contains_key("AZLW_FormatPercentage"));

    edit_text_cell_to_new_file(&original, &generation_label, "格式与下拉", "C9", "省略").unwrap();
    edit_text_cell_to_new_file(
        &generation_label,
        &sheet_generation,
        "工作表设置",
        "B8",
        "省略",
    )
    .unwrap();
    let category_generation = directory.path().join("category-generation.xlsx");
    edit_text_cell_to_new_file(
        &sheet_generation,
        &category_generation,
        "工作表设置",
        "B11",
        "省略",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &category_generation,
        &field_generation,
        "字段设置",
        "C12",
        "省略",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &field_generation,
        &format_label,
        "格式与下拉",
        "C14",
        "时间",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &format_label,
        &propose_field_format,
        "字段设置",
        "G34",
        "时间",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &propose_field_format,
        &create_field_format,
        "字段设置",
        "G35",
        "时间",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &create_field_format,
        &first_field_format,
        "字段设置",
        "G251",
        "时间",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &first_field_format,
        &second_field_format,
        "字段设置",
        "G278",
        "时间",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &second_field_format,
        &third_field_format,
        "字段设置",
        "G387",
        "时间",
    )
    .unwrap();
    edit_text_cell_to_new_file(
        &third_field_format,
        &final_field_format,
        "字段设置",
        "G402",
        "时间",
    )
    .unwrap();

    let updated = directory.path().join("all-date-formats.xlsx");
    edit_text_cell_to_new_file(&final_field_format, &updated, "字段设置", "G403", "时间").unwrap();
    let final_field_format = updated;
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let layout = load_workbook_layout(&final_field_format, &registry).unwrap();
    assert!(layout.sheets().iter().any(|sheet| {
        sheet.stable_key() == "raw_data" && sheet.generation() == LayoutGenerationMode::Omitted
    }));
    assert!(layout.fields().iter().any(|field| {
        field.sheet_key() == "loadout_plan"
            && field.stable_key() == "name"
            && field.generation() == LayoutGenerationMode::Omitted
    }));
    assert_eq!(
        layout
            .fields()
            .iter()
            .filter(|field| field.value_format() == LayoutValueFormat::DateTime)
            .count(),
        7
    );
    assert!(layout.enum_options().iter().any(|option| {
        option.category_key() == "generation_mode"
            && option.stable_value() == "omitted"
            && option.label() == "省略"
    }));
    assert!(layout.enum_options().iter().any(|option| {
        option.category_key() == "value_format"
            && option.stable_value() == "date_time"
            && option.label() == "时间"
    }));
}

fn defined_names(package: &PackageSnapshot) -> BTreeMap<String, String> {
    let part_name = "xl/workbook.xml";
    let mut reader = Reader::from_reader(package.part(part_name).unwrap());
    let mut names = BTreeMap::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"definedName" => {
                let name = optional_attribute(&reader, part_name, &element, b"name")
                    .unwrap()
                    .expect("命名范围必须包含名称");
                let end = element.name();
                let formula = reader
                    .read_text(end)
                    .unwrap()
                    .decode()
                    .unwrap()
                    .into_owned();
                names.insert(name, formula);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    names
}

fn validation_targets(package: &PackageSnapshot, sheet_name: &str) -> BTreeMap<String, String> {
    let part_name = worksheet_part_name(package, sheet_name).unwrap();
    let mut reader = Reader::from_reader(package.part(&part_name).unwrap());
    let mut active_targets = None;
    let mut validations = BTreeMap::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Start(element) if element.local_name().as_ref() == b"dataValidation" => {
                active_targets = Some(
                    optional_attribute(&reader, &part_name, &element, b"sqref")
                        .unwrap()
                        .expect("数据验证必须包含目标区域"),
                );
            }
            Event::Start(element) if element.local_name().as_ref() == b"formula1" => {
                let end = element.name();
                let formula = reader
                    .read_text(end)
                    .unwrap()
                    .decode()
                    .unwrap()
                    .into_owned();
                validations.insert(
                    formula,
                    active_targets
                        .clone()
                        .expect("下拉公式必须位于数据验证元素中"),
                );
            }
            Event::End(element) if element.local_name().as_ref() == b"dataValidation" => {
                active_targets = None;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    validations
}

fn has_exact_target(targets: &str, expected: &str) -> bool {
    targets.split_whitespace().any(|target| target == expected)
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("测试需要 HOME 或 USERPROFILE");
        let identifier = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let path = home
            .join("suzushiro")
            .join("scratch")
            .join("azlw-layout-template-tests")
            .join(format!("{label}-{}-{identifier}", std::process::id()));
        std::fs::create_dir_all(path.parent().expect("测试目录必须包含父目录"))
            .expect("应建立模板测试根目录");
        std::fs::create_dir(&path).expect("模板测试目录不得与残留目录重名");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path).expect("应清理模板测试目录");
        }
    }
}

#[test]
fn default_slot_inputs_follow_their_current_equipment() {
    let template = super::LayoutWorkbookTemplate::from_defaults().unwrap();
    let fields: Vec<_> = template
        .fields
        .iter()
        .filter(|field| {
            field.sheet_key == "loadout_plan"
                && field.generation_label
                    == super::generation_default_label(LayoutGenerationMode::Visible)
        })
        .collect();
    assert_eq!(fields.len(), 50);
    for slot in 1..=5 {
        let current = 40 + (slot - 1) * 2;
        assert_eq!(
            fields[current].stable_key,
            format!("slot_{slot}_equipment_name")
        );
        assert_eq!(
            fields[current + 1].stable_key,
            format!("slot_{slot}_target_equipment_family")
        );
        assert_eq!(fields[current].display_name, format!("槽位{slot}当前装备"));
        assert_eq!(
            fields[current + 1].display_name,
            format!("槽位{slot}更换／处理")
        );
    }
    assert_eq!(
        template
            .fields
            .iter()
            .find(|field| field.sheet_key == "loadout_plan" && field.stable_key == "group_id")
            .unwrap()
            .generation_label,
        super::generation_default_label(LayoutGenerationMode::Omitted)
    );
    assert!(
        template
            .styles
            .iter()
            .all(|style| style.horizontal_alignment == "中" && style.vertical_alignment == "中")
    );
    assert_eq!(
        fields
            .iter()
            .filter(
                |field| field.editor != crate::application::LayoutEditor::ReadOnly
                    && field.stable_key.starts_with("slot_")
            )
            .count(),
        5
    );
    let equipment: Vec<_> = template
        .fields
        .iter()
        .filter(|field| {
            field.sheet_key == "equipment_inventory"
                && field.generation_label
                    == super::generation_default_label(LayoutGenerationMode::Visible)
        })
        .collect();
    assert_eq!(equipment.len(), 33);
    assert_eq!(
        equipment
            .iter()
            .take(6)
            .map(|field| field.stable_key.as_str())
            .collect::<Vec<_>>(),
        [
            "name",
            "equipment_type",
            "quantity",
            "family_warehouse_quantity",
            "craftable_actual",
            "family_owned_enhance_distribution"
        ]
    );

    assert_eq!(
        equipment
            .iter()
            .map(|field| field.stable_key.as_str())
            .collect::<Vec<_>>(),
        super::INVENTORY_COLUMN_ORDER
    );
    assert_eq!(
        equipment
            .iter()
            .filter(|field| field.editor != crate::application::LayoutEditor::ReadOnly)
            .map(|field| field.stable_key.as_str())
            .collect::<Vec<_>>(),
        ["operation", "processing_quantity", "target_enhance_level"]
    );
    assert_eq!(template.fields.len(), 403);
}

#[test]
fn read_scope_tracks_generated_skill_fields_and_raw_sheet() {
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let scope = |template: &super::LayoutWorkbookTemplate| {
        let path = Path::new("scope-layout.xlsx");
        let bytes = super::build_workbook(path, template).unwrap();
        super::super::load_layout_snapshot(path, &bytes, &registry)
            .unwrap()
            .read_scope()
            .ship_skill_effects()
    };
    let mut template = super::LayoutWorkbookTemplate::from_defaults().unwrap();
    assert!(!scope(&template));
    let field = template
        .fields
        .iter_mut()
        .find(|field| {
            field.sheet_key == "loadout_plan" && field.stable_key == "skills_effect_parameters"
        })
        .unwrap();
    field.generation_label =
        super::generation_default_label(LayoutGenerationMode::Hidden).to_owned();
    assert!(scope(&template));
    template
        .fields
        .iter_mut()
        .find(|field| {
            field.sheet_key == "loadout_plan" && field.stable_key == "skills_effect_parameters"
        })
        .unwrap()
        .generation_label =
        super::generation_default_label(LayoutGenerationMode::Omitted).to_owned();
    assert!(!scope(&template));
    template
        .sheets
        .iter_mut()
        .find(|sheet| sheet.stable_key == "raw_data")
        .unwrap()
        .generation_label =
        super::generation_default_label(LayoutGenerationMode::Hidden).to_owned();
    assert!(scope(&template));
}

#[test]
fn equipment_details_follow_only_generated_consumers() {
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    for (weapons, skills, summary, raw) in [
        (false, false, false, false),
        (true, false, false, false),
        (false, true, false, false),
        (true, true, false, false),
        (false, false, true, false),
        (false, false, false, true),
    ] {
        let mut template = super::LayoutWorkbookTemplate::from_defaults().unwrap();
        for field in &mut template.fields {
            if field.sheet_key != "equipment_inventory" {
                continue;
            }
            let enabled = match field.stable_key.as_str() {
                "weapons_json" => weapons,
                "skill_effects_json" => skills,
                "effect_summary" => summary,
                _ => continue,
            };
            field.generation_label = super::generation_default_label(if enabled {
                LayoutGenerationMode::Hidden
            } else {
                LayoutGenerationMode::Omitted
            })
            .to_owned();
        }
        if raw {
            template
                .sheets
                .iter_mut()
                .find(|sheet| sheet.stable_key == "raw_data")
                .unwrap()
                .generation_label =
                super::generation_default_label(LayoutGenerationMode::Hidden).to_owned();
        }
        let path = Path::new("equipment-scope-layout.xlsx");
        let bytes = super::build_workbook(path, &template).unwrap();
        let layout = super::super::load_layout_snapshot(path, &bytes, &registry).unwrap();
        assert_eq!(layout.read_scope().equipment_weapons(), weapons || raw);
        assert_eq!(
            layout.read_scope().equipment_skill_effects(),
            skills || summary || raw
        );
        let state = crate::adapters::device::game_state_mapper::golden_fixture::golden_game_state();
        let requested = layout.read_scope();
        let state = crate::domain::GameState::new(
            state.source().clone().with_read_scope(requested),
            state.ships().clone(),
            state.ship_catalog().clone(),
            state.equipment_catalog().clone(),
            crate::domain::EquipmentDetailCatalog::new(
                if requested.equipment_weapons() {
                    state.equipment_details().weapons().to_vec()
                } else {
                    Vec::new()
                },
                if requested.equipment_skill_effects() {
                    state.equipment_details().skills().to_vec()
                } else {
                    Vec::new()
                },
            ),
            state.equipment_inventory().clone(),
            state.bag().clone(),
            state.resources(),
            state.raw_records().clone(),
        );
        let projection = crate::application::project_game_state_to_workbook(&state).unwrap();
        let build = crate::adapters::workbook::projection_writer::build_projection_workbook_bytes(
            path,
            &layout,
            &projection,
            1_700_000_000_123,
        )
        .unwrap();
        crate::adapters::workbook::projection_writer::verify_projection_workbook(
            path,
            &build.bytes,
            &layout,
            &projection,
        )
        .unwrap();
        if !requested.equipment_weapons() || !requested.equipment_skill_effects() {
            let default_bytes = super::build_workbook(
                path,
                &super::LayoutWorkbookTemplate::from_defaults().unwrap(),
            )
            .unwrap();
            let full = super::super::load_layout_snapshot(path, &default_bytes, &registry).unwrap();
            let error =
                crate::adapters::workbook::projection_writer::build_projection_workbook_bytes(
                    path,
                    &full,
                    &projection,
                    1_700_000_000_123,
                )
                .err()
                .expect("缺少详情的状态必须拒绝完整布局");
            assert!(error.to_string().contains("模板请求装备详情"));
        }

        assert!(
            layout
                .fields()
                .iter()
                .any(|field| field.sheet_key() == "equipment_inventory"
                    && field.stable_key() == "data_complete"
                    && field.generation() == LayoutGenerationMode::Omitted)
        );
    }
}

#[test]
fn technology_columns_follow_experience_and_control_extra_reads() {
    let mut template = super::LayoutWorkbookTemplate::from_defaults().unwrap();
    template
        .sheets
        .iter_mut()
        .find(|sheet| sheet.stable_key == "ship_technology")
        .unwrap()
        .generation_label =
        super::generation_default_label(LayoutGenerationMode::Omitted).to_owned();
    let visible: Vec<_> = template
        .fields
        .iter()
        .filter(|f| f.sheet_key == "loadout_plan" && f.generation_label == "显示")
        .map(|f| f.stable_key.as_str())
        .collect();
    let start = visible
        .iter()
        .position(|key| *key == "total_experience")
        .unwrap();
    assert_eq!(
        &visible[start + 2..start + 5],
        crate::application::TECHNOLOGY_FIELDS
    );
    assert_eq!(visible[start + 1], "technology_bonus");
    assert_eq!(visible.len(), 50);
    let path = Path::new("technology-scope.xlsx");
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    for generation in [
        LayoutGenerationMode::Omitted,
        LayoutGenerationMode::Hidden,
        LayoutGenerationMode::Visible,
    ] {
        for field in &mut template.fields {
            if field.sheet_key == "loadout_plan"
                && (field.stable_key == "technology_bonus"
                    || crate::application::TECHNOLOGY_FIELDS.contains(&field.stable_key.as_str()))
            {
                field.generation_label = super::generation_default_label(generation).to_owned();
                let editor = if field.stable_key == "technology_bonus" {
                    crate::application::LayoutEditor::Text
                } else {
                    crate::application::LayoutEditor::ReadOnly
                };
                assert_eq!(field.editor, editor);
                assert!(field.wrap);
            }
        }
        let bytes = super::build_workbook(path, &template).unwrap();
        let layout = super::super::load_layout_snapshot(path, &bytes, &registry).unwrap();
        assert_eq!(
            layout.read_scope().ship_technology(),
            generation != LayoutGenerationMode::Omitted
        );
    }
    for field in &mut template.fields {
        if field.sheet_key == "loadout_plan"
            && crate::application::TECHNOLOGY_FIELDS.contains(&field.stable_key.as_str())
        {
            field.generation_label =
                super::generation_default_label(LayoutGenerationMode::Omitted).to_owned();
        }
    }
    let bytes = super::build_workbook(path, &template).unwrap();
    let layout = super::super::load_layout_snapshot(path, &bytes, &registry).unwrap();
    assert!(layout.read_scope().ship_technology());
}

#[test]
fn technology_category_template_controls_columns_and_reads() {
    let mut template = super::LayoutWorkbookTemplate::from_defaults().unwrap();
    let fields: Vec<_> = template
        .fields
        .iter()
        .filter(|field| field.sheet_key == "ship_technology")
        .collect();
    assert_eq!(
        fields
            .iter()
            .map(|field| field.stable_key.as_str())
            .collect::<Vec<_>>(),
        crate::application::TECHNOLOGY_VIEW_FIELDS
    );
    assert!(
        fields
            .iter()
            .all(|field| field.editor == crate::application::LayoutEditor::ReadOnly)
    );
    for field in &mut template.fields {
        if field.sheet_key == "loadout_plan"
            && (field.stable_key == "technology_bonus"
                || crate::application::TECHNOLOGY_FIELDS.contains(&field.stable_key.as_str()))
        {
            field.generation_label =
                super::generation_default_label(LayoutGenerationMode::Omitted).to_owned();
        }
    }
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let path = Path::new("technology-template.xlsx");
    for generation in [
        LayoutGenerationMode::Visible,
        LayoutGenerationMode::Hidden,
        LayoutGenerationMode::Omitted,
    ] {
        template
            .sheets
            .iter_mut()
            .find(|sheet| sheet.stable_key == "ship_technology")
            .unwrap()
            .generation_label = super::generation_default_label(generation).to_owned();
        let bytes = super::build_workbook(path, &template).unwrap();
        let layout = super::super::load_layout_snapshot(path, &bytes, &registry).unwrap();
        assert_eq!(
            layout.read_scope().ship_technology(),
            generation != LayoutGenerationMode::Omitted
        );
    }
}

#[test]
fn result_sheets_default_to_six_and_nine_visible_fields() {
    let template = super::LayoutWorkbookTemplate::from_defaults().unwrap();
    for (sheet_key, expected) in [
        ("check_results", super::CHECK_RESULT_COLUMN_ORDER),
        ("execution_results", super::EXECUTION_RESULT_COLUMN_ORDER),
    ] {
        let fields: Vec<_> = template
            .fields
            .iter()
            .filter(|field| field.sheet_key == sheet_key)
            .collect();
        let visible: Vec<_> = fields
            .iter()
            .filter(|field| field.generation_label == "显示")
            .map(|field| field.stable_key.as_str())
            .collect();
        assert_eq!(visible, expected);
        for field in fields {
            if !expected.contains(&field.stable_key.as_str()) {
                assert_eq!(
                    field.generation_label,
                    if field.required {
                        "隐藏"
                    } else {
                        "不生成"
                    }
                );
            }
        }
    }
}

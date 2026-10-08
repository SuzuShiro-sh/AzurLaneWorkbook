//! 覆盖工作簿计划读取、单元格解析和输入校验的单元测试。

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use calamine::{Data, Range, Reader as CalamineReader, Xlsx, open_workbook_from_rs};

use super::load_workbook_plan_from_xlsx;
use crate::adapters::device::game_state_mapper::golden_fixture::{
    golden_game_state, golden_game_state_with_unowned_config,
};
use crate::adapters::tool_root::ToolRoot;
use crate::adapters::workbook::edit_text_cell_to_new_file;
use crate::adapters::workbook::generation::XlsxWorkbookGenerationPort;
use crate::adapters::workbook::layout::XlsxWorkbookPort;
use crate::application::{WorkbookGenerationPort, WorkbookPort, WorkbookProjectionV4};
use crate::domain::{EquipmentInventoryActionKind, EquipmentSourceRef, SlotTarget, SourcePolicy};

#[test]
fn missing_sheet_fields_keep_the_read_boundary_error() {
    use super::WorkbookPlanError;
    use crate::application::{
        LayoutColumnWidth, LayoutEditor, LayoutGenerationMode, LayoutValueFormat,
        WorkbookFieldLayout, WorkbookLayout,
    };

    let layout = WorkbookLayout::new(
        1,
        "样本".into(),
        "空字段".into(),
        Vec::new(),
        vec![WorkbookFieldLayout::new(
            "loadout_plan".into(),
            "omitted".into(),
            LayoutGenerationMode::Omitted,
            "省略".into(),
            1,
            LayoutColumnWidth::from_hundredths(1_000).unwrap(),
            LayoutValueFormat::Text,
            false,
            String::new(),
            "GameState.note".into(),
            LayoutEditor::ReadOnly,
            None,
            false,
        )],
        Vec::new(),
        Vec::new(),
        "b".repeat(64),
    )
    .unwrap();

    match super::generated_fields(&layout, "loadout_plan") {
        Err(WorkbookPlanError::InvalidCell { field, message, .. }) => {
            assert_eq!(field, "字段");
            assert_eq!(message, "布局缺少可读取字段");
        }
        other => panic!("应保留空字段边界错误，实际 {other:?}"),
    }
}

#[test]
fn disabled_rows_produce_an_empty_desired_state() {
    let fixture = generated_workbook("disabled");

    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;

    assert!(desired.is_empty());
}

#[test]
fn xlsx_workbook_port_reads_a_tool_root_workbook() {
    let fixture = generated_workbook("port");
    let registry = WorkbookProjectionV4::layout_registry().unwrap();
    let layout_path = fixture.parent.join("port-layout.xlsx");
    let bytes = crate::adapters::workbook::layout::template::build_layout_workbook_bytes(
        &layout_path,
        &fixture.layout,
    )
    .unwrap();
    fs::write(&layout_path, bytes).unwrap();
    let port = XlsxWorkbookPort::new(fixture.tool_root.clone(), layout_path, registry);
    let workbook = fixture.tool_root.existing_workbook("input.xlsx").unwrap();

    let inputs = port
        .load_plan_inputs(&workbook)
        .expect("工作簿端口应一次读取工具根目录内的输入");
    let workbook_bytes = fs::read(
        fixture
            .tool_root
            .as_path()
            .join("data/workbooks/input.xlsx"),
    )
    .unwrap();
    assert_eq!(
        inputs.source_package_sha256,
        suzushiro_content_digest::sha256_bytes(&workbook_bytes)
    );
    let desired = inputs.desired;
    let inventory_plan = inputs.inventory;

    assert!(desired.is_empty());
    assert!(inventory_plan.is_empty());
}

#[test]
fn default_inventory_rows_produce_an_empty_plan() {
    let fixture = generated_workbook("inventory-disabled");

    let plan = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .inventory;

    assert!(plan.is_empty());
}

#[test]
fn unowned_rows_are_read_only_inputs_even_after_cell_tampering() {
    for (field, value) in [
        ("operation", "拆解"),
        ("processing_quantity", "1"),
        ("target_enhance_level", "1"),
        ("note", "伪造输入"),
    ] {
        let fixture = generated_unowned_workbook(&format!("unowned-input-{field}"));
        let row = find_inventory_row(&fixture.path, &fixture.layout, "unowned:");
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);

        let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

        assert!(
            error.to_string().contains("未持有")
                || error.to_string().contains("锁定")
                || error.to_string().contains("必须选择操作")
        );
    }
}

#[test]
fn unowned_rows_reject_forged_source_identity() {
    for (field, value) in [
        ("source_ref", "unowned:1001"),
        ("source_type", "warehouse"),
        ("config_id", "1001"),
        ("quantity", "1"),
        ("runtime_id", "7001"),
        ("ship_instance_id", "9001"),
        ("ship_name", "伪造舰船"),
        ("slot_index", "1"),
    ] {
        let fixture = generated_unowned_workbook(&format!("unowned-source-{field}"));
        let row = find_inventory_row(&fixture.path, &fixture.layout, "unowned:");
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);

        let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

        assert!(error.to_string().contains("未持有"));
    }
}

#[test]
fn reads_a_warehouse_dismantle_action() {
    let fixture = generated_workbook("inventory-dismantle");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "operation",
        &enum_label(&fixture.layout, "inventory_operation", "dismantle"),
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "1",
    );
    for (field, value) in [
        ("locked", "否"),
        ("protected", "否"),
        ("dismantlable", "是"),
        ("data_complete", "是"),
    ] {
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);
    }

    let plan = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .inventory;

    assert_eq!(plan.len(), 1);
    let action = plan.actions()[0];
    assert_eq!(action.kind(), EquipmentInventoryActionKind::Dismantle);
    assert_eq!(action.dismantle_quantity(), Some(1));
    assert!(matches!(action.source(), EquipmentSourceRef::Warehouse(_)));
}

#[test]
fn reads_a_target_enhance_intent_with_an_explicit_quantity() {
    let fixture = generated_workbook("inventory-enhance");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(&fixture.path, &fixture.layout, row, "operation", "强化");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "target_enhance_level",
        "1",
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "1",
    );

    let plan = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .inventory;

    assert_eq!(plan.len(), 1);
    let action = plan.actions()[0];
    assert_eq!(action.kind(), EquipmentInventoryActionKind::Keep);
    assert_eq!(action.target_enhance_level().unwrap().get(), 1);
    assert_eq!(action.enhance_quantity(), Some(1));
    assert!(action.dismantle_quantity().is_none());
}

#[test]
fn rejects_an_enhance_level_without_an_explicit_quantity() {
    let fixture = generated_workbook("inventory-enhance-missing-quantity");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(&fixture.path, &fixture.layout, row, "operation", "强化");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "target_enhance_level",
        "1",
    );

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

    assert!(error.to_string().contains("强化必须填写"));
}

#[test]
fn rejects_an_enhance_quantity_above_the_warehouse_stack() {
    let fixture = generated_workbook("inventory-enhance-quantity-overflow");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(&fixture.path, &fixture.layout, row, "operation", "强化");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "target_enhance_level",
        "1",
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "999999",
    );

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();
    let application = super::map_desired_state_error(&fixture.path, error);

    assert_eq!(application.code().as_str(), "INPUT_INVALID");
    assert_eq!(
        application.context().get("field").unwrap(),
        "processing_quantity"
    );
}

#[test]
fn rejects_a_ship_enhance_quantity_other_than_one() {
    let fixture = generated_workbook("inventory-ship-enhance-quantity");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "ship:");
    edit_inventory_cell(&fixture.path, &fixture.layout, row, "operation", "强化");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "target_enhance_level",
        "1",
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "2",
    );

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();
    let application = super::map_desired_state_error(&fixture.path, error);

    assert_eq!(application.code().as_str(), "INPUT_INVALID");
    assert_eq!(
        application.context().get("field").unwrap(),
        "processing_quantity"
    );
}

#[test]
fn rejects_inventory_quantity_conflicts_with_cell_context() {
    let fixture = generated_workbook("inventory-invalid-quantity");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "operation",
        &enum_label(&fixture.layout, "inventory_operation", "dismantle"),
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "999999",
    );
    for (field, value) in [
        ("locked", "否"),
        ("protected", "否"),
        ("dismantlable", "是"),
        ("data_complete", "是"),
    ] {
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);
    }

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();
    let application = super::map_desired_state_error(&fixture.path, error);

    assert_eq!(application.code().as_str(), "INPUT_INVALID");
    assert_eq!(
        application.context().get("sheet").unwrap(),
        "equipment_inventory"
    );
    assert_eq!(
        application.context().get("field").unwrap(),
        "processing_quantity"
    );
}

#[test]
fn rejects_dismantle_when_runtime_safety_flags_are_incomplete() {
    let fixture = generated_workbook("inventory-safety-gap");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "operation",
        &enum_label(&fixture.layout, "inventory_operation", "dismantle"),
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "1",
    );
    for (field, value) in [
        ("locked", "否"),
        ("protected", "否"),
        ("dismantlable", "是"),
        ("data_complete", "否"),
    ] {
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);
    }

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();
    let application = super::map_desired_state_error(&fixture.path, error);

    assert_eq!(application.code().as_str(), "INPUT_INVALID");
    assert_eq!(application.context().get("field").unwrap(), "data_complete");
}

#[test]
fn rejects_processing_quantity_without_an_operation() {
    let fixture = generated_workbook("inventory-conflicting-fields");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "1",
    );

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

    assert!(matches!(
        error,
        super::WorkbookPlanError::InvalidCell { .. }
    ));
}

#[test]
fn reads_warehouse_equipment_target() {
    let fixture = generated_workbook("warehouse");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "[1000] 维修设施",
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_source_policy",
        &enum_label(&fixture.layout, "source_policy", "warehouse_only"),
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_allocation_priority",
        "0",
    );

    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;

    assert_eq!(desired.len(), 1);
    assert_eq!(desired.slots()[0].slot().to_string(), "9001:1");
    assert_eq!(desired.slots()[0].allocation_priority(), 0);
    match desired.slots()[0].target() {
        SlotTarget::Equipment(equipment) => {
            assert_eq!(equipment.family_id().get(), 1000);
            assert_eq!(equipment.source_policy(), SourcePolicy::WarehouseOnly);
            assert!(equipment.exact_source().is_none());
        }
        SlotTarget::Keep | SlotTarget::Empty => panic!("应读取为装备目标"),
    }
}

#[test]
fn reads_exact_ship_source_and_target_level() {
    let fixture = generated_workbook("ship-source");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "[1000] 维修设施",
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_source_policy",
        &enum_label(&fixture.layout, "source_policy", "exact_source"),
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_exact_source",
        "[ship:9001:4] 维修设施 +1",
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_enhance_level",
        "3",
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_allocation_priority",
        "2",
    );

    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;

    match desired.slots()[0].target() {
        SlotTarget::Equipment(equipment) => {
            assert_eq!(equipment.source_policy(), SourcePolicy::ExactSource);
            assert_eq!(equipment.target_enhance_level().unwrap().get(), 3);
            assert_eq!(
                equipment.exact_source().unwrap().to_string(),
                "舰船槽位 9001:4"
            );
        }
        SlotTarget::Keep | SlotTarget::Empty => panic!("应读取为装备目标"),
    }
}

#[test]
fn generated_xlsx_round_trips_multiple_slots_from_one_ship() {
    let fixture = generated_workbook("multiple-slots");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    for (field, value) in [
        ("slot_1_target_equipment_family", "卸下"),
        ("slot_5_target_equipment_family", "[1000] 维修设施"),
        ("slot_5_allocation_priority", "5"),
    ] {
        edit_loadout_cell(&fixture.path, &fixture.layout, row, field, value);
    }
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_5_source_policy",
        &enum_label(&fixture.layout, "source_policy", "warehouse_only"),
    );

    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;

    assert_eq!(desired.len(), 2);
    assert_eq!(desired.slots()[0].slot().to_string(), "9001:1");
    assert!(matches!(desired.slots()[0].target(), SlotTarget::Empty));
    assert_eq!(desired.slots()[1].slot().to_string(), "9001:5");
    assert!(matches!(
        desired.slots()[1].target(),
        SlotTarget::Equipment(equipment)
            if equipment.family_id().get() == 1000
                && equipment.source_policy() == SourcePolicy::WarehouseOnly
    ));
}

#[test]
fn parses_multiple_slots_per_ship_and_keeps_other_ships_independent() {
    let (layout, columns, mut range) = loadout_range(&["9001", "9002"]);
    for (row, slot, choice) in [(1, 1, "卸下"), (1, 3, "卸下"), (2, 2, "卸下")] {
        set_loadout_choice(&mut range, &columns, row, slot, choice);
    }

    let (_, first) = super::loadout::parse_loadout_row(&range, 2, &columns, &layout)
        .unwrap()
        .expect("持有舰船行必须返回配装计划");
    let (_, second) = super::loadout::parse_loadout_row(&range, 3, &columns, &layout)
        .unwrap()
        .expect("持有舰船行必须返回配装计划");

    assert_eq!(
        first
            .iter()
            .map(|desired| desired.slot().to_string())
            .collect::<Vec<_>>(),
        ["9001:1", "9001:3"]
    );
    assert!(matches!(first[0].target(), SlotTarget::Empty));
    assert!(matches!(first[1].target(), SlotTarget::Empty));
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].slot().to_string(), "9002:2");
    assert!(matches!(second[0].target(), SlotTarget::Empty));
}

#[test]
fn rejects_duplicate_ship_rows_with_different_target_slots() {
    let (layout, columns, mut range) = loadout_range(&["9001", "9001"]);
    set_loadout_choice(&mut range, &columns, 1, 1, "卸下");
    set_loadout_choice(&mut range, &columns, 2, 2, "卸下");

    let error = super::parse_loadout_rows(&range, &columns, &layout).unwrap_err();

    assert!(matches!(
        error,
        super::WorkbookPlanError::InvalidCell {
            ref sheet,
            row: 3,
            ref field,
            ref message,
        } if sheet == "loadout_plan"
            && field == "instance_id"
            && message.contains("舰船实例 ID 9001 重复")
    ));
}

#[test]
fn rejects_duplicate_ship_rows_without_targets() {
    let (layout, columns, range) = loadout_range(&["9001", "9001"]);

    let error = super::parse_loadout_rows(&range, &columns, &layout).unwrap_err();

    assert!(matches!(
        error,
        super::WorkbookPlanError::InvalidCell {
            ref sheet,
            row: 3,
            ref field,
            ..
        } if sheet == "loadout_plan" && field == "instance_id"
    ));
}

#[test]
fn rejects_target_level_without_an_equipment_choice() {
    let fixture = generated_workbook("keep-conflict");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");

    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_enhance_level",
        "1",
    );
    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

    assert!(matches!(
        error,
        super::WorkbookPlanError::InvalidCell { .. }
    ));
    let application = super::map_desired_state_error(&fixture.path, error);
    assert_eq!(application.code().as_str(), "INPUT_INVALID");
    assert_eq!(application.context().get("sheet").unwrap(), "loadout_plan");
    assert_eq!(
        application.context().get("field").unwrap(),
        "slot_1_target_equipment_family"
    );
}

#[test]
fn rejects_a_layout_snapshot_that_does_not_match_the_root_layout() {
    let fixture = generated_workbook("schema-mismatch");
    let row = 2;
    edit_schema_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "layout_hash",
        &"f".repeat(64),
    );

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

    assert!(error.to_string().contains("layout_hash"));
}

#[test]
fn rejects_incompatible_workbook_schema() {
    let fixture = generated_workbook("workbook-schema-mismatch");
    edit_schema_cell(
        &fixture.path,
        &fixture.layout,
        2,
        "workbook_schema_version",
        "18",
    );

    let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();

    let message = error.to_string();
    assert!(message.contains("workbook_schema_version"));
    assert!(message.contains("期望 19"));
    assert!(message.contains("实际 18"));
}

struct GeneratedWorkbook {
    parent: PathBuf,
    path: PathBuf,
    tool_root: ToolRoot,
    layout: crate::application::WorkbookLayout,
}

impl Drop for GeneratedWorkbook {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.parent) {
            eprintln!("清理配装读取测试目录失败: {error}");
        }
    }
}

fn generated_workbook(label: &str) -> GeneratedWorkbook {
    generated_workbook_from_state(label, &golden_game_state())
}

fn generated_unowned_workbook(label: &str) -> GeneratedWorkbook {
    generated_workbook_from_state(label, &golden_game_state_with_unowned_config())
}

fn generated_workbook_from_state(
    label: &str,
    state: &crate::domain::GameState,
) -> GeneratedWorkbook {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .expect("测试需要 HOME 或 USERPROFILE");
    let parent = home
        .join("suzushiro/scratch/azlw-workbook-reader-tests")
        .join(format!("{label}-{}", unique_suffix()));
    let root = parent.join("tool-root");
    fs::create_dir_all(&root).expect("应建立测试工具根目录");
    let tool_root = ToolRoot::open(&root).unwrap();
    let layout = crate::adapters::workbook::layout::template::full_test_layout();
    let projection = crate::application::project_game_state_to_workbook(state).unwrap();
    let generation = XlsxWorkbookGenerationPort::new(tool_root.clone());
    let report = generation
        .generate_workbook(Some("input.xlsx"), &layout, projection)
        .unwrap();
    let path = root.join(report.output_path());
    GeneratedWorkbook {
        parent,
        path,
        tool_root,
        layout,
    }
}

fn loadout_range(
    instance_ids: &[&str],
) -> (
    crate::application::WorkbookLayout,
    BTreeMap<String, u32>,
    Range<Data>,
) {
    let layout = crate::adapters::workbook::layout::template::full_test_layout();
    let fields: Vec<_> = layout
        .fields()
        .iter()
        .filter(|field| {
            field.sheet_key() == "loadout_plan"
                && field.generation() != crate::application::LayoutGenerationMode::Omitted
        })
        .collect();
    let columns: BTreeMap<String, u32> = fields
        .iter()
        .enumerate()
        .map(|(column, field)| {
            (
                field.stable_key().to_owned(),
                u32::try_from(column).unwrap(),
            )
        })
        .collect();
    let mut range = Range::new(
        (0, 0),
        (
            u32::try_from(instance_ids.len()).unwrap(),
            u32::try_from(fields.len() - 1).unwrap(),
        ),
    );
    for (column, field) in fields.iter().enumerate() {
        range.set_value(
            (0, u32::try_from(column).unwrap()),
            Data::String(field.display_name().to_owned()),
        );
    }
    for (index, instance_id) in instance_ids.iter().enumerate() {
        let row = u32::try_from(index + 1).unwrap();
        range.set_value(
            (row, columns["source_type"]),
            Data::String("owned".to_owned()),
        );
        range.set_value(
            (row, columns["source_ref"]),
            Data::String(format!("owned:{instance_id}")),
        );
        range.set_value(
            (row, columns["instance_id"]),
            Data::String((*instance_id).to_owned()),
        );
    }
    (layout, columns, range)
}

fn set_loadout_choice(
    range: &mut Range<Data>,
    columns: &BTreeMap<String, u32>,
    row: u32,
    slot: u8,
    choice: &str,
) {
    range.set_value(
        (
            row,
            columns[&format!("slot_{slot}_target_equipment_family")],
        ),
        Data::String(choice.to_owned()),
    );
}

fn find_loadout_row(
    path: &Path,
    layout: &crate::application::WorkbookLayout,
    instance_id: &str,
) -> u32 {
    let bytes = fs::read(path).unwrap();
    let mut workbook: Xlsx<std::io::Cursor<&[u8]>> =
        open_workbook_from_rs(std::io::Cursor::new(bytes.as_slice())).unwrap();
    let range = workbook.worksheet_range("配装计划").unwrap();
    let instance_column =
        u32::try_from(field_column(layout, "loadout_plan", "instance_id")).unwrap();
    for row in 1..u32::try_from(range.height()).unwrap() {
        if matches!(range.get_value((row, instance_column)), Some(Data::String(value)) if value == instance_id)
        {
            return row + 1;
        }
    }
    panic!("测试工作簿缺少舰船配装行 {instance_id}");
}

fn find_inventory_row(
    path: &Path,
    layout: &crate::application::WorkbookLayout,
    prefix: &str,
) -> u32 {
    let bytes = fs::read(path).unwrap();
    let mut workbook: Xlsx<std::io::Cursor<&[u8]>> =
        open_workbook_from_rs(std::io::Cursor::new(bytes.as_slice())).unwrap();
    let range = workbook.worksheet_range("装备总表").unwrap();
    let has_ref = layout.fields().iter().any(|field| {
        field.sheet_key() == "equipment_inventory"
            && field.stable_key() == "source_ref"
            && field.generation() != crate::application::LayoutGenerationMode::Omitted
    });
    for row in 1..u32::try_from(range.height()).unwrap() {
        let read = |key: &str| {
            let column = field_column(layout, "equipment_inventory", key) as u32;
            range
                .get_value((row, column))
                .map(ToString::to_string)
                .unwrap_or_default()
        };
        let source = if has_ref {
            read("source_ref")
        } else {
            match read("source_type").as_str() {
                "仓库" => format!("warehouse:{}", read("config_id")),
                "舰船" => format!("ship:{}:{}", read("ship_instance_id"), read("slot_index")),
                "未持有" => format!("unowned:{}", read("config_id")),
                other => panic!("未知测试来源 {other}"),
            }
        };
        if source.starts_with(prefix) {
            return row + 1;
        }
    }
    panic!("测试工作簿缺少装备总表来源 {prefix}");
}

fn edit_loadout_cell(
    path: &Path,
    layout: &crate::application::WorkbookLayout,
    row: u32,
    field: &str,
    value: &str,
) {
    let column = field_column(layout, "loadout_plan", field);
    let cell = format!("{}{}", excel_column(column), row);
    edit_cell(path, "配装计划", &cell, value);
}

fn edit_inventory_cell(
    path: &Path,
    layout: &crate::application::WorkbookLayout,
    row: u32,
    field: &str,
    value: &str,
) {
    let column = field_column(layout, "equipment_inventory", field);
    let cell = format!("{}{}", excel_column(column), row);
    edit_cell(path, "装备总表", &cell, value);
}

fn edit_schema_cell(
    path: &Path,
    layout: &crate::application::WorkbookLayout,
    row: u32,
    field: &str,
    value: &str,
) {
    let column = field_column(layout, "schema", field);
    let cell = format!("{}{}", excel_column(column), row);
    edit_cell(path, "_schema", &cell, value);
}

fn edit_cell(path: &Path, sheet: &str, cell: &str, value: &str) {
    let destination = path.with_file_name(format!(".reader-edit-{}.xlsx", unique_suffix()));
    edit_text_cell_to_new_file(path, &destination, sheet, cell, value).unwrap();
    fs::remove_file(path).unwrap();
    fs::rename(destination, path).unwrap();
}

fn field_column(layout: &crate::application::WorkbookLayout, sheet: &str, field: &str) -> usize {
    layout
        .fields()
        .iter()
        .filter(|candidate| {
            candidate.sheet_key() == sheet
                && candidate.generation() != crate::application::LayoutGenerationMode::Omitted
        })
        .position(|candidate| candidate.stable_key() == field)
        .expect("测试字段必须存在")
}

fn enum_label(
    layout: &crate::application::WorkbookLayout,
    category: &str,
    stable_value: &str,
) -> String {
    layout
        .enum_options()
        .iter()
        .find(|option| option.category_key() == category && option.stable_value() == stable_value)
        .expect("测试枚举必须存在")
        .label()
        .to_owned()
}

fn excel_column(mut zero_based: usize) -> String {
    let mut value = String::new();
    zero_based += 1;
    while zero_based > 0 {
        let remainder = (zero_based - 1) % 26;
        value.push(char::from(b'A' + u8::try_from(remainder).unwrap()));
        zero_based = (zero_based - 1) / 26;
    }
    value.chars().rev().collect()
}

fn unique_suffix() -> String {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).expect("测试需要操作系统随机源");
    format!("{}-{:032x}", std::process::id(), u128::from_le_bytes(bytes))
}

#[test]
fn compact_equipment_operation_uses_warehouse_then_compose_and_default_priority() {
    let fixture = generated_workbook("compact-equip");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "维修设施〔1000〕",
    );
    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;
    assert_eq!(desired.len(), 1);
    let SlotTarget::Equipment(equipment) = desired.slots()[0].target() else {
        panic!("应为换装")
    };
    assert_eq!(
        equipment.source_policy(),
        SourcePolicy::WarehouseThenCompose
    );
    assert_eq!(equipment.family_id().get(), 1000);
    assert_eq!(desired.slots()[0].allocation_priority(), 0);
}

#[test]
fn default_slot_choice_inputs_round_trip() {
    let mut fixture = generated_workbook("compact-default-input");
    use_default_layout(&mut fixture, &golden_game_state());
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired
            .is_empty()
    );
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .inventory
            .is_empty()
    );
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "维修设施〔1000〕",
    );
    assert_eq!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired
            .len(),
        1
    );
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    for (field, value) in [
        ("operation", "强化"),
        ("processing_quantity", "1"),
        ("target_enhance_level", "1"),
    ] {
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);
    }
    assert_eq!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .inventory
            .len(),
        1
    );
}

#[test]
fn unload_rejects_a_target_enhance_level() {
    let fixture = generated_workbook("compact-unload");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "卸下",
    );
    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;
    assert!(matches!(desired.slots()[0].target(), SlotTarget::Empty));
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_enhance_level",
        "1",
    );
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap_err()
            .to_string()
            .contains("不能填写目标强化等级")
    );
}

#[test]
fn compact_inventory_dismantle_uses_processing_quantity() {
    let fixture = generated_workbook("compact-dismantle");
    let row = find_inventory_row(&fixture.path, &fixture.layout, "warehouse:");
    for (field, value) in [
        ("operation", "拆解"),
        ("processing_quantity", "1"),
        ("locked", "否"),
        ("protected", "否"),
        ("dismantlable", "是"),
        ("data_complete", "是"),
    ] {
        edit_inventory_cell(&fixture.path, &fixture.layout, row, field, value);
    }
    let inventory = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .inventory;
    assert_eq!(inventory.actions()[0].dismantle_quantity(), Some(1));
    assert_eq!(
        inventory.actions()[0].kind(),
        EquipmentInventoryActionKind::Dismantle
    );
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "target_enhance_level",
        "1",
    );
    assert!(load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).is_err());
}

fn use_default_layout(fixture: &mut GeneratedWorkbook, state: &crate::domain::GameState) {
    fixture.layout = crate::adapters::workbook::load_workbook_layout(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        &WorkbookProjectionV4::layout_registry().unwrap(),
    )
    .unwrap();
    let projection = crate::application::project_game_state_to_workbook(state).unwrap();
    let bytes = crate::adapters::workbook::projection_writer::build_projection_workbook_bytes(
        &fixture.path,
        &fixture.layout,
        &projection,
        1_700_000_000_123,
    )
    .unwrap()
    .bytes;
    fs::write(&fixture.path, bytes).unwrap();
}

#[test]
fn slot_choices_compile_unequip_and_dismantle_with_existing_inventory_rules() {
    let state = crate::application::test_support::plan_game_state();
    let mut fixture = generated_workbook("slot-actions");
    use_default_layout(&mut fixture, &state);
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    for choice in ["卸下", "拆解"] {
        edit_loadout_cell(
            &fixture.path,
            &fixture.layout,
            row,
            "slot_1_target_equipment_family",
            choice,
        );
        let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired;
        let inventory = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .inventory;
        assert!(matches!(desired.slots()[0].target(), SlotTarget::Empty));
        assert_eq!(inventory.len(), usize::from(choice == "拆解"));
        let report =
            crate::application::compile_plan_with_inventory(&state, &desired, &inventory).unwrap();
        assert!(matches!(
            report.plan().steps()[0],
            crate::application::PlanStep::Unequip { .. }
        ));
        assert_eq!(
            report.plan().steps().len(),
            if choice == "拆解" { 2 } else { 1 }
        );
        if choice == "拆解" {
            assert!(matches!(
                report.plan().steps()[1],
                crate::application::PlanStep::Dismantle { quantity: 1, .. }
            ));
        }
    }
    use_default_layout(&mut fixture, &state);
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired
            .is_empty()
    );
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .inventory
            .is_empty()
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "未知操作",
    );
    assert!(load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).is_err());
}

#[test]
fn slot_dismantle_rejects_empty_and_protected_equipment_and_duplicate_actions() {
    let state = crate::application::test_support::plan_game_state();
    let mut fixture = generated_workbook("slot-dismantle-validation");
    use_default_layout(&mut fixture, &state);
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    for slot in [4, 2] {
        let field = format!("slot_{slot}_target_equipment_family");
        edit_loadout_cell(&fixture.path, &fixture.layout, row, &field, "拆解");
        let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired;
        let inventory = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .inventory;
        let error = crate::application::compile_plan_with_inventory(&state, &desired, &inventory)
            .unwrap_err();
        if slot == 4 {
            assert!(matches!(
                error,
                crate::application::PlanCheckError::InventoryDismantleProtected { .. }
            ));
        } else {
            assert!(matches!(
                error,
                crate::application::PlanCheckError::InventorySourceNotFound { .. }
            ));
        }
        use_default_layout(&mut fixture, &state);
    }
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "拆解",
    );
    let inventory_row = find_inventory_row(&fixture.path, &fixture.layout, "ship:9001:1");
    for (field, value) in [("operation", "拆解"), ("processing_quantity", "1")] {
        edit_inventory_cell(&fixture.path, &fixture.layout, inventory_row, field, value);
    }
    assert!(matches!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout),
        Err(super::WorkbookPlanError::InventoryModel {
            source: crate::domain::EquipmentInventoryActionError::DuplicateSource { .. }
        })
    ));
}

#[test]
fn default_inventory_sources_compile_from_visible_identity() {
    let state = crate::application::test_support::plan_game_state();
    let mut fixture = generated_workbook("default-inventory-sources");
    use_default_layout(&mut fixture, &state);
    let bytes = fs::read(&fixture.path).unwrap();
    let mut workbook: Xlsx<std::io::Cursor<&[u8]>> =
        open_workbook_from_rs(std::io::Cursor::new(bytes.as_slice())).unwrap();
    let range = workbook.worksheet_range("装备总表").unwrap();
    let source_column = field_column(&fixture.layout, "equipment_inventory", "source_type");
    let ship_column = field_column(&fixture.layout, "equipment_inventory", "ship_instance_id");
    let slot_column = field_column(&fixture.layout, "equipment_inventory", "slot_index");
    let row = (1..range.height()).find(|row| matches!(range.get_value((*row as u32,source_column as u32)),Some(Data::String(text)) if text == "舰船") && matches!(range.get_value((*row as u32,ship_column as u32)),Some(Data::String(text)) if text == "9001") && matches!(range.get_value((*row as u32,slot_column as u32)),Some(Data::Float(v)) if *v == 1.0)).unwrap() as u32 + 1;
    for key in [
        "source_ref",
        "family_id",
        "locked",
        "protected",
        "data_complete",
    ] {
        assert!(
            fixture
                .layout
                .fields()
                .iter()
                .any(|field| field.sheet_key() == "equipment_inventory"
                    && field.stable_key() == key
                    && field.generation() == crate::application::LayoutGenerationMode::Omitted)
        );
    }
    edit_inventory_cell(&fixture.path, &fixture.layout, row, "operation", "拆解");
    edit_inventory_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "processing_quantity",
        "1",
    );
    let inventory = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .inventory;
    assert!(matches!(
        inventory.actions()[0].source(),
        EquipmentSourceRef::ShipSlot(_)
    ));
    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;
    let report =
        crate::application::compile_plan_with_inventory(&state, &desired, &inventory).unwrap();
    assert!(report.plan().steps().iter().any(|step| matches!(
        step,
        crate::application::PlanStep::Dismantle { quantity: 1, .. }
    )));
    edit_inventory_cell(&fixture.path, &fixture.layout, row, "quantity", "2");
    assert!(load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).is_err());
}

#[test]
fn default_layout_reads_unowned_ships_without_group_id_and_rejects_operations() {
    let state = golden_game_state();
    let roster = crate::domain::ShipRoster::new_with_skill_effects(
        state.ships().source().clone(),
        Vec::new(),
        crate::domain::SkillEffectEvidenceCatalog::new(Vec::new()),
    );
    let state = crate::domain::GameState::new(
        state.source().clone(),
        roster,
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );
    let mut fixture = generated_workbook("unowned-compact-loadout");
    use_default_layout(&mut fixture, &state);
    assert!(
        !fixture
            .layout
            .fields()
            .iter()
            .any(|field| field.sheet_key() == "loadout_plan"
                && field.stable_key() == "group_id"
                && field.generation() != crate::application::LayoutGenerationMode::Omitted)
    );
    let mut workbook: Xlsx<_> = calamine::open_workbook(&fixture.path).unwrap();
    assert!(workbook.worksheet_range("配装计划").unwrap().height() > 1);
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired
            .is_empty()
    );
    assert!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .inventory
            .is_empty()
    );
    for input in ["卸下", "拆解", "测试装备〔1001〕"] {
        edit_loadout_cell(
            &fixture.path,
            &fixture.layout,
            2,
            "slot_1_target_equipment_family",
            input,
        );
        let error = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).unwrap_err();
        assert!(error.to_string().contains("slot_1_target_equipment_family"));
        assert!(error.to_string().contains("必须为空"));
    }
}

#[test]
fn unowned_ship_identity_checks_follow_generated_fields() {
    let (layout, mut columns, mut range) = loadout_range(&["9001"]);
    range.set_value((1, columns["instance_id"]), Data::Empty);
    range.set_value((1, columns["source_type"]), Data::String("unowned".into()));
    range.set_value(
        (1, columns["source_ref"]),
        Data::String("unowned:10117".into()),
    );
    range.set_value((1, columns["group_id"]), Data::String("10117".into()));
    assert!(
        super::parse_loadout_rows(&range, &columns, &layout)
            .unwrap()
            .desired
            .is_empty()
    );
    range.set_value((1, columns["group_id"]), Data::String("20202".into()));
    assert!(super::parse_loadout_rows(&range, &columns, &layout).is_err());
    columns.remove("group_id");
    assert!(
        super::parse_loadout_rows(&range, &columns, &layout)
            .unwrap()
            .desired
            .is_empty()
    );
    for reference in ["owned:10117", "unowned:0", "unowned:invalid"] {
        range.set_value((1, columns["source_ref"]), Data::String(reference.into()));
        assert!(super::parse_loadout_rows(&range, &columns, &layout).is_err());
    }
}

#[test]
fn equipment_choice_binds_the_selected_source() {
    for (token, policy, source) in [
        (
            "维修设施 T1 +0｜仓库：3件〔1000|warehouse:1000〕",
            SourcePolicy::ExactSource,
            Some("仓库配置 1000"),
        ),
        (
            "维修设施 T1 +0｜测试船（实例9002）槽位3：1件〔1000|ship:9002:3〕",
            SourcePolicy::ExactSource,
            Some("舰船槽位 9002:3"),
        ),
        (
            "维修设施 T1 +0｜图纸合成：可合成2件〔1000|compose〕",
            SourcePolicy::ComposeOnly,
            None,
        ),
    ] {
        let fixture = generated_workbook("source-choice");
        let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
        edit_loadout_cell(
            &fixture.path,
            &fixture.layout,
            row,
            "slot_1_target_equipment_family",
            token,
        );
        let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired;
        let SlotTarget::Equipment(equipment) = desired.slots()[0].target() else {
            panic!("应为换装")
        };
        assert_eq!(equipment.family_id().get(), 1000);
        assert_eq!(equipment.source_policy(), policy);
        assert_eq!(
            equipment
                .exact_source()
                .map(|source| source.to_string())
                .as_deref(),
            source
        );
        assert_eq!(equipment.target_enhance_level(), None);
    }
}

#[test]
fn equipment_choice_rejects_invalid_source_tokens() {
    for token in [
        "〔1000|ship:9002:6〕",
        "〔1000|warehouse:0〕",
        "〔1000|unknown〕",
        "〔1000|compose|ship:1:1〕",
        "〔1000|ship:9002:3〕extra",
    ] {
        let fixture = generated_workbook("invalid-source-choice");
        let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
        edit_loadout_cell(
            &fixture.path,
            &fixture.layout,
            row,
            "slot_1_target_equipment_family",
            token,
        );
        assert!(
            load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).is_err(),
            "{token}"
        );
    }
}

#[test]
fn default_equipment_choice_compiles_a_ship_slot_transfer() {
    let state = golden_game_state();
    let original = &state.ships().ships()[0];
    let donor = crate::domain::ShipProfile::new(
        crate::domain::ShipIdentity::new(
            crate::domain::ShipInstanceId::new(9002).unwrap(),
            original.identity().config_id(),
            "来源船".to_owned(),
            original.identity().create_time(),
        ),
        original.growth(),
        original.intimacy().clone(),
        Vec::new(),
        original.classification().clone(),
        original.performance().clone(),
        original.skills().to_vec(),
        original.slots().clone(),
    );
    let roster = crate::domain::ShipRoster::new_with_skill_effects(
        state.ships().source().clone(),
        vec![original.clone(), donor],
        state.ships().skill_effects().clone(),
    );
    let state = crate::domain::GameState::new(
        state.source().clone(),
        roster,
        state.ship_catalog().clone(),
        state.equipment_catalog().clone(),
        state.equipment_details().clone(),
        state.equipment_inventory().clone(),
        state.bag().clone(),
        state.resources(),
        state.raw_records().clone(),
    );
    let mut fixture = generated_workbook("direct-source-transfer");
    use_default_layout(&mut fixture, &state);
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_2_target_equipment_family",
        "测试主炮 T1 +0｜来源船（实例9002）槽位1：1件〔1000|ship:9002:1〕",
    );
    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;
    let report = crate::application::compile_plan(&state, &desired).unwrap();
    assert!(report.plan().steps().iter().any(|step| matches!(step,
        crate::application::PlanStep::Unequip { slot, .. } if slot.ship_instance_id() == 9002 && slot.slot_index() == 1)));
    assert!(report.plan().steps().iter().any(|step| matches!(step,
        crate::application::PlanStep::Equip { slot, .. } if slot.ship_instance_id() == 9001 && slot.slot_index() == 2)));
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "测试主炮〔1000|ship:9002:1〕",
    );
    assert!(matches!(
        load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout),
        Err(super::WorkbookPlanError::Model {
            source: crate::domain::LoadoutModelError::DuplicateShipSource { .. }
        })
    ));
}

#[test]
fn equipment_choice_rejects_conflicting_source_fields() {
    let fixture = generated_workbook("conflicting-source-choice");
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "测试主炮〔1000|warehouse:1000〕",
    );
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_source_policy",
        &enum_label(&fixture.layout, "source_policy", "compose_only"),
    );
    assert!(load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout).is_err());
}

#[test]
fn default_equipment_choice_preserves_its_own_slot() {
    let state = golden_game_state();
    let mut fixture = generated_workbook("source-choice-current");
    use_default_layout(&mut fixture, &state);
    let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
    edit_loadout_cell(
        &fixture.path,
        &fixture.layout,
        row,
        "slot_1_target_equipment_family",
        "测试主炮〔1000|ship:9001:1〕",
    );
    let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
        .unwrap()
        .desired;
    assert!(matches!(desired.slots()[0].target(), SlotTarget::Keep));
    assert!(
        crate::application::compile_plan(&state, &desired)
            .unwrap()
            .plan()
            .steps()
            .iter()
            .all(|step| matches!(step, crate::application::PlanStep::Keep { .. }))
    );
}

#[test]
fn short_equipment_choices_resolve_from_the_workbook_dictionary() {
    let state = golden_game_state();
    let projection = crate::application::project_game_state_to_workbook(&state).unwrap();
    for row_data in projection.sheet("dictionaries").unwrap().rows() {
        let (
            Some(crate::application::WorkbookProjectionValue::Text(stable)),
            Some(crate::application::WorkbookProjectionValue::Text(label)),
        ) = (
            row_data.value("stable_value"),
            row_data.value("display_label"),
        )
        else {
            continue;
        };
        if !stable.contains('|') {
            continue;
        }
        let mut fixture = generated_workbook("short-equipment-choice");
        use_default_layout(&mut fixture, &state);
        let row = find_loadout_row(&fixture.path, &fixture.layout, "9001");
        edit_loadout_cell(
            &fixture.path,
            &fixture.layout,
            row,
            "slot_2_target_equipment_family",
            label,
        );
        let desired = load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
            .unwrap()
            .desired;
        let SlotTarget::Equipment(equipment) = desired.slots()[0].target() else {
            panic!("应为换装")
        };
        let actual = match equipment.exact_source() {
            Some(EquipmentSourceRef::Warehouse(id)) => format!("warehouse:{}", id.get()),
            Some(EquipmentSourceRef::ShipSlot(slot)) => format!(
                "ship:{}:{}",
                slot.ship_instance_id().get(),
                slot.slot_index().get()
            ),
            None => {
                assert_eq!(equipment.source_policy(), SourcePolicy::ComposeOnly);
                "compose".to_owned()
            }
        };
        assert_eq!(format!("{}|{actual}", equipment.family_id().get()), *stable);
        assert!(
            load_workbook_plan_from_xlsx(&fixture.path, &fixture.layout)
                .unwrap()
                .inventory
                .is_empty()
        );
    }
}

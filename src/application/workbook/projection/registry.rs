//! 集中定义数据工作簿的产品工作表、字段、枚举和样式注册表。

use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::super::super::layout::FieldReadDependency;
use super::super::super::{
    ExecutionFinalVerificationStatus, ExecutionReportStatus, ExecutionStatus, ExecutionStopReason,
    ExecutionWriteEffect, LayoutEditor, LayoutModelError, LayoutValueFormat,
    RegisteredLayoutEnumOption, RegisteredLayoutField, RegisteredLayoutSheet,
    WorkbookLayoutRegistry,
};
use super::WorkbookProjectionV4;

mod equipment;
mod plan;
mod results;
mod support;

use equipment::{add_equipment_inventory_fields, add_resource_recipe_fields};
use plan::{add_loadout_plan_fields, add_plan_data_fields};
use results::{add_check_result_fields, add_execution_result_fields};
use support::{add_dictionary_fields, add_raw_data_fields, add_schema_fields};
const MODEL_ROOT: &str = "WorkbookProjectionV4";

impl WorkbookProjectionV4 {
    /// 返回按默认数据工作簿顺序排列的全部工作表注册项。
    pub fn registered_sheets() -> Vec<RegisteredLayoutSheet> {
        [
            ("loadout_plan", true),
            ("equipment_inventory", true),
            ("check_results", true),
            ("execution_results", true),
            ("dictionaries", true),
            ("resource_recipes", true),
            ("raw_data", false),
            ("plan_data", true),
            ("schema", true),
            ("ship_technology", false),
        ]
        .into_iter()
        .map(|(stable_key, required)| RegisteredLayoutSheet::new(stable_key, required))
        .collect()
    }

    /// 返回按默认列顺序排列、且不含通配字段的完整投影注册项。
    pub fn registered_fields() -> Vec<RegisteredLayoutField> {
        let mut fields: Vec<RegisteredLayoutField> = Vec::new();
        add_loadout_plan_fields(&mut fields);
        add_equipment_inventory_fields(&mut fields);
        add_check_result_fields(&mut fields);
        add_execution_result_fields(&mut fields);
        add_dictionary_fields(&mut fields);
        add_resource_recipe_fields(&mut fields);
        add_raw_data_fields(&mut fields);
        add_plan_data_fields(&mut fields);
        add_schema_fields(&mut fields);
        for key in crate::application::TECHNOLOGY_VIEW_FIELDS {
            let source = fields
                .iter()
                .find(|f| f.sheet_key() == "loadout_plan" && f.stable_key() == key)
                .unwrap();
            fields.push(
                RegisteredLayoutField::new(
                    "ship_technology",
                    key,
                    format!("{MODEL_ROOT}.ship_technology[].{key}"),
                    source.allowed_formats().iter().copied(),
                    LayoutEditor::ReadOnly,
                    false,
                    source.enum_category().map(str::to_owned),
                )
                .with_read_dependency(FieldReadDependency::ShipTechnology),
            );
        }
        tag_read_dependency(
            &mut fields,
            "loadout_plan",
            &[
                "static_summary",
                "static_raw_ref",
                "skills_current_effect",
                "skills_effect_parameters",
                "skills_raw_structure",
                "skills_data_complete",
                "skills_read_errors",
            ],
            FieldReadDependency::ShipSkillEffects,
        );
        tag_read_dependency(
            &mut fields,
            "loadout_plan",
            &[
                "technology_bonus",
                "technology_get",
                "technology_upgrade",
                "technology_level",
            ],
            FieldReadDependency::ShipTechnology,
        );
        tag_read_dependency(
            &mut fields,
            "equipment_inventory",
            &["weapons_json"],
            FieldReadDependency::EquipmentWeapons,
        );
        tag_read_dependency(
            &mut fields,
            "equipment_inventory",
            &["skill_effects_json", "effect_summary"],
            FieldReadDependency::EquipmentSkillEffects,
        );
        fields
    }

    /// 返回布局控制项和业务输入共同使用的稳定枚举值。
    pub fn registered_enum_options() -> Vec<RegisteredLayoutEnumOption> {
        [
            ("generation_mode", "visible"),
            ("generation_mode", "hidden"),
            ("generation_mode", "omitted"),
            ("value_format", "text"),
            ("value_format", "integer"),
            ("value_format", "decimal"),
            ("value_format", "percentage"),
            ("value_format", "date_time"),
            ("value_format", "json"),
            ("inventory_operation", "keep"),
            ("inventory_operation", "enhance"),
            ("inventory_operation", "dismantle"),
            (
                "source_policy",
                "current_then_warehouse_then_compose_then_ship",
            ),
            ("source_policy", "warehouse_then_compose"),
            ("source_policy", "warehouse_then_compose_then_ship"),
            ("source_policy", "warehouse_then_ship_then_compose"),
            ("source_policy", "compose_then_warehouse_then_ship"),
            ("source_policy", "warehouse_only"),
            ("source_policy", "compose_only"),
            ("source_policy", "ship_only"),
            ("source_policy", "exact_source"),
            ("equipment_source_type", "warehouse"),
            ("equipment_source_type", "ship"),
            ("equipment_source_type", "unowned"),
            ("check_status", "passed"),
            ("check_status", "failed"),
            ("issue_severity", "error"),
            ("issue_severity", "warning"),
            ("execution_status", ExecutionStatus::Success.as_str()),
            ("execution_status", ExecutionStatus::Failed.as_str()),
            ("execution_status", ExecutionStatus::Unknown.as_str()),
            ("execution_status", ExecutionStatus::NotExecuted.as_str()),
            (
                "execution_report_status",
                ExecutionReportStatus::Success.as_str(),
            ),
            (
                "execution_report_status",
                ExecutionReportStatus::Failed.as_str(),
            ),
            (
                "execution_report_status",
                ExecutionReportStatus::Unknown.as_str(),
            ),
            (
                "execution_report_status",
                ExecutionReportStatus::Cancelled.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::Completed.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::Cancelled.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::CommandFailed.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::CommandUnknown.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::ReadbackFailed.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::ReadbackMismatch.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::FinalStateMismatch.as_str(),
            ),
            (
                "execution_stop_reason",
                ExecutionStopReason::FinalReadbackFailed.as_str(),
            ),
            (
                "execution_write_effect",
                ExecutionWriteEffect::None.as_str(),
            ),
            (
                "execution_write_effect",
                ExecutionWriteEffect::Possible.as_str(),
            ),
            (
                "execution_write_effect",
                ExecutionWriteEffect::StateChangedMismatch.as_str(),
            ),
            (
                "execution_write_effect",
                ExecutionWriteEffect::ExpectedPostStateObserved.as_str(),
            ),
            (
                "execution_write_effect",
                ExecutionWriteEffect::Verified.as_str(),
            ),
            (
                "execution_final_verification_status",
                ExecutionFinalVerificationStatus::Verified.as_str(),
            ),
            (
                "execution_final_verification_status",
                ExecutionFinalVerificationStatus::Mismatch.as_str(),
            ),
            (
                "execution_final_verification_status",
                ExecutionFinalVerificationStatus::Incomplete.as_str(),
            ),
            (
                "execution_final_verification_status",
                ExecutionFinalVerificationStatus::Unavailable.as_str(),
            ),
            (
                "execution_final_verification_status",
                ExecutionFinalVerificationStatus::UnconfirmedStepReached.as_str(),
            ),
            (
                "execution_final_verification_status",
                ExecutionFinalVerificationStatus::UnconfirmedStepNotObserved.as_str(),
            ),
        ]
        .into_iter()
        .map(|(category, value)| RegisteredLayoutEnumOption::new(category, value))
        .collect()
    }

    /// 返回程序按稳定键引用的基础样式。
    pub fn registered_style_keys() -> Vec<String> {
        [
            "read_only",
            "input",
            "unowned",
            "warning",
            "error",
            "current_state",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// 返回固定字段到读取范围的查询表。注册表内容不变，只在首次使用时建立。
    pub(crate) fn field_read_dependencies()
    -> &'static BTreeMap<(String, String), FieldReadDependency> {
        static DEPENDENCIES: OnceLock<BTreeMap<(String, String), FieldReadDependency>> =
            OnceLock::new();
        DEPENDENCIES.get_or_init(|| {
            Self::registered_fields()
                .into_iter()
                .filter_map(|field| {
                    let dependency = field.read_dependency()?;
                    Some((
                        (field.sheet_key().to_owned(), field.stable_key().to_owned()),
                        dependency,
                    ))
                })
                .collect()
        })
    }

    /// 建立严格核对当前投影版本的生产布局注册表。
    pub fn layout_registry() -> Result<WorkbookLayoutRegistry, LayoutModelError> {
        WorkbookLayoutRegistry::new(
            Self::registered_sheets(),
            Self::registered_fields(),
            Self::registered_enum_options(),
            Self::registered_style_keys(),
        )
    }
}

fn tag_read_dependency(
    fields: &mut [RegisteredLayoutField],
    sheet_key: &str,
    field_keys: &[&str],
    dependency: FieldReadDependency,
) {
    for field in fields {
        if field.sheet_key() == sheet_key && field_keys.contains(&field.stable_key()) {
            *field = field.clone().with_read_dependency(dependency);
        }
    }
}

fn push_read_only(
    fields: &mut Vec<RegisteredLayoutField>,
    sheet: &str,
    key: &str,
    value_format: LayoutValueFormat,
    required: bool,
) {
    push_field(
        fields,
        sheet,
        key,
        value_format,
        LayoutEditor::ReadOnly,
        required,
        None,
    );
}

#[allow(clippy::too_many_arguments)]
fn push_field(
    fields: &mut Vec<RegisteredLayoutField>,
    sheet: &str,
    key: &str,
    value_format: LayoutValueFormat,
    editor: LayoutEditor,
    required: bool,
    enum_category: Option<&str>,
) {
    fields.push(RegisteredLayoutField::new(
        sheet,
        key,
        format!("{MODEL_ROOT}.{sheet}[].{key}"),
        if sheet == "equipment_inventory"
            && matches!(
                key,
                "current_enhance_level"
                    | "family_warehouse_quantity"
                    | "equipment_limit"
                    | "attributes_json"
                    | "weapons_json"
                    | "compose_material_costs"
                    | "next_cost_json"
                    | "dismantle_yield_json"
            )
        {
            vec![value_format, LayoutValueFormat::Text]
        } else {
            vec![value_format]
        },
        editor,
        required,
        enum_category.map(str::to_owned),
    ));
}

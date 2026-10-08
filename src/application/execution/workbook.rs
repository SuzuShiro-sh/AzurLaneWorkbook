//! 将执行报告映射为与具体 XLSX 实现无关的执行结果工作表行。

use serde::Serialize;

use super::super::{
    ExecutionReport, ExecutionSlotState, PlanSlot, PlanSource, WorkbookProjectionError,
    WorkbookProjectionRow, WorkbookProjectionValue,
};

const EXECUTION_RESULTS_SHEET: &str = "execution_results";

type ProjectionValue = WorkbookProjectionValue;
type ProjectionValues = Vec<(String, ProjectionValue)>;

macro_rules! projection_values {
    ($($key:literal => $value:expr),* $(,)?) => {
        vec![$(($key.to_owned(), $value)),*]
    };
}

/// 将同一执行报告的每个步骤映射为完整、已注册且可稳定排序的一行。
///
/// 投影行身份由报告摘要和补零步骤序号组成，避免不同执行报告之间发生碰撞；表内
/// `object_ref` 指向步骤实际作用的舰船槽位；无目标槽位时指向该步骤的装备来源。
pub fn project_execution_report_rows(
    report: &ExecutionReport,
    executed_at_unix_millis: i64,
) -> Result<Vec<WorkbookProjectionRow>, WorkbookProjectionError> {
    let mut rows = Vec::with_capacity(report.steps().len());
    for step in report.steps() {
        let row_ref = format!(
            "execution:{}:step:{:010}",
            report.content_sha256(),
            step.sequence()
        );
        let object_ref = step_object_ref(step.step());
        let (verified_slot_state, verified_enhance_level, verified_equipment_source) =
            verified_equipment_values(step.readback_evidence());
        let (
            gold_before,
            gold_after,
            gold_delta,
            composed_config_delta,
            dismantled_source_quantity,
            materials_before,
            materials_after,
            materials_delta,
        ) = verified_resource_values(step.readback_evidence(), &row_ref)?;
        let values: ProjectionValues = projection_values![
            "executed_at" => ProjectionValue::date_time_unix_millis(executed_at_unix_millis),
            "step_sequence" => ProjectionValue::Integer(i64::from(step.sequence())),
            "step_type" => ProjectionValue::text(step.step_kind()),
            "object_ref" => ProjectionValue::text(object_ref),
            "request_summary" => optional_text(step.request_summary()),
            "response_summary" => optional_text(step.response_summary()),
            "readback_summary" => optional_text(step.readback_summary()),
            "error_code" => optional_text(step.error_code()),
            "message" => ProjectionValue::text(step.message()),
            "verified_slot_state" => verified_slot_state,
            "verified_equipment_source" => verified_equipment_source,
            "final_verification_status" => ProjectionValue::enumeration(
                "execution_final_verification_status",
                report.final_verification_status().as_str(),
            ),
            "plan_hash" => ProjectionValue::text(report.plan_hash()),
            "status" => ProjectionValue::enumeration("execution_status", step.status().as_str()),
            "verified_enhance_level" => verified_enhance_level,
            "gold_before" => gold_before,
            "gold_after" => gold_after,
            "gold_delta" => gold_delta,
            "composed_config_delta" => composed_config_delta,
            "dismantled_source_quantity" => dismantled_source_quantity,
            "materials_before" => materials_before,
            "materials_after" => materials_after,
            "materials_delta" => materials_delta,
            "report_status" => ProjectionValue::enumeration(
                "execution_report_status",
                report.status().as_str(),
            ),
            "stop_reason" => ProjectionValue::enumeration(
                "execution_stop_reason",
                report.stop_reason().as_str(),
            ),
            "target_fingerprint_sha256" => ProjectionValue::text(
                report.target_identity().fingerprint_sha256(),
            ),
            "report_hash" => ProjectionValue::text(report.content_sha256()),
            "may_have_writes" => ProjectionValue::Boolean(report.may_have_writes()),
            "write_acknowledged" => ProjectionValue::Boolean(step.write_acknowledged()),
            "write_effect" => ProjectionValue::enumeration(
                "execution_write_effect",
                step.write_effect().as_str(),
            ),
            "acknowledged_write_count" => count_value(
                &row_ref,
                report.acknowledged_write_count(),
                "转换已确认写入数",
            )?,
            "observed_state_change_count" => count_value(
                &row_ref,
                report.observed_state_change_count(),
                "转换已观察状态变化数",
            )?,
            "verified_write_count" => count_value(
                &row_ref,
                report.verified_write_count(),
                "转换已验证写入数",
            )?,
        ];
        rows.push((row_ref, values));
    }
    WorkbookProjectionRow::validated_for_sheet(EXECUTION_RESULTS_SHEET, rows)
}

fn verified_equipment_values(
    evidence: Option<&super::super::ExecutionReadbackEvidence>,
) -> (ProjectionValue, ProjectionValue, ProjectionValue) {
    let Some(evidence) = evidence else {
        return (
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
        );
    };
    if !evidence.matches_expected() {
        return (
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
        );
    }
    let source = evidence
        .source()
        .map(source_ref)
        .map(ProjectionValue::text)
        .unwrap_or(ProjectionValue::Blank);
    let Some(target_slot) = evidence.target_slot() else {
        return (ProjectionValue::Blank, ProjectionValue::Blank, source);
    };
    // 只有完整回读一致时，计划来源和实际槽位状态才共同构成已验证证据。
    match target_slot.actual_after() {
        ExecutionSlotState::Missing => (
            ProjectionValue::text("missing"),
            ProjectionValue::Blank,
            source,
        ),
        ExecutionSlotState::Empty => (
            ProjectionValue::text("empty"),
            ProjectionValue::Blank,
            source,
        ),
        ExecutionSlotState::Equipped(equipment) => (
            ProjectionValue::text(format!("equipped:{}", equipment.config_id())),
            ProjectionValue::Integer(i64::from(equipment.enhance_level())),
            source,
        ),
    }
}

#[derive(Serialize)]
struct MaterialQuantityProjection {
    item_id: u64,
    quantity: u64,
}

#[derive(Serialize)]
struct MaterialDeltaProjection {
    item_id: u64,
    quantity: i64,
}

type VerifiedResourceValues = (
    ProjectionValue,
    ProjectionValue,
    ProjectionValue,
    ProjectionValue,
    ProjectionValue,
    ProjectionValue,
    ProjectionValue,
    ProjectionValue,
);

fn verified_resource_values(
    evidence: Option<&super::super::ExecutionReadbackEvidence>,
    row_ref: &str,
) -> Result<VerifiedResourceValues, WorkbookProjectionError> {
    let blank = || {
        (
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
            ProjectionValue::Blank,
        )
    };
    let Some(evidence) = evidence.filter(|evidence| evidence.matches_expected()) else {
        return Ok(blank());
    };
    let Some(gold) = evidence.gold() else {
        return Ok(blank());
    };
    let composed_output_quantity = evidence.composed_output_quantity();
    let dismantled_source_quantity = evidence.dismantled_source_quantity();
    let enhanced_target_quantity = evidence.enhanced_target_quantity();
    if composed_output_quantity.is_none()
        && dismantled_source_quantity.is_none()
        && enhanced_target_quantity.is_none()
    {
        return Ok(blank());
    }
    let gold_delta = signed_delta(gold.before(), gold.actual_after(), row_ref, "转换物资变化")?;
    let mut materials_before: Vec<MaterialQuantityProjection> = evidence
        .materials()
        .iter()
        .map(|material| MaterialQuantityProjection {
            item_id: material.item_id(),
            quantity: material.before_quantity(),
        })
        .collect();
    let mut materials_after: Vec<MaterialQuantityProjection> = evidence
        .materials()
        .iter()
        .map(|material| MaterialQuantityProjection {
            item_id: material.item_id(),
            quantity: material.actual_after_quantity(),
        })
        .collect();
    let mut materials_delta: Vec<MaterialDeltaProjection> = evidence
        .materials()
        .iter()
        .map(|material| {
            Ok(MaterialDeltaProjection {
                item_id: material.item_id(),
                quantity: signed_delta(
                    material.before_quantity(),
                    material.actual_after_quantity(),
                    row_ref,
                    "转换材料变化",
                )?,
            })
        })
        .collect::<Result<_, WorkbookProjectionError>>()?;
    materials_before.sort_unstable_by_key(|material| material.item_id);
    materials_after.sort_unstable_by_key(|material| material.item_id);
    materials_delta.sort_unstable_by_key(|material| material.item_id);
    Ok((
        unsigned_value(row_ref, gold.before(), "转换资源写入前物资")?,
        unsigned_value(row_ref, gold.actual_after(), "转换资源写入后物资")?,
        ProjectionValue::Integer(gold_delta),
        composed_output_quantity
            .map(|quantity| unsigned_value(row_ref, quantity, "转换合成产物增量"))
            .transpose()?
            .unwrap_or(ProjectionValue::Blank),
        dismantled_source_quantity
            .map(|quantity| unsigned_value(row_ref, quantity, "转换拆解来源数量"))
            .transpose()?
            .unwrap_or(ProjectionValue::Blank),
        ProjectionValue::Json(
            serde_json::to_string(&materials_before).expect("材料投影只包含可序列化整数"),
        ),
        ProjectionValue::Json(
            serde_json::to_string(&materials_after).expect("材料投影只包含可序列化整数"),
        ),
        ProjectionValue::Json(
            serde_json::to_string(&materials_delta).expect("材料投影只包含可序列化整数"),
        ),
    ))
}

fn optional_text(value: Option<&str>) -> ProjectionValue {
    value
        .map(ProjectionValue::text)
        .unwrap_or(ProjectionValue::Blank)
}

fn slot_ref(slot: PlanSlot) -> String {
    format!("ship:{}:{}", slot.ship_instance_id(), slot.slot_index())
}

fn step_object_ref(step: super::super::PlanStep) -> String {
    step.slot()
        .map(slot_ref)
        .or_else(|| step.source().map(source_ref))
        .expect("计划步骤必须具有目标槽位或装备来源")
}

fn source_ref(source: PlanSource) -> String {
    match source {
        PlanSource::Warehouse { config_id } => format!("warehouse:{config_id}"),
        PlanSource::ShipSlot {
            ship_instance_id,
            slot_index,
        } => format!("ship:{ship_instance_id}:{slot_index}"),
        PlanSource::Compose { recipe_id } => format!("compose:{recipe_id}"),
    }
}

fn signed_delta(
    before: u64,
    after: u64,
    object_ref: &str,
    operation: &'static str,
) -> Result<i64, WorkbookProjectionError> {
    let delta = i128::from(after) - i128::from(before);
    i64::try_from(delta).map_err(|_| WorkbookProjectionError::ArithmeticOverflow {
        sheet_key: EXECUTION_RESULTS_SHEET,
        object_ref: object_ref.to_owned(),
        operation,
    })
}

fn count_value(
    object_ref: &str,
    value: usize,
    operation: &'static str,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    i64::try_from(value)
        .map(ProjectionValue::Integer)
        .map_err(|_| WorkbookProjectionError::ArithmeticOverflow {
            sheet_key: EXECUTION_RESULTS_SHEET,
            object_ref: object_ref.to_owned(),
            operation,
        })
}

fn unsigned_value(
    object_ref: &str,
    value: u64,
    operation: &'static str,
) -> Result<ProjectionValue, WorkbookProjectionError> {
    i64::try_from(value)
        .map(ProjectionValue::Integer)
        .map_err(|_| WorkbookProjectionError::ArithmeticOverflow {
            sheet_key: EXECUTION_RESULTS_SHEET,
            object_ref: object_ref.to_owned(),
            operation,
        })
}

#[cfg(test)]
mod tests {
    use super::super::{
        execution_workbook_compose_report_fixture, execution_workbook_dismantle_report_fixture,
        execution_workbook_enhance_report_fixture, execution_workbook_mismatch_report_fixture,
        execution_workbook_report_fixture,
    };
    use super::project_execution_report_rows;
    use crate::application::test_support::empty_execution_report;
    use crate::application::{WorkbookProjectionError, WorkbookProjectionValue};

    #[test]
    fn maps_all_registered_fields_without_fabricating_unobserved_resources() {
        let report = execution_workbook_report_fixture();

        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();

        assert_eq!(rows.len(), 2);
        let first = &rows[0];
        assert_eq!(
            first.object_ref(),
            format!("execution:{}:step:0000000001", report.content_sha256())
        );
        assert_eq!(first.values().len(), 33);
        assert_eq!(
            first.value("executed_at"),
            Some(&WorkbookProjectionValue::DateTimeUnixMillis(
                1_700_000_000_123
            ))
        );
        assert_eq!(
            first.value("step_sequence"),
            Some(&WorkbookProjectionValue::Integer(1))
        );
        assert_eq!(
            first.value("step_type"),
            Some(&WorkbookProjectionValue::text("equip"))
        );
        assert_eq!(
            first.value("object_ref"),
            Some(&WorkbookProjectionValue::text("ship:9001:2"))
        );
        assert_eq!(
            first.value("verified_slot_state"),
            Some(&WorkbookProjectionValue::text("equipped:1001"))
        );
        assert_eq!(
            first.value("verified_enhance_level"),
            Some(&WorkbookProjectionValue::Integer(1))
        );
        assert_eq!(
            first.value("verified_equipment_source"),
            Some(&WorkbookProjectionValue::text("warehouse:1001"))
        );
        assert_eq!(
            first.value("status"),
            Some(&WorkbookProjectionValue::enumeration(
                "execution_status",
                "success"
            ))
        );
        assert_eq!(
            first.value("report_status"),
            Some(&WorkbookProjectionValue::enumeration(
                "execution_report_status",
                "cancelled"
            ))
        );
        assert_eq!(
            first.value("final_verification_status"),
            Some(&WorkbookProjectionValue::enumeration(
                "execution_final_verification_status",
                "incomplete"
            ))
        );
        assert_eq!(
            first.value("write_effect"),
            Some(&WorkbookProjectionValue::enumeration(
                "execution_write_effect",
                "verified"
            ))
        );
        assert_eq!(
            first.value("may_have_writes"),
            Some(&WorkbookProjectionValue::Boolean(true))
        );
        assert_eq!(
            first.value("write_acknowledged"),
            Some(&WorkbookProjectionValue::Boolean(true))
        );
        for field in [
            "gold_before",
            "gold_after",
            "gold_delta",
            "composed_config_delta",
            "dismantled_source_quantity",
            "materials_before",
            "materials_after",
            "materials_delta",
        ] {
            assert_eq!(first.value(field), Some(&WorkbookProjectionValue::Blank));
        }

        let second = &rows[1];
        assert_eq!(
            second.object_ref(),
            format!("execution:{}:step:0000000002", report.content_sha256())
        );
        for field in [
            "request_summary",
            "response_summary",
            "verified_slot_state",
            "verified_enhance_level",
            "verified_equipment_source",
        ] {
            assert_eq!(second.value(field), Some(&WorkbookProjectionValue::Blank));
        }
    }

    #[test]
    fn maps_only_verified_dismantle_resources_and_source_quantity() {
        let report = execution_workbook_dismantle_report_fixture();

        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.value("object_ref"),
            Some(&WorkbookProjectionValue::text("warehouse:1000"))
        );
        assert_eq!(
            row.value("gold_before"),
            Some(&WorkbookProjectionValue::Integer(100))
        );
        assert_eq!(
            row.value("gold_after"),
            Some(&WorkbookProjectionValue::Integer(110))
        );
        assert_eq!(
            row.value("gold_delta"),
            Some(&WorkbookProjectionValue::Integer(10))
        );
        assert_eq!(
            row.value("dismantled_source_quantity"),
            Some(&WorkbookProjectionValue::Integer(1))
        );
        assert_eq!(
            row.value("materials_before"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":2001,\"quantity\":3}]".to_owned()
            ))
        );
        assert_eq!(
            row.value("materials_after"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":2001,\"quantity\":5}]".to_owned()
            ))
        );
        assert_eq!(
            row.value("materials_delta"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":2001,\"quantity\":2}]".to_owned()
            ))
        );
    }

    #[test]
    fn maps_verified_compose_resources_with_signed_costs() {
        let report = execution_workbook_compose_report_fixture();

        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.value("object_ref"),
            Some(&WorkbookProjectionValue::text("compose:5001"))
        );
        assert_eq!(
            row.value("verified_equipment_source"),
            Some(&WorkbookProjectionValue::text("compose:5001"))
        );
        assert_eq!(
            row.value("gold_before"),
            Some(&WorkbookProjectionValue::Integer(1_000))
        );
        assert_eq!(
            row.value("gold_after"),
            Some(&WorkbookProjectionValue::Integer(900))
        );
        assert_eq!(
            row.value("gold_delta"),
            Some(&WorkbookProjectionValue::Integer(-100))
        );
        assert_eq!(
            row.value("composed_config_delta"),
            Some(&WorkbookProjectionValue::Integer(1))
        );
        assert_eq!(
            row.value("dismantled_source_quantity"),
            Some(&WorkbookProjectionValue::Blank)
        );
        assert_eq!(
            row.value("materials_before"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":2001,\"quantity\":20}]".to_owned()
            ))
        );
        assert_eq!(
            row.value("materials_after"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":2001,\"quantity\":15}]".to_owned()
            ))
        );
        assert_eq!(
            row.value("materials_delta"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":2001,\"quantity\":-5}]".to_owned()
            ))
        );
    }

    #[test]
    fn maps_verified_enhance_resources_with_signed_costs() {
        let report = execution_workbook_enhance_report_fixture();

        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(
            row.value("step_type"),
            Some(&WorkbookProjectionValue::text("enhance"))
        );
        assert_eq!(
            row.value("object_ref"),
            Some(&WorkbookProjectionValue::text("warehouse:1000"))
        );
        assert_eq!(
            row.value("verified_equipment_source"),
            Some(&WorkbookProjectionValue::text("warehouse:1000"))
        );
        assert_eq!(
            row.value("verified_slot_state"),
            Some(&WorkbookProjectionValue::Blank)
        );
        assert_eq!(
            row.value("verified_enhance_level"),
            Some(&WorkbookProjectionValue::Blank)
        );
        assert_eq!(
            row.value("gold_before"),
            Some(&WorkbookProjectionValue::Integer(100))
        );
        assert_eq!(
            row.value("gold_after"),
            Some(&WorkbookProjectionValue::Integer(90))
        );
        assert_eq!(
            row.value("gold_delta"),
            Some(&WorkbookProjectionValue::Integer(-10))
        );
        assert_eq!(
            row.value("composed_config_delta"),
            Some(&WorkbookProjectionValue::Blank)
        );
        assert_eq!(
            row.value("dismantled_source_quantity"),
            Some(&WorkbookProjectionValue::Blank)
        );
        assert_eq!(
            row.value("materials_before"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":3001,\"quantity\":10}]".to_owned()
            ))
        );
        assert_eq!(
            row.value("materials_after"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":3001,\"quantity\":8}]".to_owned()
            ))
        );
        assert_eq!(
            row.value("materials_delta"),
            Some(&WorkbookProjectionValue::Json(
                "[{\"item_id\":3001,\"quantity\":-2}]".to_owned()
            ))
        );
    }

    #[test]
    fn empty_execution_report_produces_no_synthetic_result_row() {
        let report = empty_execution_report();

        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();

        assert!(rows.is_empty());
    }

    #[test]
    fn rejects_a_nonempty_report_timestamp_outside_excel_range() {
        let report = execution_workbook_report_fixture();

        let error = project_execution_report_rows(&report, i64::MAX).unwrap_err();

        assert!(matches!(
            error,
            WorkbookProjectionError::DateTimeOutOfRange {
                sheet_key,
                field_key,
                ..
            } if sheet_key == "execution_results" && field_key == "executed_at"
        ));
    }

    #[test]
    fn does_not_label_planned_source_or_mismatched_state_as_verified() {
        let report = execution_workbook_mismatch_report_fixture();

        let rows = project_execution_report_rows(&report, 1_700_000_000_123).unwrap();
        let first = &rows[0];

        for field in [
            "verified_slot_state",
            "verified_enhance_level",
            "verified_equipment_source",
        ] {
            assert_eq!(first.value(field), Some(&WorkbookProjectionValue::Blank));
        }
        assert_eq!(
            first.value("write_effect"),
            Some(&WorkbookProjectionValue::enumeration(
                "execution_write_effect",
                "state_changed_mismatch"
            ))
        );
    }
}

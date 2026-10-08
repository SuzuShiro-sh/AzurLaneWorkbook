//! 将一次计划检查结论映射为只读结果行。

use std::collections::BTreeMap;
use std::error::Error;

use crate::application::{
    AppError, CheckReport, WorkbookProjectionError, WorkbookProjectionRow, WorkbookProjectionV4,
    WorkbookProjectionValue as Value,
};

pub(crate) fn project_check_result_rows(
    result: Result<&CheckReport, &AppError>,
    checked_at: i64,
) -> Result<Vec<WorkbookProjectionRow>, WorkbookProjectionError> {
    let mut values: BTreeMap<String, Value> = WorkbookProjectionV4::registered_fields()
        .into_iter()
        .filter(|field| field.sheet_key() == "check_results")
        .map(|field| (field.stable_key().to_owned(), Value::Blank))
        .collect();
    values.insert(
        "checked_at".into(),
        Value::date_time_unix_millis(checked_at),
    );
    values.insert("issue_sequence".into(), Value::Integer(1));
    match result {
        Ok(report) => {
            for (key, value) in [
                ("status", "通过".to_owned()),
                ("code", "OK".to_owned()),
                ("message", report.message().to_owned()),
                ("plan_hash", report.plan().content_sha256().to_owned()),
                (
                    "current_value",
                    format!(
                        "已检查 {} 个槽位、{} 项库存操作",
                        report.checked_slots(),
                        report.checked_inventory_actions()
                    ),
                ),
            ] {
                values.insert(key.into(), Value::text(value));
            }
        }
        Err(error) => {
            let context = error.context();
            let mut details = vec![error.message().to_owned()];
            let mut source = error.source();
            while let Some(cause) = source {
                let text = cause.to_string();
                if details.last() != Some(&text) {
                    details.push(text);
                }
                source = cause.source();
            }
            let location = [
                "sheet",
                "row",
                "field",
                "slot",
                "source",
                "source_slot",
                "ship_instance_id",
                "family_id",
                "config_id",
                "recipe_id",
                "resource",
            ]
            .into_iter()
            .filter_map(|key| context.get(key).map(|value| format!("{key}={value}")))
            .collect::<Vec<_>>()
            .join("；");
            for (key, value) in [
                ("status", "未通过".to_owned()),
                ("severity", "error".to_owned()),
                ("code", error.code().as_str().to_owned()),
                ("message", details.join("：")),
                (
                    "object_ref",
                    if location.is_empty() {
                        error.stage().to_owned()
                    } else {
                        location
                    },
                ),
            ] {
                values.insert(key.into(), Value::text(value));
            }
            for (field, keys) in [
                (
                    "current_value",
                    &["available", "actual_family_id", "source_level"][..],
                ),
                (
                    "expected_value",
                    &["required", "expected_family_id", "target_level"][..],
                ),
                ("sheet_key", &["sheet"][..]),
                ("field_key", &["field"][..]),
            ] {
                if let Some(value) = keys.iter().find_map(|key| context.get(*key)) {
                    values.insert(field.into(), Value::text(value));
                }
            }
            if let Some(row) = context
                .get("row")
                .and_then(|value| value.parse::<i64>().ok())
            {
                values.insert("logical_row".into(), Value::Integer(row));
            }
        }
    }
    WorkbookProjectionRow::validated_for_sheet(
        "check_results",
        vec![(
            format!("check:{checked_at}:0000000001"),
            values.into_iter().collect(),
        )],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::test_support::empty_game_state;
    use crate::application::{PlanCheckError, compile_plan, map_plan_check_error};
    use crate::domain::{DesiredState, EquipmentFamilyId};

    #[test]
    fn success_and_failure_rows_keep_meaningful_values() {
        let report =
            compile_plan(&empty_game_state(), &DesiredState::new(vec![]).unwrap()).unwrap();
        let rows = project_check_result_rows(Ok(&report), 1_700_000_000_123).unwrap();
        assert_eq!(rows[0].value("status"), Some(&Value::text("通过")));
        assert_eq!(
            rows[0].value("plan_hash"),
            Some(&Value::text(report.plan().content_sha256()))
        );
        let error = map_plan_check_error(PlanCheckError::ComposeRecipeNotFound {
            family_id: EquipmentFamilyId::new(16400).unwrap(),
        });
        let rows = project_check_result_rows(Err(&error), 1_700_000_000_123).unwrap();
        assert_eq!(rows[0].value("status"), Some(&Value::text("未通过")));
        assert_eq!(
            rows[0].value("object_ref"),
            Some(&Value::text("family_id=16400"))
        );
        assert!(
            matches!(rows[0].value("message"), Some(Value::Text(text)) if text.contains("没有可用的装备合成配方"))
        );
        assert_eq!(rows[0].value("plan_hash"), Some(&Value::Blank));
    }

    #[test]
    fn quantity_failure_keeps_available_and_required_separate() {
        let error = AppError::from_source(
            "plan.check",
            crate::application::AppErrorCode::EquipmentStateChanged,
            "装备数量不足",
            std::io::Error::other("需要9件，只有8件"),
        )
        .with_context("source", "warehouse:5240")
        .with_context("available", "8")
        .with_context("required", "9");
        let rows = project_check_result_rows(Err(&error), 1_700_000_000_123).unwrap();
        assert_eq!(rows[0].value("current_value"), Some(&Value::text("8")));
        assert_eq!(rows[0].value("expected_value"), Some(&Value::text("9")));
    }
}

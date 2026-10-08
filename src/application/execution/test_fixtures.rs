//! 构造执行结果工作簿映射所需的完整测试报告夹具。

use std::collections::BTreeMap;

use super::{
    EXECUTION_SCHEMA_VERSION, ExecutionEquipmentState, ExecutionFinalVerificationStatus,
    ExecutionGoldReadback, ExecutionMaterialReadback, ExecutionReadbackEvidence, ExecutionReport,
    ExecutionReportStatus, ExecutionSlotReadback, ExecutionSlotState, ExecutionStateMismatch,
    ExecutionStatus, ExecutionStepResult, ExecutionStopReason, ExecutionTargetIdentity,
    ExecutionWarehouseReadback, ExecutionWriteEffect,
};
use crate::application::{
    PLAN_SCHEMA_VERSION, PlanEnhanceCost, PlanEnhanceMaterialCost, PlanEquipment, PlanSlot,
    PlanSource, PlanStep,
};

#[cfg(test)]
pub(crate) fn execution_workbook_report_fixture() -> ExecutionReport {
    let target_slot = PlanSlot::from_raw(9001, 2);
    let equipped = ExecutionEquipmentState {
        config_id: 1001,
        enhance_level: 1,
    };
    let source = PlanSource::Warehouse { config_id: 1001 };
    let completed_step = ExecutionStepResult {
        sequence: 1,
        step_kind: "equip",
        step: PlanStep::Equip {
            sequence: 1,
            slot: target_slot,
            source,
            equipment: PlanEquipment::from_raw(1000, 1001, 1),
        },
        status: ExecutionStatus::Success,
        command_id: Some("d".repeat(64)),
        pre_state_content_sha256: Some("e".repeat(64)),
        post_state_content_sha256: Some("f".repeat(64)),
        request_summary: Some("将仓库装备放入目标槽位".to_owned()),
        response_summary: Some("运行态确认命令成功".to_owned()),
        readback_summary: Some("目标槽位与完整模拟一致".to_owned()),
        readback_evidence: Some(ExecutionReadbackEvidence {
            matches_expected: true,
            target_slot: Some(ExecutionSlotReadback {
                slot: target_slot,
                before: ExecutionSlotState::Empty,
                expected_after: ExecutionSlotState::Equipped(equipped),
                actual_after: ExecutionSlotState::Equipped(equipped),
            }),
            source: Some(source),
            source_slot: None,
            warehouse: vec![ExecutionWarehouseReadback {
                config_id: 1001,
                before_quantity: 2,
                expected_after_quantity: 1,
                actual_after_quantity: 1,
            }],
            gold: None,
            materials: Vec::new(),
            dismantled_source_quantity: None,
            composed_output_quantity: None,
            enhanced_target_quantity: None,
            first_mismatch: None,
        }),
        write_acknowledged: true,
        write_effect: ExecutionWriteEffect::Verified,
        error_code: None,
        message: "装备步骤执行并回读成功".to_owned(),
        diagnostics: BTreeMap::new(),
    };
    let skipped_step = ExecutionStepResult {
        sequence: 2,
        step_kind: "keep",
        step: PlanStep::Keep {
            sequence: 2,
            slot: PlanSlot::from_raw(9001, 3),
        },
        status: ExecutionStatus::NotExecuted,
        command_id: None,
        pre_state_content_sha256: None,
        post_state_content_sha256: None,
        request_summary: None,
        response_summary: None,
        readback_summary: Some("调用方已请求停止后续步骤".to_owned()),
        readback_evidence: None,
        write_acknowledged: false,
        write_effect: ExecutionWriteEffect::None,
        error_code: None,
        message: "步骤未执行".to_owned(),
        diagnostics: BTreeMap::new(),
    };
    ExecutionReport {
        schema_version: EXECUTION_SCHEMA_VERSION,
        plan_schema_version: PLAN_SCHEMA_VERSION,
        target_identity: ExecutionTargetIdentity::new("a".repeat(64)).expect("测试目标指纹应有效"),
        plan_hash: "b".repeat(64),
        initial_state_content_sha256: "1".repeat(64),
        final_state_content_sha256: Some("2".repeat(64)),
        status: ExecutionReportStatus::Cancelled,
        stop_reason: ExecutionStopReason::Cancelled,
        acknowledged_write_count: 1,
        observed_state_change_count: 1,
        verified_write_count: 1,
        may_have_writes: true,
        final_verification_status: ExecutionFinalVerificationStatus::Incomplete,
        final_verification_summary: Some("已核对停止前完成的步骤".to_owned()),
        steps: vec![completed_step, skipped_step],
        content_sha256: "c".repeat(64),
    }
}

/// 建立带完整拆解物资和材料回读的工作簿映射测试报告。
#[cfg(test)]
pub(crate) fn execution_workbook_dismantle_report_fixture() -> ExecutionReport {
    let mut report = execution_workbook_report_fixture();
    let source = PlanSource::Warehouse { config_id: 1000 };
    report.steps = vec![ExecutionStepResult {
        sequence: 1,
        step_kind: "dismantle",
        step: PlanStep::Dismantle {
            sequence: 1,
            source,
            equipment: PlanEquipment::from_raw(1000, 1000, 0),
            quantity: 1,
        },
        status: ExecutionStatus::Success,
        command_id: Some("d".repeat(64)),
        pre_state_content_sha256: Some("e".repeat(64)),
        post_state_content_sha256: Some("f".repeat(64)),
        request_summary: Some("拆解仓库装备".to_owned()),
        response_summary: Some("运行态确认命令成功".to_owned()),
        readback_summary: Some("装备、物资和材料均与完整模拟一致".to_owned()),
        readback_evidence: Some(ExecutionReadbackEvidence {
            matches_expected: true,
            target_slot: None,
            source: Some(source),
            source_slot: None,
            warehouse: vec![ExecutionWarehouseReadback {
                config_id: 1000,
                before_quantity: 1,
                expected_after_quantity: 0,
                actual_after_quantity: 0,
            }],
            gold: Some(ExecutionGoldReadback {
                before: 100,
                expected_after: 110,
                actual_after: 110,
            }),
            materials: vec![ExecutionMaterialReadback {
                item_id: 2001,
                before_quantity: 3,
                expected_after_quantity: 5,
                actual_after_quantity: 5,
            }],
            dismantled_source_quantity: Some(1),
            composed_output_quantity: None,
            enhanced_target_quantity: None,
            first_mismatch: None,
        }),
        write_acknowledged: true,
        write_effect: ExecutionWriteEffect::Verified,
        error_code: None,
        message: "装备拆解并回读成功".to_owned(),
        diagnostics: BTreeMap::new(),
    }];
    report.status = ExecutionReportStatus::Success;
    report.stop_reason = ExecutionStopReason::Completed;
    report.acknowledged_write_count = 1;
    report.observed_state_change_count = 1;
    report.verified_write_count = 1;
    report.final_verification_status = ExecutionFinalVerificationStatus::Verified;
    report.final_verification_summary = Some("终态与拆解预期一致".to_owned());
    report.content_sha256 = "e".repeat(64);
    report
}

/// 建立带完整合成产物、物资和材料回读的工作簿映射测试报告。
#[cfg(test)]
pub(crate) fn execution_workbook_compose_report_fixture() -> ExecutionReport {
    let mut report = execution_workbook_report_fixture();
    let source = PlanSource::Compose { recipe_id: 5001 };
    report.steps = vec![ExecutionStepResult {
        sequence: 1,
        step_kind: "compose",
        step: PlanStep::Compose {
            sequence: 1,
            recipe_id: 5001,
            equipment: PlanEquipment::from_raw(1000, 1000, 0),
            quantity: 1,
            material_id: 2001,
            material_quantity_per_unit: 5,
            gold_per_unit: 100,
        },
        status: ExecutionStatus::Success,
        command_id: Some("d".repeat(64)),
        pre_state_content_sha256: Some("e".repeat(64)),
        post_state_content_sha256: Some("f".repeat(64)),
        request_summary: Some("合成仓库装备".to_owned()),
        response_summary: Some("运行态确认命令成功".to_owned()),
        readback_summary: Some("装备、物资和材料均与完整模拟一致".to_owned()),
        readback_evidence: Some(ExecutionReadbackEvidence {
            matches_expected: true,
            target_slot: None,
            source: Some(source),
            source_slot: None,
            warehouse: vec![ExecutionWarehouseReadback {
                config_id: 1000,
                before_quantity: 1,
                expected_after_quantity: 2,
                actual_after_quantity: 2,
            }],
            gold: Some(ExecutionGoldReadback {
                before: 1_000,
                expected_after: 900,
                actual_after: 900,
            }),
            materials: vec![ExecutionMaterialReadback {
                item_id: 2001,
                before_quantity: 20,
                expected_after_quantity: 15,
                actual_after_quantity: 15,
            }],
            dismantled_source_quantity: None,
            composed_output_quantity: Some(1),
            enhanced_target_quantity: None,
            first_mismatch: None,
        }),
        write_acknowledged: true,
        write_effect: ExecutionWriteEffect::Verified,
        error_code: None,
        message: "装备合成并回读成功".to_owned(),
        diagnostics: BTreeMap::new(),
    }];
    report.status = ExecutionReportStatus::Success;
    report.stop_reason = ExecutionStopReason::Completed;
    report.acknowledged_write_count = 1;
    report.observed_state_change_count = 1;
    report.verified_write_count = 1;
    report.final_verification_status = ExecutionFinalVerificationStatus::Verified;
    report.final_verification_summary = Some("终态与合成预期一致".to_owned());
    report.content_sha256 = "f".repeat(64);
    report
}

/// 建立带完整强化产物、物资和材料回读的工作簿映射测试报告。
#[cfg(test)]
pub(crate) fn execution_workbook_enhance_report_fixture() -> ExecutionReport {
    let mut report = execution_workbook_report_fixture();
    let source = PlanSource::Warehouse { config_id: 1000 };
    report.steps = vec![ExecutionStepResult {
        sequence: 1,
        step_kind: "enhance",
        step: PlanStep::Enhance {
            sequence: 1,
            source,
            source_equipment: PlanEquipment::from_raw(1000, 1000, 0),
            target_equipment: PlanEquipment::from_raw(1000, 1001, 1),
            cost: PlanEnhanceCost::from_raw(10, vec![PlanEnhanceMaterialCost::from_raw(3001, 2)]),
        },
        status: ExecutionStatus::Success,
        command_id: Some("d".repeat(64)),
        pre_state_content_sha256: Some("e".repeat(64)),
        post_state_content_sha256: Some("f".repeat(64)),
        request_summary: Some("强化仓库装备".to_owned()),
        response_summary: Some("运行态确认命令成功".to_owned()),
        readback_summary: Some("装备、物资和材料均与完整模拟一致".to_owned()),
        readback_evidence: Some(ExecutionReadbackEvidence {
            matches_expected: true,
            target_slot: None,
            source: Some(source),
            source_slot: None,
            warehouse: vec![
                ExecutionWarehouseReadback {
                    config_id: 1000,
                    before_quantity: 1,
                    expected_after_quantity: 0,
                    actual_after_quantity: 0,
                },
                ExecutionWarehouseReadback {
                    config_id: 1001,
                    before_quantity: 2,
                    expected_after_quantity: 3,
                    actual_after_quantity: 3,
                },
            ],
            gold: Some(ExecutionGoldReadback {
                before: 100,
                expected_after: 90,
                actual_after: 90,
            }),
            materials: vec![ExecutionMaterialReadback {
                item_id: 3001,
                before_quantity: 10,
                expected_after_quantity: 8,
                actual_after_quantity: 8,
            }],
            dismantled_source_quantity: None,
            composed_output_quantity: None,
            enhanced_target_quantity: Some(1),
            first_mismatch: None,
        }),
        write_acknowledged: true,
        write_effect: ExecutionWriteEffect::Verified,
        error_code: None,
        message: "装备强化并回读成功".to_owned(),
        diagnostics: BTreeMap::new(),
    }];
    report.status = ExecutionReportStatus::Success;
    report.stop_reason = ExecutionStopReason::Completed;
    report.acknowledged_write_count = 1;
    report.observed_state_change_count = 1;
    report.verified_write_count = 1;
    report.final_verification_status = ExecutionFinalVerificationStatus::Verified;
    report.final_verification_summary = Some("终态与强化预期一致".to_owned());
    report.content_sha256 = "0".repeat(64);
    report
}

/// 建立实际回读偏离计划来源和目标状态的工作簿映射测试报告。
#[cfg(test)]
pub(crate) fn execution_workbook_mismatch_report_fixture() -> ExecutionReport {
    let mut report = execution_workbook_report_fixture();
    let step = &mut report.steps[0];
    let evidence = step
        .readback_evidence
        .as_mut()
        .expect("首个测试步骤应包含回读证据");
    let actual = ExecutionSlotState::Equipped(ExecutionEquipmentState {
        config_id: 1000,
        enhance_level: 0,
    });
    evidence.matches_expected = false;
    let target_slot = evidence
        .target_slot
        .as_mut()
        .expect("装备步骤应包含目标槽位证据");
    target_slot.actual_after = actual;
    let slot = target_slot.slot;
    let expected = target_slot.expected_after;
    evidence.first_mismatch = Some(ExecutionStateMismatch::Slot {
        slot,
        expected,
        actual,
    });
    step.status = ExecutionStatus::Failed;
    step.write_effect = ExecutionWriteEffect::StateChangedMismatch;
    step.readback_summary = Some("目标槽位实际装备与完整模拟不一致".to_owned());
    step.message = "命令完成，但回读状态不符合计划".to_owned();
    report.status = ExecutionReportStatus::Failed;
    report.stop_reason = ExecutionStopReason::ReadbackMismatch;
    report.verified_write_count = 0;
    report.final_verification_status = ExecutionFinalVerificationStatus::Mismatch;
    report.content_sha256 = "d".repeat(64);
    report
}

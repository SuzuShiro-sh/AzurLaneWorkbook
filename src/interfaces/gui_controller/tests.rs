//! 覆盖界面控制器命令编排、状态更新和错误呈现的单元测试。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::{
    GuiAction, GuiController, GuiControllerError, GuiDiagnosticItem, GuiInstanceItem, GuiOperation,
    GuiOperationOutput, GuiSupportSnapshot, GuiTaskFactory, GuiTaskOutput,
};
use crate::application::{
    DiagnosticArtifactRef, EmulatorInstanceState, LogCatalogEntry, LogRecordKind,
};
use suzushiro_task_runtime::TaskFailure;

const TASK_TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn diagnostic_refresh_updates_readable_sources_and_keeps_failed_sources() {
    let root = std::env::temp_dir().join(format!("azlw-partial-catalog-{}", std::process::id()));
    std::fs::create_dir_all(root.join("data/logs")).unwrap();
    // 设置无效使实例发现留在本地，诊断目录仍独立读取。
    std::fs::write(root.join("settings.json"), b"{").unwrap();
    std::fs::write(root.join("data/history"), b"not a directory").unwrap();
    let first = "data/logs/001-runtime.jsonl";
    let latest = "data/logs/999-runtime.jsonl";
    std::fs::write(root.join(first), b"{\"timestamp_ms\":1}\n").unwrap();
    let mut controller =
        GuiController::new(crate::bootstrap::bootstrap_gui_task_factory(root.clone()));
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(
        controller
            .view()
            .diagnostics()
            .iter()
            .any(|item| item.source().relative_path() == first)
    );
    std::fs::write(root.join(latest), b"{\"timestamp_ms\":2}\n").unwrap();
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let index = controller
        .view()
        .diagnostics()
        .iter()
        .position(|item| item.source().relative_path() == latest)
        .unwrap();
    controller.select_diagnostic(index).unwrap();
    let selected = controller
        .view()
        .selected_diagnostic()
        .unwrap()
        .source()
        .clone();
    std::fs::remove_dir_all(root.join("data/logs")).unwrap();
    std::fs::write(root.join("data/logs"), b"not a directory").unwrap();
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(
        controller.view().selected_diagnostic().unwrap().source(),
        &selected
    );
    let view = controller.view();
    let detail = view.last_failure_detail().unwrap();
    assert!(detail.contains("历史目录读取"));
    assert!(detail.contains("日志目录读取"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn refresh_replaces_equal_length_log_identity_and_preserves_selection() {
    let root = std::env::temp_dir().join(format!("azlw-log-refresh-{}", std::process::id()));
    std::fs::create_dir_all(root.join("data/logs")).unwrap();
    std::fs::write(root.join("settings.json"), b"{}").unwrap();
    let selected = "data/logs/001-runtime.jsonl";
    std::fs::write(root.join(selected), b"{\"timestamp_ms\":1,\"value\":1}\n").unwrap();
    let mut controller =
        GuiController::new(crate::bootstrap::bootstrap_gui_task_factory(root.clone()));
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let index = controller
        .view()
        .diagnostics()
        .iter()
        .position(|item| item.source().relative_path() == selected)
        .unwrap();
    controller.select_diagnostic(index).unwrap();
    let before = controller
        .view()
        .selected_diagnostic()
        .unwrap()
        .source()
        .clone();
    // 首行时间和长度不变，只替换正文；目录刷新必须取得新的内容身份。
    std::fs::write(root.join(selected), b"{\"timestamp_ms\":1,\"value\":2}\n").unwrap();
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let after = controller
        .view()
        .selected_diagnostic()
        .unwrap()
        .source()
        .clone();
    assert_eq!(after.relative_path(), selected);
    assert_eq!(before.size_bytes(), after.size_bytes());
    assert_ne!(before.file_sha256(), after.file_sha256());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn agent_management_uses_the_selected_instance_and_refreshes_status() {
    let factory = GuiTaskFactory::from_handler(|operation, instance, _| match operation {
        GuiOperation::Initialize => Ok(delivered(ready_output("ready"))),
        GuiOperation::AgentStatus | GuiOperation::UnloadAgent => {
            assert_eq!(instance.as_deref(), Some("1"));
            let status = if operation == GuiOperation::AgentStatus {
                "代理可用"
            } else {
                "代理已卸载"
            };
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success(status).with_agent_status(status.to_owned())),
                None,
            ))
        }
        _ => panic!("不应发起其他操作"),
    });
    let mut controller = GuiController::new(factory);
    assert!(controller.start(GuiAction::UnloadAgent, || {}).is_err());
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    for (action, expected) in [
        (GuiAction::AgentStatus, "代理可用"),
        (GuiAction::UnloadAgent, "代理已卸载"),
    ] {
        controller.start(action, || {}).unwrap();
        assert_eq!(controller.view().agent_status(), None);
        drain_until_terminal(&mut controller);
        assert_eq!(controller.view().agent_status(), Some(expected));
    }
}

fn ready_output(summary: &str) -> GuiOperationOutput {
    GuiOperationOutput::success(summary).with_support_snapshot(GuiSupportSnapshot::new(
        vec![GuiInstanceItem::explicit(
            "1".to_owned(),
            "1: 已启动实例".to_owned(),
            EmulatorInstanceState::Ready,
        )],
        Some("1".to_owned()),
    ))
}

fn delivered(output: GuiOperationOutput) -> GuiTaskOutput {
    GuiTaskOutput::new(Ok(output), None)
}

#[test]
fn settings_state_changes_only_after_successful_save_without_device_selection() {
    let factory = GuiTaskFactory::from_handler(|operation, _, _| match operation {
        GuiOperation::LoadSettings => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("已读取")
                .with_preferences(crate::application::UserPreferences::default())),
            None,
        )),
        GuiOperation::SaveSettings {
            original,
            preferences,
        } if preferences.ship_acquisition_enabled => {
            assert_eq!(original, crate::application::UserPreferences::default());
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("已保存").with_preferences(
                    crate::application::UserPreferences {
                        ship_acquisition_enabled: true,
                        ..Default::default()
                    },
                )),
                None,
            ))
        }
        GuiOperation::SaveSettings { .. } => Err(TaskFailure::new("保存失败", "磁盘只读")),
        _ => panic!("设置不应访问游戏操作"),
    });
    let mut controller = GuiController::new(factory);
    controller.start(GuiAction::LoadSettings, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(
        controller
            .view()
            .preferences()
            .map(|p| p.ship_acquisition_enabled),
        Some(false)
    );
    controller
        .start(
            GuiAction::SaveSettings {
                original: crate::application::UserPreferences::default(),
                preferences: crate::application::UserPreferences {
                    ship_acquisition_enabled: true,
                    ..Default::default()
                },
            },
            || {},
        )
        .unwrap();
    assert_eq!(
        controller
            .view()
            .preferences()
            .map(|p| p.ship_acquisition_enabled),
        Some(false)
    );
    drain_until_terminal(&mut controller);
    assert_eq!(
        controller
            .view()
            .preferences()
            .map(|p| p.ship_acquisition_enabled),
        Some(true)
    );
    controller
        .start(
            GuiAction::SaveSettings {
                original: controller.view().preferences().unwrap(),
                preferences: crate::application::UserPreferences::default(),
            },
            || {},
        )
        .unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(
        controller
            .view()
            .preferences()
            .map(|p| p.ship_acquisition_enabled),
        Some(true)
    );
    assert!(
        controller
            .view()
            .last_failure_detail()
            .unwrap()
            .contains("磁盘只读")
    );
}

#[test]
fn settings_operations_do_not_read_diagnostic_catalogs() {
    let root = std::env::temp_dir().join(format!("azlw-settings-catalog-{}", std::process::id()));
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(
        root.join("settings.json"),
        include_str!("../../../settings.json"),
    )
    .unwrap();
    std::fs::write(root.join("data/history"), b"not a directory").unwrap();
    let mut controller =
        GuiController::new(crate::bootstrap::bootstrap_gui_task_factory(root.clone()));
    controller.start(GuiAction::LoadSettings, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let original = controller.view().preferences().unwrap();
    let mut preferences = original;
    assert!(controller.view().last_failure_detail().is_none());
    assert!(controller.view().diagnostics().is_empty());
    preferences.ship_acquisition_enabled = !preferences.ship_acquisition_enabled;
    controller
        .start(
            GuiAction::SaveSettings {
                original,
                preferences,
            },
            || {},
        )
        .unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().preferences(), Some(preferences));
    assert!(controller.view().last_failure_detail().is_none());
    assert!(controller.view().diagnostics().is_empty());
    let settings: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        settings["workbook"]["ship_acquisition_enabled"],
        preferences.ship_acquisition_enabled
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_save_rejects_changes_since_the_gui_loaded_its_draft() {
    let root = std::env::temp_dir().join(format!("azlw-settings-conflict-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    std::fs::write(&path, include_str!("../../../settings.json")).unwrap();
    let mut controller =
        GuiController::new(crate::bootstrap::bootstrap_gui_task_factory(root.clone()));
    controller.start(GuiAction::LoadSettings, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let original = controller.view().preferences().unwrap();
    let newer = crate::application::UserPreferences {
        detailed_diagnostics: !original.detailed_diagnostics,
        ..original
    };
    crate::adapters::settings::Settings::save_preferences(&root, original, newer).unwrap();
    let saved = std::fs::read(&path).unwrap();
    let preferences = crate::application::UserPreferences {
        ship_acquisition_enabled: !original.ship_acquisition_enabled,
        ..original
    };
    controller
        .start(
            GuiAction::SaveSettings {
                original,
                preferences,
            },
            || {},
        )
        .unwrap();
    drain_until_terminal(&mut controller);
    assert!(
        controller
            .view()
            .last_failure_detail()
            .unwrap()
            .contains("重新打开设置")
    );
    assert_eq!(controller.view().preferences(), Some(original));
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    controller.start(GuiAction::LoadSettings, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let original = controller.view().preferences().unwrap();
    assert_eq!(original, newer);
    let preferences = crate::application::UserPreferences {
        ship_acquisition_enabled: !original.ship_acquisition_enabled,
        ..original
    };
    controller
        .start(
            GuiAction::SaveSettings {
                original,
                preferences,
            },
            || {},
        )
        .unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().last_failure_detail().is_none());
    assert_eq!(controller.view().preferences(), Some(preferences));
    assert_eq!(
        crate::adapters::settings::Settings::load(&root)
            .unwrap()
            .preferences(),
        preferences
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn settings_save_keeps_the_file_access_cause_in_gui_details() {
    let root = std::env::temp_dir().join(format!("azlw-gui-settings-error-{}", std::process::id()));
    std::fs::create_dir_all(root.join("settings.lock")).unwrap();
    let path = root.join("settings.json");
    let contents = include_str!("../../../settings.json");
    std::fs::write(&path, contents).unwrap();
    let mut controller =
        GuiController::new(crate::bootstrap::bootstrap_gui_task_factory(root.clone()));
    controller.start(GuiAction::LoadSettings, || {}).unwrap();
    drain_until_terminal(&mut controller);
    let original = controller.view().preferences().unwrap();
    controller
        .start(
            GuiAction::SaveSettings {
                original,
                preferences: crate::application::UserPreferences {
                    ship_acquisition_enabled: !original.ship_acquisition_enabled,
                    ..original
                },
            },
            || {},
        )
        .unwrap();
    drain_until_terminal(&mut controller);
    let view = controller.view();
    let detail = view.last_failure_detail().unwrap();
    assert!(detail.contains("settings.lock"), "{detail}");
    assert!(detail.contains("普通文件"), "{detail}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn diagnostic_open_requires_a_selected_file() {
    let factory = GuiTaskFactory::from_handler(|operation, _, _| {
        assert_eq!(operation, GuiOperation::Initialize);
        Ok(delivered(ready_output("ready")))
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(!controller.view().controls().diagnostic_open_enabled);
    assert!(matches!(
        controller.start_diagnostic_open(|| {}),
        Err(GuiControllerError::DiagnosticNotSelected)
    ));
}

#[test]
fn initialization_loads_sorted_workbooks_and_enables_business_actions() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| {
        assert_eq!(operation, GuiOperation::Initialize);
        Ok(delivered(ready_output("应用已就绪").with_workbooks(vec![
            "b.xlsx".to_owned(),
            "a.xlsx".to_owned(),
        ])))
    });
    let mut controller = GuiController::new(factory);

    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "已就绪");
    assert_eq!(controller.view().workbooks(), ["a.xlsx", "b.xlsx"]);
    assert_eq!(controller.view().selected_workbook(), Some("a.xlsx"));
    let controls = controller.view().controls();
    assert!(controls.synchronize_enabled);
    assert!(controls.check_enabled);
    assert!(controls.execute_enabled);
    assert!(controls.open_enabled);
}

#[test]
fn selected_workbook_is_bound_to_each_requested_operation() {
    let operations: Arc<Mutex<Vec<GuiOperation>>> = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&operations);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| {
        observed.lock().unwrap().push(operation.clone());
        match operation {
            GuiOperation::Initialize => {
                Ok(delivered(ready_output("ready").with_workbooks(vec![
                    "a.xlsx".to_owned(),
                    "b.xlsx".to_owned(),
                ])))
            }
            _ => Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("done")),
                None,
            )),
        }
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.select_workbook(1).unwrap();

    for action in [
        GuiAction::CheckPlan,
        GuiAction::ExecutePlan,
        GuiAction::OpenWorkbook,
    ] {
        controller.start(action, || {}).unwrap();
        drain_until_terminal(&mut controller);
    }

    assert_eq!(
        operations.lock().unwrap().as_slice(),
        [
            GuiOperation::Initialize,
            GuiOperation::CheckPlan("b.xlsx".to_owned()),
            GuiOperation::ExecutePlan("b.xlsx".to_owned()),
            GuiOperation::OpenWorkbook("b.xlsx".to_owned()),
        ]
    );
}

#[test]
fn generated_workbook_is_added_and_selected_without_masking_success() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let calls = Arc::clone(&call_count);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| {
        calls.fetch_add(1, Ordering::AcqRel);
        match operation {
            GuiOperation::Initialize => Ok(delivered(
                ready_output("ready").with_workbooks(vec!["old.xlsx".to_owned()]),
            )),
            GuiOperation::SynchronizeAndGenerate => Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("generated")
                    .with_selected_workbook("new.xlsx".to_owned())),
                None,
            )),
            other => panic!("未预期操作: {other:?}"),
        }
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    controller
        .start(GuiAction::SynchronizeAndGenerate, || {})
        .unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(call_count.load(Ordering::Acquire), 2);
    assert_eq!(controller.view().status(), "操作完成");
    assert_eq!(controller.view().workbooks(), ["new.xlsx", "old.xlsx"]);
    assert_eq!(controller.view().selected_workbook(), Some("new.xlsx"));
}

#[test]
fn completed_generation_cleanup_failure_keeps_the_workbook_visible_and_requires_check() {
    let operations: Arc<Mutex<Vec<GuiOperation>>> = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&operations);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| {
        observed.lock().unwrap().push(operation.clone());
        match operation {
            GuiOperation::Initialize => Ok(delivered(
                ready_output("ready").with_workbooks(vec!["old.xlsx".to_owned()]),
            )),
            GuiOperation::SynchronizeAndGenerate => Ok(GuiTaskOutput::new(
                Ok(
                    GuiOperationOutput::attention("工作簿已经生成，但游戏会话清理未完整确认")
                        .with_selected_workbook("new.xlsx".to_owned())
                        .with_diagnostic_detail("cleanup=failed".to_owned()),
                ),
                None,
            )),
            GuiOperation::CheckPlan(workbook) => {
                assert_eq!(workbook, "new.xlsx");
                Ok(GuiTaskOutput::new(
                    Ok(GuiOperationOutput::success("checked")),
                    None,
                ))
            }
            GuiOperation::ExecutePlan(workbook) => {
                assert_eq!(workbook, "new.xlsx");
                Ok(GuiTaskOutput::new(
                    Ok(GuiOperationOutput::success("executed")),
                    None,
                ))
            }
            other => panic!("未预期操作: {other:?}"),
        }
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    controller
        .start(GuiAction::SynchronizeAndGenerate, || {})
        .unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "需要核对");
    assert!(controller.view().message().contains("工作簿已经生成"));
    assert_eq!(controller.view().workbooks(), ["new.xlsx", "old.xlsx"]);
    assert_eq!(controller.view().selected_workbook(), Some("new.xlsx"));
    assert_eq!(
        controller.view().last_failure_detail(),
        Some("cleanup=failed")
    );
    assert!(controller.view().controls().check_enabled);
    assert!(!controller.view().controls().execute_enabled);
    assert!(!controller.view().controls().synchronize_enabled);
    assert!(matches!(
        controller.start(GuiAction::SynchronizeAndGenerate, || {}),
        Err(GuiControllerError::GenerationRecheckRequired)
    ));

    assert!(matches!(
        controller.start(GuiAction::ExecutePlan, || {}),
        Err(GuiControllerError::ExecutionCheckRequired)
    ));
    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().execute_enabled);

    controller.start(GuiAction::ExecutePlan, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().status(), "操作完成");
    assert_eq!(
        operations.lock().unwrap().as_slice(),
        [
            GuiOperation::Initialize,
            GuiOperation::SynchronizeAndGenerate,
            GuiOperation::CheckPlan("new.xlsx".to_owned()),
            GuiOperation::ExecutePlan("new.xlsx".to_owned()),
        ]
    );

    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().synchronize_enabled);
    assert_eq!(
        operations.lock().unwrap().last(),
        Some(&GuiOperation::Initialize)
    );
}

#[test]
fn generation_worker_panic_requires_release_recheck() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(delivered(
            ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
        )),
        GuiOperation::SynchronizeAndGenerate => panic!("fixture generation panic"),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    controller
        .start(GuiAction::SynchronizeAndGenerate, || {})
        .unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "操作失败");
    assert!(controller.view().message().contains("生成结果不确定"));
    assert!(!controller.view().controls().synchronize_enabled);
    assert!(!controller.view().controls().execute_enabled);
    assert!(controller.view().controls().refresh_enabled);
    assert!(matches!(
        controller.start(GuiAction::SynchronizeAndGenerate, || {}),
        Err(GuiControllerError::GenerationRecheckRequired)
    ));

    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().synchronize_enabled);
}

#[test]
fn recheck_revalidates_release_and_preserves_an_existing_selection() {
    let operations: Arc<Mutex<Vec<GuiOperation>>> = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&operations);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| {
        observed.lock().unwrap().push(operation.clone());
        assert_eq!(operation, GuiOperation::Initialize);
        Ok(delivered(ready_output("ready").with_workbooks(vec![
            "a.xlsx".to_owned(),
            "b.xlsx".to_owned(),
        ])))
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.select_workbook(1).unwrap();

    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().selected_workbook(), Some("b.xlsx"));
    assert_eq!(
        operations.lock().unwrap().as_slice(),
        [GuiOperation::Initialize, GuiOperation::Initialize]
    );
}

#[test]
fn running_operation_is_exclusive_and_cancellation_reaches_checkpoint() {
    let barrier = Arc::new(Barrier::new(2));
    let task_barrier = Arc::clone(&barrier);
    let factory =
        GuiTaskFactory::from_handler(move |operation, _instance, context| match operation {
            GuiOperation::Initialize => Ok(delivered(
                ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
            )),
            GuiOperation::CheckPlan(_) => {
                task_barrier.wait();
                task_barrier.wait();
                context
                    .report_progress(50, "不应入队")
                    .map_err(TaskFailure::from_signal)?;
                Ok(GuiTaskOutput::new(
                    Ok(GuiOperationOutput::success("不应展示")),
                    None,
                ))
            }
            other => panic!("未预期操作: {other:?}"),
        });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    barrier.wait();

    assert!(matches!(
        controller.start(GuiAction::OpenWorkbook, || {}),
        Err(GuiControllerError::Busy)
    ));
    assert!(controller.request_cancel());
    barrier.wait();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "已取消");
}

#[test]
fn cancellation_during_execution_keeps_the_terminal_result_visible() {
    let barrier = Arc::new(Barrier::new(2));
    let task_barrier = Arc::clone(&barrier);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(delivered(
            ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
        )),
        GuiOperation::ExecutePlan(_) => {
            task_barrier.wait();
            task_barrier.wait();
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::attention(
                    "执行结果无法确认，请人工核对",
                )),
                None,
            ))
        }
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::ExecutePlan, || {}).unwrap();
    barrier.wait();

    assert!(controller.request_cancel());
    barrier.wait();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "需要核对");
    assert!(controller.view().message().contains("请人工核对"));
}

#[test]
fn cancellation_during_generation_attention_keeps_the_published_workbook_visible() {
    let barrier = Arc::new(Barrier::new(2));
    let task_barrier = Arc::clone(&barrier);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(delivered(
            ready_output("ready").with_workbooks(vec!["old.xlsx".to_owned()]),
        )),
        GuiOperation::SynchronizeAndGenerate => {
            task_barrier.wait();
            task_barrier.wait();
            Ok(GuiTaskOutput::new(
                Ok(
                    GuiOperationOutput::attention("工作簿已发布但清理状态待核对")
                        .with_selected_workbook("new.xlsx".to_owned()),
                ),
                None,
            ))
        }
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller
        .start(GuiAction::SynchronizeAndGenerate, || {})
        .unwrap();
    barrier.wait();

    assert!(controller.request_cancel());
    barrier.wait();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().selected_workbook(), Some("new.xlsx"));
}

#[test]
fn attention_blocks_execution_until_the_selected_workbook_is_checked() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| match operation {
        GuiOperation::Initialize => {
            Ok(delivered(ready_output("ready").with_workbooks(vec![
                "other.xlsx".to_owned(),
                "plan.xlsx".to_owned(),
            ])))
        }
        GuiOperation::ExecutePlan(workbook_name) => {
            assert_eq!(workbook_name, "other.xlsx");
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::attention(
                    "执行结果无法确认，请人工核对",
                )),
                None,
            ))
        }
        GuiOperation::CheckPlan(workbook_name) => {
            assert_eq!(workbook_name, "other.xlsx");
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("检查完成")),
                None,
            ))
        }
        GuiOperation::SynchronizeAndGenerate => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("generated")
                .with_selected_workbook("other.xlsx".to_owned())),
            None,
        )),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    controller.start(GuiAction::ExecutePlan, || {}).unwrap();
    drain_until_terminal(&mut controller);

    assert!(!controller.view().controls().execute_enabled);
    assert!(matches!(
        controller.start(GuiAction::ExecutePlan, || {}),
        Err(GuiControllerError::ExecutionCheckRequired)
    ));

    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    drain_until_terminal(&mut controller);

    assert!(controller.view().controls().execute_enabled);
    controller.select_workbook(1).unwrap();
    assert!(!controller.view().controls().execute_enabled);
    assert!(matches!(
        controller.start(GuiAction::ExecutePlan, || {}),
        Err(GuiControllerError::ExecutionCheckRequired)
    ));
    controller.select_workbook(0).unwrap();
    assert!(controller.view().controls().execute_enabled);

    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(!controller.view().controls().execute_enabled);

    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().execute_enabled);
    controller
        .start(GuiAction::SynchronizeAndGenerate, || {})
        .unwrap();
    drain_until_terminal(&mut controller);
    assert!(!controller.view().controls().execute_enabled);
}

#[test]
fn execution_failure_blocks_retry_and_exposes_the_recovery_action() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(delivered(
            ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
        )),
        GuiOperation::ExecutePlan(_) => Err(TaskFailure::new(
            "执行历史保存失败",
            "execution_completed=true; backup_path=data/backups/plan.xlsx",
        )),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    controller.start(GuiAction::ExecutePlan, || {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "操作失败");
    assert!(controller.view().message().contains("禁止直接重试"));
    assert!(controller.view().message().contains("检查当前工作簿计划"));
    assert!(!controller.view().controls().execute_enabled);
}

#[test]
fn a_later_failed_check_invalidates_the_execution_recovery() {
    let check_count = Arc::new(AtomicUsize::new(0));
    let checks = Arc::clone(&check_count);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(delivered(
            ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
        )),
        GuiOperation::ExecutePlan(_) => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::attention(
                "执行结果无法确认，请人工核对",
            )),
            None,
        )),
        GuiOperation::CheckPlan(_) if checks.fetch_add(1, Ordering::AcqRel) == 0 => Ok(
            GuiTaskOutput::new(Ok(GuiOperationOutput::success("检查完成")), None),
        ),
        GuiOperation::CheckPlan(_) => {
            Err(TaskFailure::new("计划检查失败", "fixture check failure"))
        }
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::ExecutePlan, || {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().execute_enabled);

    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    assert!(!controller.view().controls().execute_enabled);
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "操作失败");
    assert!(!controller.view().controls().execute_enabled);
    assert_eq!(check_count.load(Ordering::Acquire), 2);
}

#[test]
fn notifier_panic_does_not_leave_the_controller_running() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| {
        assert_eq!(operation, GuiOperation::Initialize);
        Ok(delivered(ready_output("ready")))
    });
    let mut controller = GuiController::new(factory);
    controller
        .start_initialization(|| panic!("fixture notifier failure"))
        .unwrap();
    drain_until_terminal(&mut controller);

    assert!(!controller.view().is_running());
    assert!(controller.view().controls().refresh_enabled);
    assert_eq!(controller.view().status(), "检查未通过");
    assert!(
        controller
            .view()
            .last_failure_detail()
            .unwrap()
            .contains("后台工作线程回收失败")
    );
}

#[test]
fn initialization_attention_does_not_unlock_or_publish_workbooks() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| {
        assert_eq!(operation, GuiOperation::Initialize);
        Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::attention("应用状态仍需核对")
                .with_workbooks(vec!["must-not-appear.xlsx".to_owned()])),
            None,
        ))
    });
    let mut controller = GuiController::new(factory);

    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "需要核对");
    assert!(controller.view().workbooks().is_empty());
    assert!(!controller.view().controls().synchronize_enabled);
    assert!(controller.view().controls().refresh_enabled);
}

#[test]
fn cancelled_execution_requires_a_fresh_check() {
    let barrier = Arc::new(Barrier::new(2));
    let task_barrier = Arc::clone(&barrier);
    let factory =
        GuiTaskFactory::from_handler(move |operation, _instance, context| match operation {
            GuiOperation::Initialize => Ok(delivered(
                ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
            )),
            GuiOperation::ExecutePlan(_) => {
                task_barrier.wait();
                task_barrier.wait();
                context
                    .report_progress(50, "取消检查点")
                    .map_err(TaskFailure::from_signal)?;
                Ok(GuiTaskOutput::new(
                    Ok(GuiOperationOutput::success("不应返回")),
                    None,
                ))
            }
            GuiOperation::CheckPlan(_) => Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("checked")),
                None,
            )),
            other => panic!("未预期操作: {other:?}"),
        });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::ExecutePlan, || {}).unwrap();
    barrier.wait();

    assert!(controller.request_cancel());
    barrier.wait();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "已取消");
    assert!(!controller.view().controls().execute_enabled);
    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().execute_enabled);
}

#[test]
fn support_refresh_displays_configured_instance_and_opens_diagnostics() {
    let log = LogCatalogEntry::new(
        LogRecordKind::RuntimeProbe,
        "data/logs/001-runtime.jsonl".to_owned(),
        12,
        "a".repeat(64),
        1_700_000_000_123,
        Some(1),
    );
    let diagnostic = GuiDiagnosticItem::new(
        "日志 | 运行态探针".to_owned(),
        "日志 运行态探针".to_owned(),
        None,
        None,
        DiagnosticArtifactRef::from_log(&log),
        log.timestamp_unix_millis(),
        log.sequence(),
    );
    let initial_snapshot = GuiSupportSnapshot::new(
        vec![GuiInstanceItem::explicit(
            "7".to_owned(),
            "7: 碧蓝航线（已启动）".to_owned(),
            EmulatorInstanceState::Ready,
        )],
        Some("7".to_owned()),
    );
    let operations = Arc::new(Mutex::new(Vec::<GuiOperation>::new()));
    let observed = Arc::clone(&operations);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| {
        observed.lock().unwrap().push(operation.clone());
        match operation {
            GuiOperation::Initialize => Ok(GuiTaskOutput::new(
                Ok(ready_output("ready")
                    .with_workbooks(vec!["plan.xlsx".to_owned()])
                    .with_support_snapshot(initial_snapshot.clone())),
                Some(Ok(super::GuiDiagnosticSnapshot {
                    items: vec![diagnostic.clone()],
                    warnings: Vec::new(),
                    failed_sources: Vec::new(),
                })),
            )),
            GuiOperation::OpenDiagnostic(request) => {
                assert_eq!(request.relative_path(), log.relative_path());
                Ok(GuiTaskOutput::new(
                    Ok(GuiOperationOutput::success("opened")),
                    None,
                ))
            }
            other => panic!("未预期操作: {other:?}"),
        }
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().instances().len(), 1);
    assert_eq!(controller.view().diagnostics().len(), 1);

    assert_eq!(controller.view().selected_instance(), Some("7"));

    controller.start_diagnostic_open(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().status(), "操作完成");
    assert_eq!(operations.lock().unwrap().len(), 3);

    controller.start(GuiAction::Refresh, || {}).unwrap();
    assert_eq!(controller.view().instances().len(), 0);
    assert_eq!(controller.view().diagnostics().len(), 1);
    drain_until_terminal(&mut controller);
    assert_eq!(operations.lock().unwrap().len(), 4);
    assert_eq!(controller.view().workbooks(), &["plan.xlsx".to_owned()]);
    assert_eq!(controller.view().instances().len(), 1);
    assert_eq!(controller.view().selected_instance(), Some("7"));
    assert_eq!(controller.view().diagnostics().len(), 1);
    assert!(controller.view().controls().diagnostic_open_enabled);
}

#[test]
fn uncertain_diagnostic_open_invalidates_support_until_a_fresh_refresh() {
    let log = LogCatalogEntry::new(
        LogRecordKind::RuntimeProbe,
        "data/logs/001-runtime.jsonl".to_owned(),
        12,
        "a".repeat(64),
        1_700_000_000_123,
        Some(1),
    );
    let snapshot = super::GuiDiagnosticSnapshot {
        items: super::ordered_diagnostic_items(&[], &[log]),
        warnings: Vec::new(),
        failed_sources: Vec::new(),
    };
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("refreshed")),
            Some(Ok(snapshot.clone())),
        )),
        GuiOperation::OpenDiagnostic(_) => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::attention(
                "日志打开状态待核对，请刷新后确认文件身份",
            )),
            None,
        )),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().diagnostic_open_enabled);

    controller.start_diagnostic_open(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "需要核对");
    assert!(controller.view().diagnostics().is_empty());
    assert_eq!(controller.view().selected_diagnostic_index(), None);
    assert!(!controller.view().controls().diagnostic_open_enabled);
    assert!(controller.view().controls().refresh_enabled);
    assert!(matches!(
        controller.start_diagnostic_open(|| {}),
        Err(GuiControllerError::DiagnosticNotSelected)
    ));

    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert!(controller.view().controls().diagnostic_open_enabled);
}

#[test]
fn diagnostic_open_worker_panic_invalidates_support_until_a_fresh_refresh() {
    let log = LogCatalogEntry::new(
        LogRecordKind::RuntimeProbe,
        "data/logs/001-runtime.jsonl".to_owned(),
        12,
        "a".repeat(64),
        1_700_000_000_123,
        Some(1),
    );
    let snapshot = super::GuiDiagnosticSnapshot {
        items: super::ordered_diagnostic_items(&[], &[log]),
        warnings: Vec::new(),
        failed_sources: Vec::new(),
    };
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("refreshed")),
            Some(Ok(snapshot.clone())),
        )),
        GuiOperation::OpenDiagnostic(_) => panic!("fixture diagnostic open panic"),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);

    controller.start_diagnostic_open(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "操作失败");
    assert!(controller.view().message().contains("结果不确定"));
    assert!(controller.view().diagnostics().is_empty());
    assert!(!controller.view().controls().diagnostic_open_enabled);
    assert!(controller.view().controls().refresh_enabled);
}

#[test]
fn cancellation_during_uncertain_diagnostic_open_keeps_the_attention_visible() {
    let log = LogCatalogEntry::new(
        LogRecordKind::RuntimeProbe,
        "data/logs/001-runtime.jsonl".to_owned(),
        12,
        "a".repeat(64),
        1_700_000_000_123,
        Some(1),
    );
    let snapshot = super::GuiDiagnosticSnapshot {
        items: super::ordered_diagnostic_items(&[], &[log]),
        warnings: Vec::new(),
        failed_sources: Vec::new(),
    };
    let barrier = Arc::new(Barrier::new(2));
    let task_barrier = Arc::clone(&barrier);
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize => Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("refreshed")),
            Some(Ok(snapshot.clone())),
        )),
        GuiOperation::OpenDiagnostic(_) => {
            task_barrier.wait();
            task_barrier.wait();
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::attention(
                    "日志打开状态待核对，请刷新后确认文件身份",
                )),
                None,
            ))
        }
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start_diagnostic_open(|| {}).unwrap();
    barrier.wait();

    assert!(controller.request_cancel());
    barrier.wait();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "需要核对");
}

#[test]
fn failed_support_refresh_does_not_leave_the_previous_catalog_actionable() {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&refreshes);
    let snapshot = GuiSupportSnapshot::new(
        vec![GuiInstanceItem::explicit(
            "7".to_owned(),
            "7: 已启动实例".to_owned(),
            EmulatorInstanceState::Ready,
        )],
        None,
    );
    let factory = GuiTaskFactory::from_handler(move |operation, _instance, _| match operation {
        GuiOperation::Initialize if observed.fetch_add(1, Ordering::AcqRel) < 2 => {
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("refreshed")
                    .with_support_snapshot(snapshot.clone())),
                None,
            ))
        }
        GuiOperation::Initialize => Err(TaskFailure::new("刷新失败", "fixture refresh failure")),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().instances().len(), 1);

    controller.start(GuiAction::Refresh, || {}).unwrap();
    assert_eq!(controller.view().instances().len(), 0);
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "检查未通过");
    assert_eq!(controller.view().instances().len(), 0);
}

#[test]
fn failure_keeps_diagnostics_out_of_the_visible_message_and_allows_retry() {
    let factory = GuiTaskFactory::from_handler(|operation, _instance, _| match operation {
        GuiOperation::Initialize => Err(TaskFailure::new(
            "应用初始化未通过",
            "manifest.json missing at private fixture path",
        )),
        other => panic!("未预期操作: {other:?}"),
    });
    let mut controller = GuiController::new(factory);

    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);

    assert_eq!(controller.view().status(), "检查未通过");
    assert_eq!(controller.view().message(), "应用初始化未通过");
    assert!(!controller.view().message().contains("manifest.json"));
    assert!(
        controller
            .view()
            .last_failure_detail()
            .unwrap()
            .contains("manifest.json")
    );
    assert!(controller.view().controls().refresh_enabled);
    assert!(!controller.view().controls().synchronize_enabled);
}

fn drain_until_terminal(controller: &mut GuiController) {
    let deadline = Instant::now() + TASK_TIMEOUT;
    while controller.view().is_running() {
        controller.drain_events().unwrap();
        assert!(Instant::now() < deadline, "等待 GUI 后台任务终态超时");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn generation_progress_distinguishes_phase_counts_cleanup_and_terminal_results() {
    for fails in [false, true] {
        let barrier = Arc::new(Barrier::new(2));
        let worker_barrier = Arc::clone(&barrier);
        let factory =
            GuiTaskFactory::from_handler(move |operation, _instance, context| match operation {
                GuiOperation::Initialize => Ok(delivered(ready_output("ready"))),
                GuiOperation::SynchronizeAndGenerate => {
                    context.report_activity(Some((1, 2)), "写入工作表").unwrap();
                    worker_barrier.wait();
                    worker_barrier.wait();
                    context.report_activity(None, "卸载与清理").unwrap();
                    worker_barrier.wait();
                    worker_barrier.wait();
                    if fails {
                        Err(TaskFailure::new("清理失败", "fixture cleanup error"))
                    } else {
                        Ok(GuiTaskOutput::new(
                            Ok(GuiOperationOutput::success("生成完成")),
                            None,
                        ))
                    }
                }
                other => panic!("unexpected {other:?}"),
            });
        let mut controller = GuiController::new(factory);
        controller.start_initialization(|| {}).unwrap();
        drain_until_terminal(&mut controller);
        assert_eq!(controller.view().progress_percent(), Some(100));
        controller
            .start(GuiAction::SynchronizeAndGenerate, || {})
            .unwrap();
        assert_eq!(controller.view().progress_percent(), None);
        barrier.wait();
        controller.drain_events().unwrap();
        assert_eq!(controller.view().progress_percent(), Some(50));
        assert!(controller.view().status().contains("当前阶段 1/2"));
        assert_eq!(controller.view().progress().unwrap().units(), Some((1, 2)));
        barrier.wait();
        barrier.wait();
        controller.drain_events().unwrap();
        assert!(controller.view().is_running());
        assert_eq!(controller.view().progress_percent(), None);
        assert!(!controller.view().status().contains('%'));
        assert_eq!(controller.view().message(), "卸载与清理");
        barrier.wait();
        drain_until_terminal(&mut controller);
        assert_eq!(
            controller.view().progress_percent(),
            if fails { None } else { Some(100) }
        );
        assert_eq!(
            controller.view().progress().unwrap().message(),
            "卸载与清理"
        );
    }
}

#[test]
fn instance_selection_controls_availability_and_binds_game_tasks() {
    let observed = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::clone(&observed);
    let factory = GuiTaskFactory::from_handler(move |operation, instance, _| {
        calls
            .lock()
            .unwrap()
            .push((operation.clone(), instance.clone()));
        match operation {
            GuiOperation::Initialize => Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("ready")
                    .with_workbooks(vec!["plan.xlsx".to_owned()])
                    .with_support_snapshot(GuiSupportSnapshot::new(
                        vec![
                            GuiInstanceItem::explicit(
                                "1".to_owned(),
                                "一".to_owned(),
                                EmulatorInstanceState::Ready,
                            ),
                            GuiInstanceItem::explicit(
                                "2".to_owned(),
                                "二".to_owned(),
                                EmulatorInstanceState::Stopped,
                            ),
                            GuiInstanceItem::explicit(
                                "3".to_owned(),
                                "三".to_owned(),
                                EmulatorInstanceState::Starting,
                            ),
                            GuiInstanceItem::explicit(
                                "4".to_owned(),
                                "四".to_owned(),
                                EmulatorInstanceState::Unavailable,
                            ),
                            GuiInstanceItem::explicit(
                                "5".to_owned(),
                                "五".to_owned(),
                                EmulatorInstanceState::Ready,
                            ),
                        ],
                        instance,
                    ))),
                None,
            )),
            _ => Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("done")),
                None,
            )),
        }
    });
    let mut controller = GuiController::new(factory);
    assert!(!controller.view().controls().synchronize_enabled);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().instances().len(), 5);
    assert_eq!(controller.view().selected_instance(), Some("1"));
    for index in 1..=3 {
        controller.select_instance(index).unwrap();
        assert!(!controller.view().controls().synchronize_enabled);
        assert!(!controller.view().controls().check_enabled);
        assert!(!controller.view().controls().execute_enabled);
        assert!(matches!(
            controller.start(GuiAction::SynchronizeAndGenerate, || {}),
            Err(GuiControllerError::InstanceUnavailable)
        ));
    }
    controller.select_instance(4).unwrap();
    assert!(controller.view().controls().synchronize_enabled);
    assert!(controller.view().controls().check_enabled);
    assert!(!controller.view().controls().execute_enabled);
    for action in [
        GuiAction::SynchronizeAndGenerate,
        GuiAction::CheckPlan,
        GuiAction::ExecutePlan,
    ] {
        controller.start(action, || {}).unwrap();
        assert!(matches!(
            controller.select_instance(0),
            Err(GuiControllerError::Busy)
        ));
        drain_until_terminal(&mut controller);
    }
    assert!(controller.view().controls().execute_enabled);
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().selected_instance(), Some("5"));
    assert!(!controller.view().controls().execute_enabled);
    let calls = observed.lock().unwrap();
    assert_eq!(calls.len(), 5);
    for (_, instance) in &calls[1..] {
        assert_eq!(instance.as_deref(), Some("5"));
    }
}

#[test]
fn refresh_does_not_substitute_another_instance_when_the_selected_target_disappears() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&calls);
    let factory = GuiTaskFactory::from_handler(move |operation, selected, _| {
        assert_eq!(operation, GuiOperation::Initialize);
        let first = count.fetch_add(1, Ordering::Relaxed) == 0;
        Ok(GuiTaskOutput::new(
            Ok(GuiOperationOutput::success("ready").with_support_snapshot(
                GuiSupportSnapshot::new(
                    vec![GuiInstanceItem::explicit(
                        if first { "1" } else { "2" }.to_owned(),
                        "实例".to_owned(),
                        EmulatorInstanceState::Ready,
                    )],
                    selected,
                ),
            )),
            None,
        ))
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().selected_instance(), Some("1"));
    controller.start(GuiAction::Refresh, || {}).unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().selected_instance(), None);
    assert!(!controller.view().controls().synchronize_enabled);
    controller.select_instance(0).unwrap();
    assert_eq!(controller.view().selected_instance(), Some("2"));
    assert!(controller.view().controls().synchronize_enabled);
}

#[test]
fn plan_check_progress_shows_activity_and_phase_counts() {
    let barrier = Arc::new(Barrier::new(2));
    let worker = Arc::clone(&barrier);
    let factory = GuiTaskFactory::from_handler(move |operation, _, context| match operation {
        GuiOperation::Initialize => Ok(delivered(
            ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
        )),
        GuiOperation::CheckPlan(_) => {
            context.report_activity(None, "正在读取配装计划").unwrap();
            worker.wait();
            worker.wait();
            context
                .report_activity(Some((2, 5)), "正在读取舰船详情")
                .unwrap();
            worker.wait();
            worker.wait();
            context
                .report_activity(None, "正在保存计划检查记录")
                .unwrap();
            worker.wait();
            worker.wait();
            Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("检查完成")),
                None,
            ))
        }
        other => panic!("unexpected {other:?}"),
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller.start(GuiAction::CheckPlan, || {}).unwrap();
    for (message, units) in [
        ("正在读取配装计划", None),
        ("正在读取舰船详情", Some((2, 5))),
        ("正在保存计划检查记录", None),
    ] {
        barrier.wait();
        controller.drain_events().unwrap();
        assert!(controller.view().is_running());
        assert_eq!(controller.view().message(), message);
        assert_eq!(controller.view().progress().unwrap().units(), units);
        if units.is_none() {
            assert!(controller.view().status().contains(message));
            assert_eq!(controller.view().progress_percent(), None);
        } else {
            assert_eq!(controller.view().progress_percent(), Some(40));
        }
        barrier.wait();
    }
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().progress_percent(), Some(100));
}

#[test]
fn execution_progress_keeps_writeback_and_cleanup_running_after_step_completion() {
    for fails in [false, true] {
        let barrier = Arc::new(Barrier::new(2));
        let worker = Arc::clone(&barrier);
        let factory = GuiTaskFactory::from_handler(move |operation, _, context| match operation {
            GuiOperation::Initialize => Ok(delivered(
                ready_output("ready").with_workbooks(vec!["plan.xlsx".to_owned()]),
            )),
            GuiOperation::CheckPlan(_) => Ok(GuiTaskOutput::new(
                Ok(GuiOperationOutput::success("检查完成")),
                None,
            )),
            GuiOperation::ExecutePlan(_) => {
                for (message, units) in [
                    ("第1步已核验：装备", Some((1, 1))),
                    ("正在写回装备总表", None),
                    ("正在清理会话", None),
                ] {
                    context.report_activity(units, message).unwrap();
                    worker.wait();
                    worker.wait();
                }
                if fails {
                    Err(TaskFailure::new("清理失败", "fixture cleanup failure"))
                } else {
                    Ok(GuiTaskOutput::new(
                        Ok(GuiOperationOutput::success("执行完成")),
                        None,
                    ))
                }
            }
            other => panic!("unexpected {other:?}"),
        });
        let mut controller = GuiController::new(factory);
        controller.start_initialization(|| {}).unwrap();
        drain_until_terminal(&mut controller);
        controller.start(GuiAction::CheckPlan, || {}).unwrap();
        drain_until_terminal(&mut controller);
        controller.start(GuiAction::ExecutePlan, || {}).unwrap();
        for (message, percent) in [
            ("第1步已核验：装备", Some(100)),
            ("正在写回装备总表", None),
            ("正在清理会话", None),
        ] {
            barrier.wait();
            controller.drain_events().unwrap();
            assert!(controller.view().is_running());
            assert_eq!(controller.view().message(), message);
            assert_eq!(controller.view().progress_percent(), percent);
            if percent.is_some() {
                assert!(controller.view().status().contains("当前阶段"));
            }
            barrier.wait();
        }
        drain_until_terminal(&mut controller);
        assert_eq!(
            controller.view().progress_percent(),
            if fails { None } else { Some(100) }
        );
    }
}

#[test]
fn completed_operations_refresh_diagnostics_without_replacing_their_outcome_or_instance() {
    for fails in [false, true] {
        for action in [
            GuiAction::SynchronizeAndGenerate,
            GuiAction::CheckPlan,
            GuiAction::ExecutePlan,
        ] {
            let factory = GuiTaskFactory::from_handler(move |operation, _, _| {
                if matches!(operation, GuiOperation::Initialize) {
                    return Ok(delivered(
                        ready_output("ready").with_workbooks(vec!["plan.xlsx".into()]),
                    ));
                }
                let result = if fails {
                    Err(TaskFailure::new("业务失败", "original-error"))
                } else {
                    Ok(GuiOperationOutput::success("业务完成"))
                };
                let log = LogCatalogEntry::new(
                    LogRecordKind::RuntimeProbe,
                    "data/logs/completed.jsonl".into(),
                    1,
                    "a".repeat(64),
                    300,
                    None,
                );
                Ok(GuiTaskOutput::new(
                    result,
                    Some(Ok(super::GuiDiagnosticSnapshot {
                        items: vec![GuiDiagnosticItem::new(
                            "最新日志".into(),
                            "日志 运行态探针".into(),
                            None,
                            None,
                            DiagnosticArtifactRef::from_log(&log),
                            log.timestamp_unix_millis(),
                            log.sequence(),
                        )],
                        warnings: Vec::new(),
                        failed_sources: Vec::new(),
                    })),
                ))
            });
            let mut controller = GuiController::new(factory);
            controller.start_initialization(|| {}).unwrap();
            drain_until_terminal(&mut controller);
            controller.start(action, || {}).unwrap();
            drain_until_terminal(&mut controller);
            assert_eq!(controller.view().selected_instance(), Some("1"));
            assert_eq!(controller.view().selected_workbook(), Some("plan.xlsx"));
            assert_eq!(
                controller.view().selected_diagnostic().unwrap().label(),
                "最新日志"
            );
            assert_eq!(
                controller.view().status(),
                if fails {
                    "操作失败"
                } else {
                    "操作完成"
                }
            );
            if fails {
                assert!(
                    controller
                        .view()
                        .last_failure_detail()
                        .unwrap()
                        .contains("original-error")
                );
            }
        }
    }
}

#[test]
fn diagnostic_refresh_failure_keeps_the_original_operation_error() {
    let factory = GuiTaskFactory::from_handler(|operation, _, _| {
        if matches!(operation, GuiOperation::Initialize) {
            return Ok(delivered(ready_output("ready")));
        }
        Ok(GuiTaskOutput::new(
            Err(TaskFailure::new("业务失败", "original-error")),
            Some(Err(TaskFailure::new("日志读取失败", "catalog-error"))),
        ))
    });
    let mut controller = GuiController::new(factory);
    controller.start_initialization(|| {}).unwrap();
    drain_until_terminal(&mut controller);
    controller
        .start(GuiAction::SynchronizeAndGenerate, || {})
        .unwrap();
    drain_until_terminal(&mut controller);
    assert_eq!(controller.view().status(), "操作失败");
    let view = controller.view();
    let detail = view.last_failure_detail().unwrap();
    assert!(detail.contains("original-error"));
    assert!(detail.contains("catalog-error"));
}

use super::*;
use std::cell::Cell;

fn manager(path: &Path) -> ResolvedManager {
    ResolvedManager {
        executable: path.into(),
        instances: BTreeMap::new(),
    }
}
fn options(hint: Option<&Path>, strict: bool) -> DiscoveryRequest<'_> {
    DiscoveryRequest {
        manager_hint: hint,
        include_running: true,
        strict_hint: strict,
    }
}
fn failure() -> EmulatorError {
    EmulatorError::Discovery {
        message: "查询失败样本".into(),
    }
}

#[test]
fn preferred_installation_does_not_hide_other_providers() {
    let hint = Path::new("MuMuManager.exe");
    let mut calls = Vec::new();
    let report = discover_with(
        &options(Some(hint), false),
        || {
            (
                vec![hint.into(), "ldconsole.exe".into(), "LDCONSOLE.EXE".into()],
                vec![],
            )
        },
        || DISCOVERY_QUERY_TIMEOUT,
        |path, timeout| {
            calls.push(path.to_path_buf());
            assert_eq!(timeout, INSTANCE_QUERY_TIMEOUT);
            Ok((manager(path), vec![]))
        },
    )
    .unwrap();
    assert_eq!(report.managers.len(), 2);
    assert_eq!(calls, [hint.to_path_buf(), PathBuf::from("ldconsole.exe")]);
}

#[test]
fn strict_hint_queries_only_the_bound_installation() {
    let report = discover_with(
        &options(Some(Path::new("bound.exe")), true),
        || panic!("严格绑定时不枚举其他安装"),
        || INSTANCE_QUERY_TIMEOUT,
        |path, _| Ok((manager(path), vec!["元数据降级".into()])),
    )
    .unwrap();
    assert_eq!(report.managers.len(), 1);
    assert_eq!(report.warnings, ["元数据降级"]);
    assert!(
        discover_with(
            &options(Some(Path::new("missing.exe")), true),
            || panic!("绑定失败不切换目标"),
            || INSTANCE_QUERY_TIMEOUT,
            |_, _| Err(failure())
        )
        .is_err()
    );
}

#[test]
fn partial_failures_and_provider_warnings_survive_success() {
    let report = discover_with(
        &options(Some(Path::new("missing.exe")), false),
        || {
            (
                vec!["ready.exe".into(), "failed.exe".into()],
                vec!["注册表读取失败".into()],
            )
        },
        || DISCOVERY_QUERY_TIMEOUT,
        |path, _| {
            if path == Path::new("ready.exe") {
                Ok((manager(path), vec!["配置文件降级".into()]))
            } else {
                Err(failure())
            }
        },
    )
    .unwrap();
    assert_eq!(report.managers.len(), 1);
    assert_eq!(report.warnings.len(), 4);
    assert!(report.warnings[0].contains("missing.exe"));
    assert_eq!(report.warnings[1], "注册表读取失败");
    assert_eq!(report.warnings[2], "配置文件降级");
    assert!(report.warnings[3].contains("failed.exe"));
}

#[test]
fn total_budget_stops_queries_and_preserves_verified_candidates() {
    let remaining = Cell::new(Duration::from_secs(7));
    let mut budgets = Vec::new();
    let report = discover_with(
        &options(None, false),
        || {
            (
                vec!["first.exe".into(), "second.exe".into(), "third.exe".into()],
                vec![],
            )
        },
        || remaining.get(),
        |path, timeout| {
            budgets.push(timeout);
            remaining.set(remaining.get().saturating_sub(timeout));
            Ok((manager(path), vec![]))
        },
    )
    .unwrap();
    assert_eq!(budgets, [Duration::from_secs(5), Duration::from_secs(2)]);
    assert_eq!(report.managers.len(), 2);
    assert!(report.warnings[0].contains("预算已耗尽"));
}

#[test]
fn all_failures_report_all_candidates_and_exhausted_budget_skips_execution() {
    let error = discover_with(
        &options(None, false),
        || (vec!["first.exe".into(), "second.exe".into()], vec![]),
        || INSTANCE_QUERY_TIMEOUT,
        |_, _| Err(failure()),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("first.exe"));
    assert!(error.to_string().contains("second.exe"));
    assert!(
        discover_with(
            &options(None, false),
            || (vec!["first.exe".into()], vec![]),
            || Duration::ZERO,
            |_, _| panic!("预算耗尽后不执行命令")
        )
        .is_err()
    );
}

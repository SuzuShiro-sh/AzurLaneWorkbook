//! MuMu12 安装布局和实例协议的离线测试。
use super::super::discovery::install_root_from_uninstall_command;
use super::super::{EmulatorInstance, TargetState};
use super::instances::parse_instances;
use super::{manager_candidate_from_process_image, manager_candidates_from_install_root};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const TWO_INSTANCES: &str = r#"{
            "0": {
                "adb_host_ip": "127.0.0.1",
                "adb_port": 16384,
                "android_version": "12.0",
                "error_code": 0,
                "index": "0",
                "is_android_started": true,
                "is_process_started": true,
                "name": "MuMu安卓设备",
                "player_state": "start_finished",
                "ignored_vendor_field": "allowed"
            },
            "1": {
                "android_version": "12.0",
                "error_code": 0,
                "index": "1",
                "is_android_started": false,
                "is_process_started": false,
                "name": "MuMu安卓设备-1"
            }
        }"#;
const INSTANCES_WITHOUT_ANDROID_VERSION: &str = r#"{
            "0": {
                "adb_host_ip": "127.0.0.1",
                "adb_port": 16384,
                "created_timestamp": 1757767736231497,
                "error_code": 0,
                "index": "0",
                "is_android_started": true,
                "is_process_started": true,
                "name": "MuMu安卓设备"
            },
            "1": {
                "created_timestamp": 1761920876078264,
                "error_code": 0,
                "index": "1",
                "is_android_started": false,
                "is_process_started": false,
                "name": "MuMu模拟器12-1"
            }
        }"#;
/// 安装版本变化不影响产品候选识别，无关产品不会匹配。
#[test]
fn mumu_registry_contract_matches_products_independently_of_version() {
    for name in [
        "MuMuPlayer",
        "MuMuPlayer-16.2",
        "MuMuPlayerGlobal-17.0",
        "MuMu Player 18.1",
        "YXArkNights-16.0",
    ] {
        assert!(super::is_mumu_uninstall_name(name));
    }
    for name in ["OtherPlayer-16.0", "MuMuPlayerHelper", "NotMuMuPlayer"] {
        assert!(!super::is_mumu_uninstall_name(name));
    }
}
/// 安装根同时生成新版与旧版管理器候选，后续仍需执行 version 验真。
#[test]
fn install_root_yields_bounded_manager_candidates() {
    assert_eq!(
        manager_candidates_from_install_root(Path::new("D:/MuMu")),
        [
            PathBuf::from("D:/MuMu/nx_main/MuMuManager.exe"),
            PathBuf::from("D:/MuMu/shell/MuMuManager.exe"),
        ]
    );
}
/// 卸载登记只接受明确的 MuMu uninstall.exe，不从 MSI 或不完整命令猜路径。
#[test]
fn uninstall_command_recovers_only_explicit_install_roots() {
    assert_eq!(
        install_root_from_uninstall_command(
            r#""D:/Program Files/MuMu Player 12/uninstall.exe" --from-control-panel"#
        ),
        Some(PathBuf::from("D:/Program Files/MuMu Player 12"))
    );
    assert_eq!(
        install_root_from_uninstall_command("D:/MuMu/uninstall.exe --silent"),
        Some(PathBuf::from("D:/MuMu"))
    );
    assert_eq!(
        install_root_from_uninstall_command(r#""D:/MuMu/nx_device/12.0/uninstall.exe" --silent"#),
        Some(PathBuf::from("D:/MuMu"))
    );
    assert_eq!(
        install_root_from_uninstall_command(r#""D:/MuMu/nx_device/latest/uninstall.exe" --silent"#),
        Some(PathBuf::from("D:/MuMu"))
    );
    assert_eq!(
        install_root_from_uninstall_command("MsiExec.exe /X{PRODUCT-CODE}"),
        None
    );
    assert_eq!(install_root_from_uninstall_command(""), None);
}
/// MuMu 新旧主进程及管理器自身都能回推唯一的管理器候选。
#[test]
fn known_mumu_process_images_recover_manager_candidates() {
    assert_eq!(
        manager_candidate_from_process_image(Path::new("D:/MuMu/nx_main/MuMuManager.exe")),
        Some(PathBuf::from("D:/MuMu/nx_main/MuMuManager.exe"))
    );
    assert_eq!(
        manager_candidate_from_process_image(Path::new("D:/MuMu/nx_main/MuMuNxMain.exe")),
        Some(PathBuf::from("D:/MuMu/nx_main/MuMuManager.exe"))
    );
    assert_eq!(
        manager_candidate_from_process_image(Path::new(
            "D:/MuMu/nx_device/12.0/shell/MuMuNxDevice.exe"
        )),
        Some(PathBuf::from("D:/MuMu/nx_main/MuMuManager.exe"))
    );
    assert_eq!(
        manager_candidate_from_process_image(Path::new("D:/MuMu/shell/MuMuPlayer.exe")),
        Some(PathBuf::from("D:/MuMu/shell/MuMuManager.exe"))
    );
    assert_eq!(
        manager_candidate_from_process_image(Path::new("D:/Other/Player.exe")),
        None
    );
    assert_eq!(
        manager_candidate_from_process_image(Path::new(
            "D:/Other/random/12.0/shell/MuMuNxDevice.exe"
        )),
        None
    );
    assert_eq!(
        manager_candidate_from_process_image(Path::new(
            "D:/MuMu/nx_device/latest/shell/MuMuNxDevice.exe"
        )),
        Some(PathBuf::from("D:/MuMu/nx_main/MuMuManager.exe"))
    );
}
/// 厂商实例映射允许新增无关字段，但必须保持键与内部索引一致。
#[test]
fn manager_parser_accepts_actual_shape_and_rejects_mismatched_index() {
    let instances: BTreeMap<String, EmulatorInstance> = parse_instances(TWO_INSTANCES).unwrap();
    assert_eq!(instances.len(), 2);
    let mismatched: String = TWO_INSTANCES.replacen("\"index\": \"0\"", "\"index\": \"9\"", 1);
    assert!(parse_instances(&mismatched).is_err());
}
/// 管理器省略 Android 版本和 player_state 时仍保留实例和真实运行状态。
#[test]
fn manager_parser_accepts_instances_without_android_version() {
    let instances = parse_instances(INSTANCES_WITHOUT_ANDROID_VERSION).unwrap();
    let running = instances.get("0").unwrap();
    assert_eq!(running.android_version, None);

    assert_eq!(running.android_version_label(), "未报告");
    assert_eq!(running.catalog_state(), TargetState::Ready);

    let stopped = instances.get("1").unwrap();
    assert_eq!(stopped.android_version, None);
    assert_eq!(stopped.catalog_state(), TargetState::Stopped);

    assert!(running.is_ready());
}
/// 厂商状态文案变化不影响实际进程和 Android 就绪能力。
#[test]
fn manager_readiness_accepts_unfamiliar_state_text() {
    let updated = TWO_INSTANCES.replace("start_finished", "running");
    let instances = parse_instances(&updated).unwrap();
    assert!(instances["0"].is_ready());
}
#[test]
fn manager_display_labels_do_not_block_usable_instances() {
    let updated = TWO_INSTANCES
        .replacen("MuMu安卓设备", &format!(" {} ", "设备".repeat(100)), 1)
        .replacen(
            "\"android_version\": \"12.0\"",
            &format!("\"android_version\": \"Android {}\"", "build".repeat(20)),
            1,
        );
    let instances = parse_instances(&updated).unwrap();
    assert!(instances["0"].is_ready());
}
/// Android 版本只作为有界展示文本，不再要求固定的数字点号格式。
#[test]
fn manager_parser_rejects_unsafe_display_fields() {
    let control_name = TWO_INSTANCES.replacen(
        "\"name\": \"MuMu安卓设备\"",
        "\"name\": \"MuMu\\n安卓设备\"",
        1,
    );
    assert!(parse_instances(&control_name).is_err());

    let descriptive_version = TWO_INSTANCES.replacen(
        "\"android_version\": \"12.0\"",
        "\"android_version\": \"Android 12\"",
        1,
    );
    assert!(parse_instances(&descriptive_version).is_ok());

    let invalid_version = TWO_INSTANCES.replacen(
        "\"android_version\": \"12.0\"",
        "\"android_version\": \"Android\\n12\"",
        1,
    );
    assert!(parse_instances(&invalid_version).is_err());
}

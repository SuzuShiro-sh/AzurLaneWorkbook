//! 显式运行的模拟器实机验收，不参与离线测试。
use std::path::PathBuf;
use suzushiro_emulator::adapter_for_manager;

#[test]
#[ignore = "通过 AZLW_EMULATOR_MANAGER 指定已安装管理器，只读查询实例和端点归属"]
fn installed_adapter_passes_read_only_contract() {
    let manager =
        PathBuf::from(std::env::var_os("AZLW_EMULATOR_MANAGER").expect("AZLW_EMULATOR_MANAGER"));
    let adapter = adapter_for_manager(&manager).unwrap();
    let instances = adapter.instances(&manager).unwrap();
    assert!(!instances.is_empty());
    for instance in instances.values() {
        println!(
            "provider={} index={} name={} ready={} endpoint={:?}",
            adapter.id(),
            instance.index,
            instance.name,
            instance.is_ready(),
            instance.serial()
        );
    }
    assert!(
        instances.values().any(|instance| instance.is_ready()),
        "需要至少一个已启动且端点归属正确的实例"
    );
}

#[test]
#[ignore = "需要 AZLW_EMULATOR_MANAGER、AZLW_TOOL_ROOT，只读检查 ADB/root/ABI，不加载 Agent"]
fn installed_adapter_checks_adb_prerequisites() {
    use suzushiro_adb::OwnedAdbServer;
    let manager =
        PathBuf::from(std::env::var_os("AZLW_EMULATOR_MANAGER").expect("AZLW_EMULATOR_MANAGER"));
    let root = PathBuf::from(std::env::var_os("AZLW_TOOL_ROOT").expect("AZLW_TOOL_ROOT"));
    let adapter = adapter_for_manager(&manager).unwrap();
    let instances = adapter.instances(&manager).unwrap();
    let index = std::env::var("AZLW_EMULATOR_INDEX").expect("AZLW_EMULATOR_INDEX");
    let instance = instances.get(&index).expect("所选实例存在");
    assert!(instance.is_ready());
    let bundle = crate::adapters::device::adb_config::load_adb_bundle(&root, None).unwrap();
    let mut server = OwnedAdbServer::start(bundle, instance.serial().unwrap()).unwrap();
    server.connect_target().unwrap();
    for command in [
        "getprop ro.product.cpu.abi",
        "getprop ro.product.cpu.abilist",
        "getprop ro.build.version.release",
        "uname -r",
        "grep -m 1 flags /proc/cpuinfo",
        "pidof com.bilibili.azurlane",
    ] {
        let result = server.run_target_checked(
            &["shell".to_owned(), command.to_owned()],
            "test.adb_prerequisite",
        );
        println!("{command}: {result:?}");
    }
    let receipt = suzushiro_emulator::transport::RootShell {
        adapter,
        manager: manager.to_str().unwrap(),
        index: &index,
    }
    .run(
        "id",
        "test.root_prerequisite",
        None,
        |arguments, stage, _| {
            server
                .run_target_checked(arguments, stage)
                .map_err(|error| error.to_string())
        },
        |error| error.to_string(),
    );
    let receipt = receipt.unwrap();
    println!(
        "root_exit={} identity={}",
        receipt.exit_code,
        receipt.stdout.trim()
    );
    let shutdown = server.shutdown();
    assert!(shutdown.is_ok(), "{shutdown:?}");
    assert_eq!(receipt.exit_code, 0);
    assert!(receipt.stdout.contains("uid=0(root)"));
}

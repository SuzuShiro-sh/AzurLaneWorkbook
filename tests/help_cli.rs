//! 帮助必须在独立程序目录内直接运行，不能依赖配置、解包或游戏连接。

use std::{fs, process::Command};

#[test]
fn standalone_help_lists_commands_and_has_no_initialization_side_effects() {
    let root = std::env::temp_dir().join(format!("azlw-help-{}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let binary = root.join("AzurLaneWorkbook.exe");
    fs::copy(env!("CARGO_BIN_EXE_AzurLaneWorkbook"), &binary).unwrap();
    for arguments in [
        vec!["--help"],
        vec!["-h"],
        vec!["help"],
        vec!["ships", "--help"],
        vec!["catalog", "ships", "--help"],
        vec!["catalog", "equipment", "--help"],
        vec!["catalog", "skills", "--help"],
        vec!["recipes", "--help"],
        vec!["items", "--help"],
        vec!["resources", "--help"],
        vec!["fleets", "--help"],
        vec!["technology", "--help"],
        vec!["compose", "--help"],
        vec!["history", "--help"],
        vec!["logs", "--help"],
        vec!["help", "equipment-actions"],
        vec!["--instance", "example", "equip", "--help"],
    ] {
        let output = Command::new(&binary)
            .args(&arguments)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("用法"));
        if arguments == ["--help"] {
            for command in [
                "ships",
                "catalog",
                "recipes",
                "items",
                "resources",
                "fleets",
                "technology",
                "compose",
                "equipment",
                "equip",
                "unequip",
                "enhance",
                "dismantle",
                "equipment-actions",
                "agent",
                "instances",
                "generate",
                "check",
                "check-save",
                "execute",
                "open",
                "workbooks",
                "history",
                "logs",
                "settings",
                "update-acquisition",
                "doctor",
                "verify-release",
                "layout-check",
                "layout-upgrade",
                "layout-preview",
            ] {
                assert!(
                    text.lines()
                        .any(|line| line.split_whitespace().next() == Some(command)),
                    "缺少 {command}"
                );
            }
        }
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "帮助不应创建资源或日志"
        );
    }
    let output = Command::new(&binary)
        .args(["help", "unknown-command"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("未知帮助主题")
    );
    fs::remove_dir_all(root).unwrap();
}

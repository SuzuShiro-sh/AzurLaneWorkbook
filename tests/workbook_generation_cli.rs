//! 通过复制后的正式可执行文件验证工作簿生成命令的参数和未就绪边界。

use std::fs;
use std::process::{Command, Output};

mod common;

use common::TestDirectory;

#[test]
fn reports_a_stable_not_ready_error_without_creating_a_workbook() {
    let fixture = TestDirectory::new("azlw-workbook-generation-cli-tests", "default-name");
    let executable = common::prepare_install(fixture.path());
    let working_directory = fixture.path().join("unrelated-working-directory");
    fs::create_dir(&working_directory).unwrap();

    let output = Command::new(executable)
        .arg("generate")
        .current_dir(working_directory)
        .output()
        .unwrap();

    assert_not_ready(&output);
    assert!(
        !common::resource_root(fixture.path())
            .join("data/workbooks")
            .exists()
    );
}

#[test]
fn accepts_one_requested_name_but_does_not_publish_without_runtime_state() {
    let fixture = TestDirectory::new("azlw-workbook-generation-cli-tests", "requested-name");
    let executable = common::prepare_install(fixture.path());

    let output = Command::new(executable)
        .args(["generate", "fleet-plan.xlsx"])
        .current_dir(fixture.path())
        .output()
        .unwrap();

    assert_not_ready(&output);
    assert!(
        !common::resource_root(fixture.path())
            .join("data/workbooks")
            .exists()
    );
}

fn assert_not_ready(output: &Output) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr.clone()).unwrap();
    let expected: &[&str] = if cfg!(target_os = "windows") {
        &[
            "工作簿生成失败 [RUNTIME_BOOTSTRAP_FAILED]",
            "阶段：game.bootstrap.adb_bundle",
            "游戏只读运行态连接未能建立",
            "修复建议：",
            "本次未发布工作簿",
        ]
    } else {
        &[
            "工作簿生成失败 [GAME_NOT_READY]",
            "阶段：workbook.generate",
            "工作簿生成需要已认证的游戏运行态连接",
            "缺少端口：game",
            "原因：应用服务未配置 game 端口",
            "修复建议：",
            "没有读取游戏状态或写入工作簿",
        ]
    };
    for expected in expected {
        assert!(error.contains(expected), "诊断缺少 {expected:?}: {error}");
    }
}

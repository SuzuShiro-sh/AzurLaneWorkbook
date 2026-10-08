//! 定义 Native 测试执行器的公共选项、报告证据和错误契约。

use std::path::PathBuf;

use serde::Serialize;
use suzushiro_session_core::SessionId;
use thiserror::Error;

/// 声明固定工具、受控构建目录、ADB 工具根和唯一设备目标。
#[derive(Clone, Debug)]
pub struct NativeTestRunnerOptions {
    pub(super) repository_root: PathBuf,
    pub(super) tool_root: PathBuf,
    pub(super) toolchain: NativeTestToolchain,
    pub(super) build_relative: PathBuf,
    pub(super) serial: String,
}

/// 汇集一次 Native 构建所需的固定 CMake、CTest、Ninja 与 NDK 文件。
#[derive(Clone, Debug)]
pub struct NativeTestToolchain {
    pub(super) cmake_executable: PathBuf,
    pub(super) ctest_executable: PathBuf,
    pub(super) ninja_executable: PathBuf,
    pub(super) ndk_toolchain: PathBuf,
}

impl NativeTestToolchain {
    /// 构造由依赖锁解析出的完整 Native 构建工具链。
    pub fn new(
        cmake_executable: impl Into<PathBuf>,
        ctest_executable: impl Into<PathBuf>,
        ninja_executable: impl Into<PathBuf>,
        ndk_toolchain: impl Into<PathBuf>,
    ) -> Self {
        Self {
            cmake_executable: cmake_executable.into(),
            ctest_executable: ctest_executable.into(),
            ninja_executable: ninja_executable.into(),
            ndk_toolchain: ndk_toolchain.into(),
        }
    }
}

impl NativeTestRunnerOptions {
    /// 构造完整执行选项；所有路径和目标约束在运行前统一验证。
    pub fn new(
        repository_root: impl Into<PathBuf>,
        tool_root: impl Into<PathBuf>,
        toolchain: NativeTestToolchain,
        build_relative: impl Into<PathBuf>,
        serial: impl Into<String>,
    ) -> Self {
        Self {
            repository_root: repository_root.into(),
            tool_root: tool_root.into(),
            toolchain,
            build_relative: build_relative.into(),
            serial: serial.into(),
        }
    }
}

/// 单个构建工具命令形成的可重算输出摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestHostCommandEvidence {
    pub(super) stage: &'static str,
    pub(super) exit_code: i32,
    pub(super) stdout_sha256: String,
    pub(super) stderr_sha256: String,
}

/// CTest 清单中参与推送的单个 ELF 产物证据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestArtifactEvidence {
    pub(super) filename: String,
    pub(super) size_bytes: u64,
    pub(super) sha256: String,
    pub(super) device_sha256_verified: bool,
}

/// 单个测试输出文件的相对路径、大小和内容摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestOutputEvidence {
    pub(super) path: PathBuf,
    pub(super) size_bytes: u64,
    pub(super) sha256: String,
}

/// 单个远端测试进程的身份与定向终止结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestRemoteProcessEvidence {
    pub(super) pid: u32,
    pub(super) start_time: u64,
    pub(super) stopped: bool,
    pub(super) cleanup_action: String,
}

/// 单个设备测试的退出状态、输出证据、远端进程身份和耗时。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestCaseReport {
    pub(super) name: String,
    pub(super) status: &'static str,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: u64,
    pub(super) stdout: Option<NativeTestOutputEvidence>,
    pub(super) stderr: Option<NativeTestOutputEvidence>,
    pub(super) remote_process: Option<NativeTestRemoteProcessEvidence>,
    pub(super) error: Option<String>,
}

/// 自有 ADB 服务和唯一远端目录的清理证据。
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct NativeTestCleanupEvidence {
    pub(super) remote_processes_stopped: bool,
    pub(super) remote_directory_removed: bool,
    pub(super) adb_process_stopped: bool,
    pub(super) adb_port_released: bool,
    pub(super) adb_temporary_root_removed: bool,
}

/// 单个基础设施或清理失败的阶段与完整诊断。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestFailureEvidence {
    pub(super) stage: String,
    pub(super) message: String,
}

/// 一次 Native 实机执行的完整结构化报告。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestRunReport {
    pub(super) schema_version: u32,
    pub(super) status: &'static str,
    pub(super) session_id: SessionId,
    pub(super) serial: String,
    pub(super) android_boot_id: Option<String>,
    pub(super) android_api: Option<u32>,
    pub(super) android_abi: Option<String>,
    pub(super) adb_revision: Option<String>,
    pub(super) adb_server_port: Option<u16>,
    pub(super) adb_server_process_id: Option<u32>,
    pub(super) adb_server_log: Option<PathBuf>,
    pub(super) remote_directory: String,
    pub(super) build_directory: PathBuf,
    pub(super) inventory_sha256: String,
    pub(super) configured_test_count: usize,
    pub(super) passed_test_count: usize,
    pub(super) failed_test_count: usize,
    pub(super) infra_failed_test_count: usize,
    pub(super) blocked_test_count: usize,
    pub(super) duration_ms: u64,
    pub(super) host_commands: Vec<NativeTestHostCommandEvidence>,
    pub(super) artifacts: Vec<NativeTestArtifactEvidence>,
    pub(super) tests: Vec<NativeTestCaseReport>,
    pub(super) cleanup: NativeTestCleanupEvidence,
    pub(super) failures: Vec<NativeTestFailureEvidence>,
}

impl NativeTestRunReport {
    /// 只有当前清单全部测试通过且清理证据完整时才形成成功报告。
    pub fn passed(&self) -> bool {
        self.status == "passed" && self.results_complete()
    }

    pub(super) fn results_complete(&self) -> bool {
        self.configured_test_count > 0
            && self.tests.len() == self.configured_test_count
            && self
                .tests
                .iter()
                .all(|test| test.status == "passed" && test.exit_code == Some(0))
            && self.passed_test_count == self.configured_test_count
            && self.failed_test_count == 0
            && self.infra_failed_test_count == 0
            && self.blocked_test_count == 0
            && self
                .artifacts
                .iter()
                .all(|artifact| artifact.device_sha256_verified)
            && self.cleanup.remote_processes_stopped
            && self.cleanup.remote_directory_removed
            && self.cleanup.adb_process_stopped
            && self.cleanup.adb_port_released
            && self.cleanup.adb_temporary_root_removed
            && self.failures.is_empty()
    }
}

/// 已发布报告的路径与摘要，供命令行入口输出稳定收据。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct NativeTestRunOutcome {
    pub report: NativeTestRunReport,
    pub report_path: PathBuf,
    pub report_sha256: String,
    pub report_size_bytes: u64,
}

/// 构建、清单、设备执行或双重清理没有形成完整证据。
#[derive(Debug, Error)]
pub enum NativeTestRunnerError {
    #[error("Native 实机测试执行器只支持原生 Windows")]
    UnsupportedHost,
    #[error("Native 实机测试选项无效: {message}")]
    InvalidOptions { message: String },
    #[error("Native 实机测试路径无效: {message}")]
    InvalidPath { message: String },
    #[error("主机命令阶段 {stage} 失败: {message}")]
    HostCommand {
        stage: &'static str,
        message: String,
    },
    #[error("CTest 清单无效: {message}")]
    InvalidInventory { message: String },
    #[error("Native ELF {path} 无效: {message}")]
    InvalidElf { path: PathBuf, message: String },
    #[error("设备阶段 {stage} 失败: {message}")]
    Device {
        stage: &'static str,
        message: String,
    },
    #[error("读取 {path} 失败: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("发布 Native 测试报告失败: {message}")]
    Report { message: String },
}

impl NativeTestRunnerError {
    #[cfg(target_os = "windows")]
    pub(super) fn stage(&self) -> &'static str {
        match self {
            Self::UnsupportedHost => "native.unsupported_host",
            Self::InvalidOptions { .. } => "native.options",
            Self::InvalidPath { .. } | Self::Io { .. } => "native.path",
            Self::HostCommand { stage, .. } | Self::Device { stage, .. } => stage,
            Self::InvalidInventory { .. } => "native.inventory",
            Self::InvalidElf { .. } => "native.elf",
            Self::Report { .. } => "native.report",
        }
    }
}

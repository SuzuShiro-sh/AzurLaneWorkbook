//! 定义离线 doctor 命令使用的稳定、脱敏报告模型。

use serde::Serialize;

/// 离线 doctor 报告的版本号。
pub const DOCTOR_SCHEMA_VERSION: u32 = 1;

/// 本地文件校验成功，但没有建立运行态会话的总体状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    OfflineReady,
}

/// 单个离线文件校验的结果。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCheckStatus {
    Ready,
}

/// doctor 没有执行的运行态或写入能力。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCapabilityStatus {
    Unavailable,
}

/// 离线 doctor 的完整结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub schema_version: u32,
    pub status: DoctorStatus,
    pub checks: DoctorChecks,
    pub capabilities: DoctorCapabilities,
}

/// 离线 doctor 读取设置、布局和发布清单的来源。不建立游戏会话。
pub(crate) trait OfflineDoctorSources {
    fn settings_check(&self) -> Result<DoctorSettingsCheck, crate::application::AppError>;
    fn layout_check(&self) -> Result<DoctorLayoutCheck, crate::application::AppError>;
    fn release_check(&self) -> Result<DoctorReleaseCheck, crate::application::AppError>;
}

/// 按固定顺序执行离线 doctor。
pub struct OfflineDoctor {
    sources: Box<dyn OfflineDoctorSources>,
}

impl OfflineDoctor {
    pub(crate) fn new(sources: Box<dyn OfflineDoctorSources>) -> Self {
        Self { sources }
    }

    /// 返回脱敏报告。任一项失败时不继续后面的检查。
    pub fn diagnose(&self) -> Result<DoctorReport, crate::application::AppError> {
        Ok(DoctorReport::new(
            self.sources.settings_check()?,
            self.sources.layout_check()?,
            self.sources.release_check()?,
        ))
    }
}

impl DoctorReport {
    /// 使用已经通过校验的文件证据建立脱敏报告。
    pub fn new(
        settings: DoctorSettingsCheck,
        layout: DoctorLayoutCheck,
        release: DoctorReleaseCheck,
    ) -> Self {
        Self {
            schema_version: DOCTOR_SCHEMA_VERSION,
            status: DoctorStatus::OfflineReady,
            checks: DoctorChecks {
                tool_root: DoctorCheck::ready(),
                settings,
                layout,
                release,
            },
            capabilities: DoctorCapabilities::offline(),
        }
    }
}

/// doctor 逐项文件和目录校验结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorChecks {
    pub tool_root: DoctorCheck,
    pub settings: DoctorSettingsCheck,
    pub layout: DoctorLayoutCheck,
    pub release: DoctorReleaseCheck,
}

/// 工具根目录校验结果，不输出绝对路径或链接目标。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub status: DoctorCheckStatus,
}

impl DoctorCheck {
    const fn ready() -> Self {
        Self {
            status: DoctorCheckStatus::Ready,
        }
    }
}

/// 设置文件的脱敏校验摘要，不包含地址、包名或路径原文。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorSettingsCheck {
    pub status: DoctorCheckStatus,
    pub device_mode: &'static str,
    pub adb_path_configured: bool,
    pub serial_configured: bool,
    pub instance_configured: bool,
    pub game_package_configured: bool,
    pub connect_timeout_seconds: u32,
    pub startup_timeout_seconds: u32,
}

impl DoctorSettingsCheck {
    /// 建立不泄露设置正文的校验摘要。
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        device_mode: &'static str,
        adb_path_configured: bool,
        serial_configured: bool,
        instance_configured: bool,
        game_package_configured: bool,
        connect_timeout_seconds: u32,
        startup_timeout_seconds: u32,
    ) -> Self {
        Self {
            status: DoctorCheckStatus::Ready,
            device_mode,
            adb_path_configured,
            serial_configured,
            instance_configured,
            game_package_configured,
            connect_timeout_seconds,
            startup_timeout_seconds,
        }
    }
}

/// 工作簿布局的结构和内容摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorLayoutCheck {
    pub status: DoctorCheckStatus,
    pub schema_version: u32,
    pub sheet_count: usize,
    pub field_count: usize,
    pub enum_option_count: usize,
    pub style_count: usize,
    pub content_sha256: String,
}

impl DoctorLayoutCheck {
    /// 建立已经通过注册表核对的布局摘要。
    pub fn new(
        schema_version: u32,
        sheet_count: usize,
        field_count: usize,
        enum_option_count: usize,
        style_count: usize,
        content_sha256: String,
    ) -> Self {
        Self {
            status: DoctorCheckStatus::Ready,
            schema_version,
            sheet_count,
            field_count,
            enum_option_count,
            style_count,
            content_sha256,
        }
    }
}

/// 发布清单和文件闭包的校验摘要。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReleaseCheck {
    pub status: DoctorCheckStatus,
    pub product_version: String,
    pub checked_files: usize,
    pub immutable_files: usize,
    pub modified_configurations: Vec<String>,
}

impl DoctorReleaseCheck {
    /// 建立发布清单校验摘要，并保留可供脚本定位的相对配置路径。
    pub fn new(
        product_version: String,
        checked_files: usize,
        immutable_files: usize,
        modified_configurations: Vec<String>,
    ) -> Self {
        Self {
            status: DoctorCheckStatus::Ready,
            product_version,
            checked_files,
            immutable_files,
            modified_configurations,
        }
    }
}

/// 离线 doctor 明确没有探测或执行的能力集合。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCapabilities {
    pub runtime_probe: DoctorCapability,
    pub write_operations: DoctorCapability,
}

impl DoctorCapabilities {
    const fn offline() -> Self {
        Self {
            runtime_probe: DoctorCapability {
                status: DoctorCapabilityStatus::Unavailable,
                reason_code: "offline_not_probed",
                message: "doctor 只读取本地文件，不启动模拟器、游戏或 ADB",
            },
            write_operations: DoctorCapability {
                status: DoctorCapabilityStatus::Unavailable,
                reason_code: "offline_not_executed",
                message: "doctor 不执行游戏写入，也不发布 data/** 文件",
            },
        }
    }
}

/// 单项运行态或写入能力的明确不可用原因。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCapability {
    pub status: DoctorCapabilityStatus,
    pub reason_code: &'static str,
    pub message: &'static str,
}

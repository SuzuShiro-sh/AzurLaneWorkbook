//! 把离线 doctor 的文件读取接到应用检查来源。

use std::path::{Path, PathBuf};

use crate::adapters::release::{ReleaseError, verify_release};
use crate::adapters::settings::{DeviceMode, Settings, SettingsError};
use crate::adapters::tool_root::{ToolRoot, ToolRootError};
use crate::adapters::workbook::load_workbook_layout;
use crate::application::{
    AppError, AppErrorCode, DoctorLayoutCheck, DoctorReleaseCheck, DoctorSettingsCheck,
    LayoutModelError, OfflineDoctor, OfflineDoctorSources, WorkbookProjectionV4,
};

pub(super) struct ToolRootDoctor {
    root: ToolRoot,
}

impl ToolRootDoctor {
    pub(super) fn open(path: &Path) -> Result<Self, ToolRootError> {
        Ok(Self {
            root: ToolRoot::open(path)?,
        })
    }
}

impl OfflineDoctorSources for ToolRootDoctor {
    fn settings_check(&self) -> Result<DoctorSettingsCheck, AppError> {
        let settings = Settings::load(self.root.as_path()).map_err(settings_error)?;
        let device = settings.device();
        Ok(DoctorSettingsCheck::new(
            match device.mode() {
                DeviceMode::Auto => "auto",
                DeviceMode::Manual => "manual",
            },
            !device.adb_path().is_empty(),
            !device.serial().is_empty(),
            !device.instance().is_empty(),
            !device.game_package().is_empty(),
            settings.runtime().connect_timeout_seconds(),
            settings.runtime().startup_timeout_seconds(),
        ))
    }

    fn layout_check(&self) -> Result<DoctorLayoutCheck, AppError> {
        let registry = WorkbookProjectionV4::layout_registry().map_err(registry_error)?;
        let layout_path: PathBuf = self
            .root
            .existing_file(Path::new("workbook-layout.xlsx"))
            .map_err(layout_path_error)?;
        let layout = load_workbook_layout(&layout_path, &registry)?;
        Ok(DoctorLayoutCheck::new(
            layout.schema_version(),
            layout.sheets().len(),
            layout.fields().len(),
            layout.enum_options().len(),
            layout.styles().len(),
            layout.content_sha256().to_owned(),
        ))
    }

    fn release_check(&self) -> Result<DoctorReleaseCheck, AppError> {
        let release = verify_release(self.root.as_path()).map_err(release_error)?;
        Ok(DoctorReleaseCheck::new(
            release.product_version,
            release.checked_files,
            release.immutable_files,
            release.modified_configurations,
        ))
    }
}

pub fn open_offline_doctor(path: &Path) -> Result<OfflineDoctor, ToolRootError> {
    Ok(OfflineDoctor::new(Box::new(ToolRootDoctor::open(path)?)))
}

fn settings_error(error: SettingsError) -> AppError {
    AppError::from_source(
        "doctor.settings",
        AppErrorCode::SettingsInvalid,
        "settings.json 未通过严格校验",
        std::io::Error::other(error.summary()),
    )
    .with_context("doctor_step", "settings")
    .with_context("detail", error.summary())
}

fn registry_error(error: LayoutModelError) -> AppError {
    AppError::from_source(
        "doctor.layout_registry",
        AppErrorCode::ApplicationInitializationFailed,
        "程序内置工作簿布局注册表无效",
        error,
    )
    .with_context("doctor_step", "registry")
}

fn layout_path_error(error: ToolRootError) -> AppError {
    let detail = match &error {
        ToolRootError::InvalidRelativePath { message, .. }
        | ToolRootError::UnsafePath { message, .. }
        | ToolRootError::PathConflict { message, .. } => message.clone(),
        ToolRootError::Io { operation, .. } => format!("{operation}失败"),
    };
    AppError::from_source(
        "doctor.layout",
        AppErrorCode::LayoutInvalid,
        "工具根目录缺少可读取的 workbook-layout.xlsx",
        error,
    )
    .with_context("doctor_step", "layout_path")
    .with_context("detail", detail)
}

fn release_error(error: ReleaseError) -> AppError {
    let code = match error.code() {
        "APPLICATION_INITIALIZATION_FAILED" => AppErrorCode::ApplicationInitializationFailed,
        "SETTINGS_INVALID" => AppErrorCode::SettingsInvalid,
        "LAYOUT_INVALID" => AppErrorCode::LayoutInvalid,
        "LAYOUT_UPGRADE_REQUIRED" => AppErrorCode::LayoutUpgradeRequired,
        "RUNTIME_INCOMPATIBLE" => AppErrorCode::RuntimeIncompatible,
        _ => AppErrorCode::ManifestInvalid,
    };
    let detail = match &error {
        ReleaseError::ToolRoot(_) => "工具根目录路径无效".to_owned(),
        ReleaseError::Io { stage, .. } => format!("{stage} 失败"),
        ReleaseError::Settings(source) => source.summary(),
        _ => error.to_string(),
    };
    let mut mapped = AppError::from_source(
        error.stage(),
        code,
        "发布清单、文件闭包或发布配置未通过离线校验",
        std::io::Error::other(detail.clone()),
    )
    .with_context("doctor_step", "release")
    .with_context("detail", detail);
    for (key, value) in error.context() {
        mapped = mapped.with_context(key, value);
    }
    mapped
}

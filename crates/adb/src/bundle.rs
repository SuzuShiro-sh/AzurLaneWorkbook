//! ADB 资源校验与受控状态目录。

#[cfg(target_os = "windows")]
use crate::environment::create_process_policy;
use crate::{AdbConfig, AdbLogSink, IsolatedAdbError};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use suzushiro_controlled_root::ControlledRoot;
#[cfg(target_os = "windows")]
use suzushiro_host_command::NativeCommandPolicy;
const ADB_EXECUTABLE_NAME: &str = "adb.exe";
const ADB_API_NAME: &str = "AdbWinApi.dll";
const ADB_NOTICE_NAME: &str = "NOTICE.txt";
const ADB_PROPERTIES_NAME: &str = "source.properties";
const MAX_PROPERTIES_BYTES: u64 = 4 * 1024;

/// 固定单目录发布中 ADB 的可执行文件、依赖、许可和状态路径。
#[derive(Clone, Debug)]
pub struct AdbBundle {
    pub(crate) tool_root: ControlledRoot,
    pub(crate) state_relative: PathBuf,
    pub(crate) log_sink: Arc<dyn AdbLogSink>,
    pub(crate) executable: PathBuf,
    pub(crate) api_library: PathBuf,
    pub(crate) notice: PathBuf,
    pub(crate) properties: PathBuf,
    pub(crate) revision: String,
    pub(crate) state_root: PathBuf,
    pub(crate) temporary_root: PathBuf,
    pub(crate) key_root: PathBuf,
    pub(crate) log_root: PathBuf,
    #[cfg(target_os = "windows")]
    pub(crate) process_policy: NativeCommandPolicy,
}

impl AdbBundle {
    /// 调用方明确指定资源、状态和日志边界；库不假设应用目录布局。
    pub fn load(
        config: AdbConfig,
        log_sink: Arc<dyn AdbLogSink>,
    ) -> Result<Self, IsolatedAdbError> {
        let tool_root = ControlledRoot::open(&config.root)?;
        let executable_relative_path = &config.executable;
        if !executable_relative_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(ADB_EXECUTABLE_NAME))
        {
            return Err(IsolatedAdbError::InvalidBundle {
                path: executable_relative_path.to_path_buf(),
                message: "ADB 可执行文件名必须是 adb.exe".to_owned(),
            });
        }
        let bundle_directory: &Path =
            executable_relative_path
                .parent()
                .ok_or_else(|| IsolatedAdbError::InvalidBundle {
                    path: executable_relative_path.to_path_buf(),
                    message: "adb.exe 缺少工具目录内的父目录".to_owned(),
                })?;
        let executable: PathBuf = tool_root.existing_file(executable_relative_path)?;
        let api_library: PathBuf = tool_root.existing_file(&bundle_directory.join(ADB_API_NAME))?;
        let notice: PathBuf = tool_root.existing_file(&bundle_directory.join(ADB_NOTICE_NAME))?;
        let properties: PathBuf =
            tool_root.existing_file(&bundle_directory.join(ADB_PROPERTIES_NAME))?;
        let properties_metadata: fs::Metadata =
            fs::metadata(&properties).map_err(|source| IsolatedAdbError::Io {
                stage: "adb.read_properties_metadata",
                path: properties.clone(),
                source,
            })?;
        if properties_metadata.len() > MAX_PROPERTIES_BYTES {
            return Err(IsolatedAdbError::InvalidBundle {
                path: properties,
                message: format!("source.properties 超过 {MAX_PROPERTIES_BYTES} 字节"),
            });
        }
        let properties_text: String =
            fs::read_to_string(&properties).map_err(|source| IsolatedAdbError::Io {
                stage: "adb.read_properties",
                path: properties.clone(),
                source,
            })?;
        let revision: String = parse_revision_at(&properties_text, &properties)?;

        let state_relative = config.state_directory;
        let state_root = tool_root.ensure_directory(&state_relative)?;
        let log_root = tool_root.ensure_directory(&config.log_directory)?;
        let bundle_root = executable.parent().expect("已校验 ADB 父目录");
        for (left, right) in [
            (&state_root as &Path, bundle_root),
            (&state_root, &log_root),
        ] {
            if paths_overlap(left, right) {
                return Err(IsolatedAdbError::InvalidBundle {
                    path: state_root.clone(),
                    message: "状态目录必须与 ADB 资源目录、日志目录相互独立".to_owned(),
                });
            }
        }
        let home_root = tool_root.ensure_directory(&state_relative.join("home"))?;
        let temporary_root = tool_root.ensure_directory(&state_relative.join("temp"))?;
        let key_root = tool_root.ensure_directory(&state_relative.join("keys"))?;
        let key_path = tool_root.as_path().join(state_relative.join("keys/adbkey"));
        #[cfg(target_os = "windows")]
        let process_policy: NativeCommandPolicy = {
            let roaming_root: PathBuf =
                tool_root.ensure_directory(&state_relative.join("home/AppData/Roaming"))?;
            let local_root: PathBuf =
                tool_root.ensure_directory(&state_relative.join("home/AppData/Local"))?;
            create_process_policy(
                &executable,
                &home_root,
                &roaming_root,
                &local_root,
                &temporary_root,
                &key_path,
            )?
        };

        Ok(Self {
            tool_root,
            state_relative,
            log_sink,
            executable,
            api_library,
            notice,
            properties,
            revision,
            state_root,
            temporary_root,
            key_root,
            log_root,
            #[cfg(target_os = "windows")]
            process_policy,
        })
    }

    /// 返回 Platform-Tools 自带属性文件声明的修订号。
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// 返回规范化后的工具根目录，供发布文件证据转换为相对路径。
    pub fn tool_root(&self) -> &Path {
        self.tool_root.as_path()
    }

    /// 返回独立 ADB 可写状态的规范工具内根目录。
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// 返回固定随包 ADB 可执行文件。
    #[cfg(target_os = "windows")]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// 返回随包 ADB 服务端、密钥生成器和客户端共用的最小进程边界。
    #[cfg(target_os = "windows")]
    pub fn process_policy(&self) -> &NativeCommandPolicy {
        &self.process_policy
    }

    /// 返回组成 TCP 目标发布闭包的四个固定文件，供收据摘要和清单核对。
    pub fn files(&self) -> [&Path; 4] {
        [
            &self.executable,
            &self.api_library,
            &self.notice,
            &self.properties,
        ]
    }
}

/// 提取唯一的 Platform-Tools 十进制修订号。
pub fn parse_revision(properties: &str) -> Result<String, IsolatedAdbError> {
    parse_revision_at(properties, Path::new("source.properties"))
}

/// 严格提取版本并把失败绑定到实际读取的属性文件。
fn parse_revision_at(properties: &str, properties_path: &Path) -> Result<String, IsolatedAdbError> {
    let revisions: Vec<&str> = properties
        .lines()
        .filter_map(|line: &str| line.trim().strip_prefix("Pkg.Revision="))
        .collect();
    if revisions.len() != 1 {
        return Err(IsolatedAdbError::InvalidBundle {
            path: properties_path.to_path_buf(),
            message: "source.properties 必须包含唯一 Pkg.Revision".to_owned(),
        });
    }
    let revision: &str = revisions[0];
    if revision.is_empty()
        || revision.len() > 32
        || !revision.split('.').all(|segment: &str| {
            !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(IsolatedAdbError::InvalidBundle {
            path: properties_path.to_path_buf(),
            message: "Pkg.Revision 不是受限的十进制版本".to_owned(),
        });
    }
    Ok(revision.to_owned())
}

/// Windows 路径的大小写差异不改变目录所有权边界。
fn paths_overlap(left: &Path, right: &Path) -> bool {
    #[cfg(target_os = "windows")]
    let (left, right) = (
        PathBuf::from(left.as_os_str().to_ascii_lowercase()),
        PathBuf::from(right.as_os_str().to_ascii_lowercase()),
    );
    left.starts_with(&right) || right.starts_with(&left)
}

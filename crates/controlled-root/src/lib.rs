//! 提供跨平台工具文件系统根目录及严格的路径、链接和文件句柄边界。

use std::ffi::OsStr;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// 已规范化且可用于工具文件操作的根目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlledRoot {
    path: PathBuf,
}

impl ControlledRoot {
    /// 打开既有根目录，并拒绝根路径本身是链接或重解析点。
    pub fn open(path: &Path) -> Result<Self, ControlledRootError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| ControlledRootError::Io {
            operation: "读取工具根目录元数据",
            path: path.to_path_buf(),
            source,
        })?;
        if has_link_semantics(&metadata) || !metadata.is_dir() {
            return Err(ControlledRootError::UnsafePath {
                path: path.to_path_buf(),
                message: "工具根路径必须是既有普通目录，不能是符号链接或重解析点".to_owned(),
            });
        }

        let canonical = fs::canonicalize(path).map_err(|source| ControlledRootError::Io {
            operation: "规范化工具根目录",
            path: path.to_path_buf(),
            source,
        })?;
        let root = Self { path: canonical };
        root.validate_existing_directory(&root.path)?;
        Ok(root)
    }

    /// 返回规范化后的工具根目录。
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// 有界等待工具根内的排他文件锁，返回的句柄关闭时释放锁。
    ///
    /// 锁文件长期保留；调用方不得删除或替换它，以免其他进程锁定不同文件。
    pub fn lock_file(
        &self,
        relative: &Path,
        timeout: std::time::Duration,
    ) -> std::io::Result<File> {
        use std::io;
        use std::time::{Duration, Instant};

        let started = Instant::now();
        loop {
            let result: io::Result<File> = (|| {
                let path = self
                    .prepare_new_file(relative)
                    .or_else(|error| match error {
                        ControlledRootError::PathConflict { .. } => self.existing_file(relative),
                        other => Err(other),
                    })
                    .map_err(io::Error::other)?;
                let mut options = fs::OpenOptions::new();
                options.read(true).write(true).create(true).truncate(false);
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.share_mode(0);
                }
                let file = options.open(&path)?;
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd;
                    // 非阻塞锁配合有界重试，避免无限等待失去响应。
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                self.ensure_open_file_matches(&file, &path)
                    .map_err(io::Error::other)?;
                Ok(file)
            })();
            match result {
                Ok(file) => return Ok(file),
                Err(error) => {
                    #[cfg(windows)]
                    let busy = error.raw_os_error()
                        == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32);
                    #[cfg(not(windows))]
                    let busy = error.kind() == io::ErrorKind::WouldBlock;
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if !busy || remaining.is_zero() {
                        return Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(10).min(remaining));
                }
            }
        }
    }

    /// 按当前平台的路径比较规则确认规范绝对路径是否位于工具根目录内。
    pub fn contains_canonical_path(&self, path: &Path) -> bool {
        path_starts_with(path, &self.path)
    }

    /// 按当前平台规则比较两个已经规范化的绝对路径。
    pub fn canonical_paths_equal(&self, left: &Path, right: &Path) -> bool {
        paths_equal(left, right)
    }

    /// 根据已打开文件句柄确认真实目标就是预先解析的工具根内文件。
    ///
    /// 调用方必须继续使用同一个句柄读取，不能在校验后按原路径重新打开。
    pub fn ensure_open_file_matches(
        &self,
        file: &File,
        expected_path: &Path,
    ) -> Result<PathBuf, ControlledRootError> {
        let actual = opened_file_path(file).map_err(|source| ControlledRootError::Io {
            operation: "解析已打开文件的真实路径",
            path: expected_path.to_path_buf(),
            source,
        })?;
        if !actual.is_absolute() {
            return Err(ControlledRootError::UnsafePath {
                path: expected_path.to_path_buf(),
                message: format!("已打开文件的真实路径不是绝对路径: {}", actual.display()),
            });
        }
        if !path_starts_with(&actual, &self.path) || !paths_equal(&actual, expected_path) {
            return Err(ControlledRootError::UnsafePath {
                path: expected_path.to_path_buf(),
                message: format!(
                    "已打开文件的真实目标 {} 与工具文件 {} 不一致",
                    actual.display(),
                    expected_path.display()
                ),
            });
        }
        ensure_file_identity(file, expected_path)?;
        Ok(actual)
    }

    /// 校验尚不要求存在的工具根内相对路径，并返回规范表示。
    pub fn validated_relative_path(
        &self,
        relative_path: &Path,
    ) -> Result<PathBuf, ControlledRootError> {
        normalize_relative_path(relative_path)
    }

    /// 解析工具根内已经存在的普通目录，并拒绝任一路径段的链接语义。
    pub fn existing_directory(&self, relative_path: &Path) -> Result<PathBuf, ControlledRootError> {
        let normalized = normalize_relative_path(relative_path)?;
        let requested = self.path.join(&normalized);
        self.validate_existing_parent(&normalized)?;
        self.validate_existing_directory(&requested)?;
        self.validate_canonical_boundary(&requested, "规范化工具目录")
    }

    /// 解析工具根内已经存在的普通文件，并拒绝任一路径段的链接语义。
    pub fn existing_file(&self, relative_path: &Path) -> Result<PathBuf, ControlledRootError> {
        let normalized = normalize_relative_path(relative_path)?;
        let requested = self.path.join(&normalized);
        self.validate_existing_parent(&normalized)?;
        let metadata =
            fs::symlink_metadata(&requested).map_err(|source| ControlledRootError::Io {
                operation: "读取工具文件元数据",
                path: requested.clone(),
                source,
            })?;
        if has_link_semantics(&metadata) || !metadata.is_file() {
            return Err(ControlledRootError::UnsafePath {
                path: requested,
                message: "目标必须是工具根内的普通文件，不能是符号链接或重解析点".to_owned(),
            });
        }
        self.validate_canonical_boundary(&requested, "规范化工具文件")
    }

    /// 枚举指定相对目录的直接普通文件；目录缺失时返回空集合。
    pub fn list_direct_files(
        &self,
        relative_directory: &Path,
    ) -> Result<Vec<(PathBuf, u64)>, ControlledRootError> {
        let normalized_directory = normalize_relative_path(relative_directory)?;
        let mut directory = self.path.clone();
        for component in normalized_directory.components() {
            let Component::Normal(name) = component else {
                unreachable!("相对目录已经完成规范校验");
            };
            directory.push(name);
            match fs::symlink_metadata(&directory) {
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Vec::new());
                }
                Err(source) => {
                    return Err(ControlledRootError::Io {
                        operation: "读取工具目录元数据",
                        path: directory,
                        source,
                    });
                }
                Ok(metadata) if has_link_semantics(&metadata) || !metadata.is_dir() => {
                    return Err(ControlledRootError::UnsafePath {
                        path: directory,
                        message: "目录路径段必须是普通目录，不能是符号链接或文件".to_owned(),
                    });
                }
                Ok(_) => self.validate_existing_directory(&directory)?,
            }
        }

        let entries = fs::read_dir(&directory).map_err(|source| ControlledRootError::Io {
            operation: "枚举工具目录",
            path: directory.clone(),
            source,
        })?;
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ControlledRootError::Io {
                operation: "读取工具目录项",
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| ControlledRootError::Io {
                    operation: "读取工具目录项元数据",
                    path: path.clone(),
                    source,
                })?;
            if has_link_semantics(&metadata) {
                return Err(ControlledRootError::UnsafePath {
                    path,
                    message: "工具目录不能包含符号链接或重解析点".to_owned(),
                });
            }
            if !metadata.is_file() {
                return Err(ControlledRootError::UnsafePath {
                    path,
                    message: "工具目录只能直接包含普通文件".to_owned(),
                });
            }
            let name = entry
                .file_name()
                .to_str()
                .ok_or_else(|| ControlledRootError::InvalidRelativePath {
                    path: entry.path(),
                    message: "文件名必须是有效 Unicode 文本".to_owned(),
                })?
                .to_owned();
            let relative = normalized_directory.join(name);
            self.existing_file(&relative)?;
            files.push((relative, metadata.len()));
        }
        files.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(files)
    }

    /// 递归枚举工具根内普通文件，并跳过指定的顶层目录。
    pub fn list_regular_files(
        &self,
        excluded_top_level: &[&str],
    ) -> Result<Vec<PathBuf>, ControlledRootError> {
        let mut excluded = Vec::with_capacity(excluded_top_level.len());
        for value in excluded_top_level {
            let normalized = normalize_relative_path(Path::new(value))?;
            if normalized.components().count() != 1 {
                return Err(ControlledRootError::InvalidRelativePath {
                    path: normalized,
                    message: "枚举排除项只能是单个顶层目录名".to_owned(),
                });
            }
            excluded.push(value.to_ascii_lowercase());
        }

        let mut files = Vec::new();
        let mut pending = vec![(self.path.clone(), PathBuf::new())];
        while let Some((directory, relative_directory)) = pending.pop() {
            self.validate_existing_directory(&directory)?;
            let entries = fs::read_dir(&directory).map_err(|source| ControlledRootError::Io {
                operation: "枚举工具根目录",
                path: directory.clone(),
                source,
            })?;
            let mut entries = entries.collect::<Result<Vec<_>, _>>().map_err(|source| {
                ControlledRootError::Io {
                    operation: "读取工具根目录项",
                    path: directory.clone(),
                    source,
                }
            })?;
            entries.sort_by_key(fs::DirEntry::file_name);

            for entry in entries {
                let relative = relative_directory.join(entry.file_name());
                let normalized = normalize_relative_path(&relative)?;
                let path = entry.path();
                let metadata =
                    fs::symlink_metadata(&path).map_err(|source| ControlledRootError::Io {
                        operation: "读取工具根目录项元数据",
                        path: path.clone(),
                        source,
                    })?;
                if has_link_semantics(&metadata) {
                    return Err(ControlledRootError::UnsafePath {
                        path,
                        message: "工具根目录枚举遇到符号链接或重解析点".to_owned(),
                    });
                }
                if metadata.is_dir() {
                    self.validate_existing_directory(&path)?;
                    let is_excluded = normalized.components().count() == 1
                        && normalized
                            .to_str()
                            .is_some_and(|name| excluded.contains(&name.to_ascii_lowercase()));
                    if !is_excluded {
                        pending.push((path, normalized));
                    }
                } else if metadata.is_file() {
                    self.existing_file(&normalized)?;
                    files.push(normalized);
                } else {
                    return Err(ControlledRootError::UnsafePath {
                        path,
                        message: "工具根目录只能包含普通文件和普通目录".to_owned(),
                    });
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// 逐段建立工具根内目录，创建任何子项前先验证已有父目录。
    pub fn ensure_directory(&self, relative_path: &Path) -> Result<PathBuf, ControlledRootError> {
        let normalized = normalize_relative_path(relative_path)?;
        let mut current = self.path.clone();
        for component in normalized.components() {
            let Component::Normal(name) = component else {
                unreachable!("相对路径已经完成规范校验");
            };
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(_) => self.validate_existing_directory(&current)?,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(source) => {
                            return Err(ControlledRootError::Io {
                                operation: "建立工具目录",
                                path: current,
                                source,
                            });
                        }
                    }
                    self.validate_existing_directory(&current)?;
                }
                Err(source) => {
                    return Err(ControlledRootError::Io {
                        operation: "读取工具目录元数据",
                        path: current,
                        source,
                    });
                }
            }
        }
        Ok(current)
    }

    /// 返回可用排他新建方式创建的工具根内文件路径。
    pub fn prepare_new_file(&self, relative_path: &Path) -> Result<PathBuf, ControlledRootError> {
        let normalized = normalize_relative_path(relative_path)?;
        let requested = self.path.join(&normalized);
        self.validate_existing_parent(&normalized)?;
        match fs::symlink_metadata(&requested) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(requested),
            Ok(_) => Err(ControlledRootError::PathConflict {
                path: requested,
                message: "排他新建目标已经存在".to_owned(),
            }),
            Err(source) => Err(ControlledRootError::Io {
                operation: "检查工具文件新建目标",
                path: requested,
                source,
            }),
        }
    }

    /// 以排他链接创建最终名称，成功后删除临时名称，不覆盖并发出现的目标。
    /// 创建句柄必须保持打开；发布前后和清理时均核对文件身份。
    /// 核对不是路径操作的原子隔离，调用方仍需控制目录写入者和文件内容修改。
    pub fn rename_new_file(
        &self,
        file: &File,
        source_relative: &Path,
        target_relative: &Path,
    ) -> Result<PathBuf, ControlledRootError> {
        let source = self.existing_file(source_relative)?;
        let target = self.prepare_new_file(target_relative)?;
        ensure_file_identity(file, &source)?;
        match fs::hard_link(&source, &target) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ControlledRootError::PathConflict {
                    path: target,
                    message: "排他发布目标已经存在".to_owned(),
                });
            }
            Err(source) => {
                return Err(ControlledRootError::Io {
                    operation: "排他发布工具文件",
                    path: target,
                    source,
                });
            }
        }
        ensure_file_identity(file, &target)?;
        if let Err(source_cleanup) = self.remove_file_if_exists(source_relative, Some(file)) {
            if let Err(target_rollback) = self.remove_file_if_exists(target_relative, Some(file)) {
                return Err(ControlledRootError::Io {
                    operation: "回滚工具文件发布",
                    path: target.clone(),
                    source: std::io::Error::other(PublishRollbackError {
                        temporary_path: source,
                        target_path: target,
                        source_cleanup,
                        target_rollback,
                    }),
                });
            }
            return Err(source_cleanup);
        }
        ensure_file_identity(file, &target)?;
        Ok(target)
    }

    /// 幂等删除工具根内普通文件，缺失目标不视为错误。
    /// 提供句柄时拒绝删除身份不同的文件；None 仅用于调用方独占管理的路径。
    pub fn remove_file_if_exists(
        &self,
        relative_path: &Path,
        expected_file: Option<&File>,
    ) -> Result<bool, ControlledRootError> {
        let normalized = normalize_relative_path(relative_path)?;
        let Some(requested) = self.resolve_existing_or_missing(&normalized, false)? else {
            return Ok(false);
        };
        if let Some(file) = expected_file {
            ensure_file_identity(file, &requested)?;
        }
        fs::remove_file(&requested).map_err(|source| ControlledRootError::Io {
            operation: "删除工具文件",
            path: requested,
            source,
        })?;
        Ok(true)
    }

    /// 幂等删除工具根内普通目录，删除前拒绝链接和重解析点。
    pub fn remove_directory_if_exists(
        &self,
        relative_path: &Path,
    ) -> Result<bool, ControlledRootError> {
        let normalized = normalize_relative_path(relative_path)?;
        let Some(requested) = self.resolve_existing_or_missing(&normalized, true)? else {
            return Ok(false);
        };
        fs::remove_dir_all(&requested).map_err(|source| ControlledRootError::Io {
            operation: "删除工具目录",
            path: requested,
            source,
        })?;
        Ok(true)
    }

    fn validate_existing_parent(&self, normalized: &Path) -> Result<(), ControlledRootError> {
        let parent = normalized
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if parent == Path::new(".") {
            return Ok(());
        }
        let mut current = self.path.clone();
        for component in parent.components() {
            let Component::Normal(name) = component else {
                unreachable!("相对路径已经完成规范校验");
            };
            current.push(name);
            self.validate_existing_directory(&current)?;
        }
        Ok(())
    }

    fn resolve_existing_or_missing(
        &self,
        normalized: &Path,
        expect_directory: bool,
    ) -> Result<Option<PathBuf>, ControlledRootError> {
        let mut current = self.path.clone();
        let component_count = normalized.components().count();
        for (index, component) in normalized.components().enumerate() {
            let Component::Normal(name) = component else {
                unreachable!("相对路径已经完成规范校验");
            };
            current.push(name);
            let metadata = match fs::symlink_metadata(&current) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(source) => {
                    return Err(ControlledRootError::Io {
                        operation: "读取待清理路径元数据",
                        path: current,
                        source,
                    });
                }
            };
            if has_link_semantics(&metadata) {
                return Err(ControlledRootError::UnsafePath {
                    path: current,
                    message: "待清理路径包含符号链接或重解析点".to_owned(),
                });
            }
            let is_final = index + 1 == component_count;
            let correct_kind = if is_final {
                if expect_directory {
                    metadata.is_dir()
                } else {
                    metadata.is_file()
                }
            } else {
                metadata.is_dir()
            };
            if !correct_kind {
                return Err(ControlledRootError::UnsafePath {
                    path: current,
                    message: if is_final {
                        "待清理目标类型与操作不匹配".to_owned()
                    } else {
                        "待清理路径的父项不是目录".to_owned()
                    },
                });
            }
            self.validate_canonical_boundary(&current, "规范化待清理路径")?;
        }
        Ok(Some(current))
    }

    fn validate_existing_directory(&self, path: &Path) -> Result<(), ControlledRootError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| ControlledRootError::Io {
            operation: "读取工具目录元数据",
            path: path.to_path_buf(),
            source,
        })?;
        if has_link_semantics(&metadata) || !metadata.is_dir() {
            return Err(ControlledRootError::UnsafePath {
                path: path.to_path_buf(),
                message: "路径段必须是普通目录，不能是文件、符号链接或重解析点".to_owned(),
            });
        }
        self.validate_canonical_boundary(path, "规范化工具目录")?;
        Ok(())
    }

    fn validate_canonical_boundary(
        &self,
        path: &Path,
        operation: &'static str,
    ) -> Result<PathBuf, ControlledRootError> {
        let canonical = fs::canonicalize(path).map_err(|source| ControlledRootError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
        if !path_starts_with(&canonical, &self.path) {
            return Err(ControlledRootError::UnsafePath {
                path: path.to_path_buf(),
                message: format!(
                    "规范化路径 {} 已离开工具根目录 {}",
                    canonical.display(),
                    self.path.display()
                ),
            });
        }
        Ok(canonical)
    }
}

/// 工具路径不满足相对、普通且不经链接跳转的约束。
#[derive(Debug, Error)]
pub enum ControlledRootError {
    /// 相对路径包含空值、绝对前缀、父级跳转或 Windows 歧义名称。
    #[error("工具根内相对路径 {path} 无效: {message}")]
    InvalidRelativePath { path: PathBuf, message: String },
    /// 既有路径类型、链接属性或规范化边界不安全。
    #[error("工具路径 {path} 不安全: {message}")]
    UnsafePath { path: PathBuf, message: String },
    /// 排他创建或发布目标与既有条目冲突。
    #[error("工具路径 {path} 冲突: {message}")]
    PathConflict { path: PathBuf, message: String },
    /// 文件系统调用失败，并保留底层错误链。
    #[error("{operation}访问 {path} 失败: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Error)]
#[error(
    "删除临时文件 {temporary_path} 失败: {source_cleanup}; 删除发布目标 {target_path} 同时失败: {target_rollback}"
)]
struct PublishRollbackError {
    temporary_path: PathBuf,
    target_path: PathBuf,
    #[source]
    source_cleanup: ControlledRootError,
    target_rollback: ControlledRootError,
}

fn ensure_file_identity(file: &File, path: &Path) -> Result<(), ControlledRootError> {
    let failure = |source| ControlledRootError::Io {
        operation: "核对工具文件身份",
        path: path.to_path_buf(),
        source,
    };
    if file_identity(file).map_err(failure)? != path_file_identity(path).map_err(failure)? {
        return Err(ControlledRootError::UnsafePath {
            path: path.to_path_buf(),
            message: "路径对应的文件身份已经改变，拒绝处理其他文件".to_owned(),
        });
    }
    Ok(())
}

#[cfg(unix)]
type FileIdentity = (u64, u64);
#[cfg(windows)]
type FileIdentity = (u64, [u8; 16]);
#[cfg(not(any(unix, windows)))]
type FileIdentity = ();

#[cfg(unix)]
fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(unix)]
fn path_file_identity(path: &Path) -> std::io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path)?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File) -> std::io::Result<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let mut information: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut information as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        information.VolumeSerialNumber,
        information.FileId.Identifier,
    ))
}

#[cfg(windows)]
fn path_file_identity(path: &Path) -> std::io::Result<FileIdentity> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
    };
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    file_identity(&file)
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "当前平台未实现文件身份查询",
    ))
}

#[cfg(not(any(unix, windows)))]
fn path_file_identity(_path: &Path) -> std::io::Result<FileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "当前平台未实现文件身份查询",
    ))
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf, ControlledRootError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                validate_portable_component(path, value)?;
                normalized.push(value);
            }
            _ => {
                return Err(ControlledRootError::InvalidRelativePath {
                    path: path.to_path_buf(),
                    message: "只允许普通相对路径段，不允许根、盘符、当前目录或父目录跳转"
                        .to_owned(),
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ControlledRootError::InvalidRelativePath {
            path: path.to_path_buf(),
            message: "路径不得为空".to_owned(),
        });
    }
    Ok(normalized)
}

/// 拒绝会在 Windows 上产生别名、设备路径或备用数据流的名称。
pub fn validate_portable_component(path: &Path, value: &OsStr) -> Result<(), ControlledRootError> {
    let text = value
        .to_str()
        .ok_or_else(|| ControlledRootError::InvalidRelativePath {
            path: path.to_path_buf(),
            message: "路径必须是有效 Unicode".to_owned(),
        })?;
    if text.contains(['\0', ':']) || text.ends_with([' ', '.']) {
        return Err(ControlledRootError::InvalidRelativePath {
            path: path.to_path_buf(),
            message: "路径段不能包含 NUL、冒号，也不能以空格或句点结尾".to_owned(),
        });
    }
    let stem = text
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        return Err(ControlledRootError::InvalidRelativePath {
            path: path.to_path_buf(),
            message: "路径段不能使用 Windows 保留设备名".to_owned(),
        });
    }
    Ok(())
}

/// 判断目录项是否具有符号链接或 Windows 重解析点语义。
pub fn has_link_semantics(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn opened_file_path(file: &File) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd;

    fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn opened_file_path(_file: &File) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "当前 Unix 平台未实现已打开文件的真实路径解析",
    ))
}

#[cfg(windows)]
fn opened_file_path(file: &File) -> std::io::Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    const INITIAL_UTF16_UNITS: u32 = 512;
    const MAXIMUM_UTF16_UNITS: u32 = 65_536;

    let handle = file.as_raw_handle() as HANDLE;
    let mut capacity = INITIAL_UTF16_UNITS;
    loop {
        let mut buffer = vec![0; usize::try_from(capacity).unwrap_or(usize::MAX)];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if length < capacity {
            buffer.truncate(usize::try_from(length).unwrap_or(usize::MAX));
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        capacity = length.checked_add(1).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "已打开文件的真实路径长度溢出",
            )
        })?;
        if capacity > MAXIMUM_UTF16_UNITS {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "已打开文件的真实路径超过 Windows 路径上限",
            ));
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn opened_file_path(_file: &File) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "当前平台未实现已打开文件的真实路径解析",
    ))
}

#[cfg(windows)]
fn path_starts_with(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    root.components().all(|root_component| {
        path_components.next().is_some_and(|path_component| {
            path_component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&root_component.as_os_str().to_string_lossy())
        })
    })
}

#[cfg(not(windows))]
fn path_starts_with(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    path_starts_with(left, right) && path_starts_with(right, left)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::{ControlledRoot, ControlledRootError};

    #[test]
    fn creates_directories_and_publishes_new_file() {
        let fixture = TestDirectory::new("create");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        root.ensure_directory(Path::new("data/history")).unwrap();
        let temporary_relative = Path::new("data/history/.receipt.tmp");
        let temporary = root.prepare_new_file(temporary_relative).unwrap();
        fs::write(&temporary, b"receipt").unwrap();
        let file = File::open(&temporary).unwrap();

        let target = root
            .rename_new_file(
                &file,
                temporary_relative,
                Path::new("data/history/receipt.json"),
            )
            .unwrap();
        assert_eq!(fs::read(target).unwrap(), b"receipt");
        assert!(!temporary.exists());
    }

    #[test]
    fn concurrent_publication_never_replaces_the_winner() {
        const PUBLISHER_COUNT: usize = 8;

        let fixture = TestDirectory::new("concurrent-publish");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        root.ensure_directory(Path::new("data/history")).unwrap();
        let barrier = Arc::new(Barrier::new(PUBLISHER_COUNT));
        let mut publishers = Vec::with_capacity(PUBLISHER_COUNT);
        for identifier in 0..PUBLISHER_COUNT {
            let relative = PathBuf::from(format!("data/history/.receipt-{identifier}.tmp"));
            fs::write(
                fixture.root.join(&relative),
                format!("receipt-{identifier}"),
            )
            .unwrap();
            let publisher_root = root.clone();
            let publisher_barrier = Arc::clone(&barrier);
            publishers.push(thread::spawn(move || {
                publisher_barrier.wait();
                let file = File::open(publisher_root.as_path().join(&relative)).unwrap();
                let result = publisher_root.rename_new_file(
                    &file,
                    &relative,
                    Path::new("data/history/receipt.json"),
                );
                (identifier, relative, result)
            }));
        }

        let results: Vec<_> = publishers
            .into_iter()
            .map(|publisher| publisher.join().unwrap())
            .collect();
        let winners: Vec<_> = results
            .iter()
            .filter(|(_, _, result)| result.is_ok())
            .collect();
        assert_eq!(winners.len(), 1);
        let winner = winners[0].0;
        assert_eq!(
            fs::read_to_string(fixture.root.join("data/history/receipt.json")).unwrap(),
            format!("receipt-{winner}")
        );
        for (identifier, source, result) in results {
            if identifier == winner {
                assert!(!fixture.root.join(source).exists());
            } else {
                assert!(matches!(
                    result,
                    Err(ControlledRootError::PathConflict { .. })
                ));
                assert_eq!(
                    fs::read_to_string(fixture.root.join(source)).unwrap(),
                    format!("receipt-{identifier}")
                );
            }
        }
    }

    #[test]
    fn enumerates_direct_and_recursive_regular_files() {
        let fixture = TestDirectory::new("enumerate");
        fs::create_dir_all(fixture.root.join("runtime/adb")).unwrap();
        fs::create_dir_all(fixture.root.join("data/logs")).unwrap();
        fs::write(fixture.root.join("manifest.json"), b"{}").unwrap();
        fs::write(fixture.root.join("runtime/adb/adb.exe"), b"adb").unwrap();
        fs::write(fixture.root.join("data/logs/session.log"), b"runtime").unwrap();
        let root = ControlledRoot::open(&fixture.root).unwrap();

        assert_eq!(
            root.existing_directory(Path::new("runtime/adb")).unwrap(),
            fs::canonicalize(fixture.root.join("runtime/adb")).unwrap()
        );
        assert_eq!(
            root.list_direct_files(Path::new("data/logs")).unwrap(),
            [(PathBuf::from("data/logs/session.log"), 7)]
        );
        assert_eq!(
            root.list_regular_files(&["data"]).unwrap(),
            [
                PathBuf::from("manifest.json"),
                PathBuf::from("runtime/adb/adb.exe")
            ]
        );
        assert!(
            root.list_direct_files(Path::new("missing/directory"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn file_lock_waits_for_release_and_preserves_the_lock_file() {
        use std::time::Duration;
        let fixture = TestDirectory::new("file-lock");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let relative = Path::new("resource.lock");
        fs::write(fixture.root.join(relative), b"persistent").unwrap();
        let first = root.lock_file(relative, Duration::ZERO).unwrap();
        assert!(root.lock_file(relative, Duration::from_millis(20)).is_err());
        let contender = root.clone();
        let waiting = std::thread::spawn(move || {
            contender.lock_file(Path::new("resource.lock"), Duration::from_secs(2))
        });
        drop(first);
        drop(waiting.join().unwrap().unwrap());
        assert_eq!(
            fs::read(fixture.root.join(relative)).unwrap(),
            b"persistent"
        );
        assert!(
            root.lock_file(Path::new("../outside.lock"), Duration::ZERO)
                .is_err()
        );
    }

    #[test]
    fn rejects_ambiguous_or_escaping_paths() {
        let fixture = TestDirectory::new("invalid");
        let root = ControlledRoot::open(&fixture.root).unwrap();

        for invalid in ["../outside", "data/value:stream", "data/NUL.txt"] {
            assert!(matches!(
                root.prepare_new_file(Path::new(invalid)),
                Err(ControlledRootError::InvalidRelativePath { .. })
            ));
        }
        assert!(matches!(
            root.prepare_new_file(&fixture.root.join("absolute")),
            Err(ControlledRootError::InvalidRelativePath { .. })
        ));
        assert!(!fixture.parent.join("outside").exists());
    }

    #[test]
    fn rejects_replaced_source_during_publication_and_cleanup() {
        let fixture = TestDirectory::new("replaced-source");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let relative = Path::new("temporary.json");
        let temporary = root.prepare_new_file(relative).unwrap();
        fs::write(&temporary, b"original").unwrap();
        let file = File::open(&temporary).unwrap();
        let displaced = fixture.root.join("displaced.json");
        fs::rename(&temporary, &displaced).unwrap();
        fs::write(&temporary, b"replacement").unwrap();

        assert!(matches!(
            root.rename_new_file(&file, relative, Path::new("final.json")),
            Err(ControlledRootError::UnsafePath { .. })
        ));
        assert!(matches!(
            root.remove_file_if_exists(relative, Some(&file)),
            Err(ControlledRootError::UnsafePath { .. })
        ));
        assert!(!fixture.root.join("final.json").exists());
        assert_eq!(fs::read(&temporary).unwrap(), b"replacement");
        assert_eq!(fs::read(&displaced).unwrap(), b"original");
        assert!(
            root.remove_file_if_exists(Path::new("displaced.json"), Some(&file))
                .unwrap()
        );
        assert!(
            !root
                .remove_file_if_exists(Path::new("displaced.json"), Some(&file))
                .unwrap()
        );
    }

    #[test]
    fn validates_open_file_identity_and_root_boundary() {
        let fixture = TestDirectory::new("open-handle");
        let inside = fixture.root.join("inside.json");
        let other = fixture.root.join("other.json");
        let outside = fixture.parent.join("outside.json");
        fs::write(&inside, b"inside").unwrap();
        fs::write(&other, b"other").unwrap();
        fs::write(&outside, b"outside").unwrap();
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let canonical_inside = root.existing_file(Path::new("inside.json")).unwrap();

        let inside_file = File::open(&inside).unwrap();
        assert_eq!(
            root.ensure_open_file_matches(&inside_file, &canonical_inside)
                .unwrap(),
            canonical_inside
        );
        let other_file = File::open(&other).unwrap();
        assert!(matches!(
            root.ensure_open_file_matches(&other_file, &canonical_inside),
            Err(ControlledRootError::UnsafePath { .. })
        ));
        let canonical_outside = fs::canonicalize(&outside).unwrap();
        assert!(root.contains_canonical_path(&canonical_inside));
        assert!(!root.contains_canonical_path(&canonical_outside));
    }

    #[test]
    fn rejects_link_before_external_write() {
        let fixture = TestDirectory::new("link");
        let outside = fixture.parent.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::create_dir(fixture.root.join("data")).unwrap();
        let link = fixture.root.join("data/redirect");
        if let Err(source) = create_directory_link(&outside, &link) {
            #[cfg(windows)]
            if source.raw_os_error() == Some(1314) {
                eprintln!("当前 Windows 令牌没有创建目录链接的权限，跳过动态链接样本");
                return;
            }
            panic!("建立目录链接样本失败: {source}");
        }

        let root = ControlledRoot::open(&fixture.root).unwrap();
        assert!(
            root.ensure_directory(Path::new("data/redirect/created"))
                .is_err()
        );
        assert!(!outside.join("created").exists());
        assert!(
            root.lock_file(
                Path::new("data/redirect/external.lock"),
                std::time::Duration::ZERO
            )
            .is_err()
        );
        assert!(!outside.join("external.lock").exists());
    }

    #[cfg(unix)]
    fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_directory_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    struct TestDirectory {
        parent: PathBuf,
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).expect("测试需要操作系统随机源");
            let parent = home
                .join("suzushiro/scratch/controlled-root-tests")
                .join(format!(
                    "{label}-{}-{:032x}",
                    std::process::id(),
                    u128::from_le_bytes(random)
                ));
            let root = parent.join("root");
            fs::create_dir_all(&root).expect("应建立独立工具根目录样本");
            Self { parent, root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }
}

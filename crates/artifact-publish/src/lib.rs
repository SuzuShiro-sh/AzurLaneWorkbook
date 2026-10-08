//! 提供受控根目录内有界、带摘要且不覆盖既有目标的新建产物发布协议。

use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use suzushiro_content_digest::BoundedSha256Writer;
use suzushiro_controlled_root::{ControlledRoot, ControlledRootError};
use thiserror::Error;

/// 文件内容完成同步并以最终名称发布后的稳定元数据。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedArtifact {
    path: PathBuf,
    size_bytes: u64,
    sha256: String,
}

impl PublishedArtifact {
    /// 返回受控根目录内最终文件的规范绝对路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 消费发布结果并返回最终文件的规范绝对路径。
    pub fn into_path(self) -> PathBuf {
        self.path
    }

    /// 返回写入闭包实际成功写入的字节数。
    pub const fn size_bytes(&self) -> u64 {
        self.size_bytes
    }

    /// 返回实际发布内容的小写十六进制 SHA-256。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// 新建产物的路径或容量参数不满足无覆盖发布契约。
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PublishContractError {
    /// 字节上限为零，任何非空内容都会失败，空产物也不属于该协议。
    #[error("产物字节上限必须大于零")]
    ZeroMaximumBytes,
    /// 临时路径和最终路径相同，不能安全清理或发布。
    #[error("临时路径和最终路径必须不同")]
    SamePath,
    /// 临时路径和最终路径不在同一个非根相对目录。
    #[error("临时路径和最终路径必须位于同一个非根目录")]
    DifferentParent,
}

/// 通用发布器自身执行的文件系统操作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublishIoOperation {
    /// 以排他方式建立临时文件。
    CreateTemporary,
    /// 刷新缓冲区并同步临时文件内容。
    SynchronizeTemporary,
}

impl Display for PublishIoOperation {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CreateTemporary => "排他创建产物临时文件",
            Self::SynchronizeTemporary => "同步产物临时文件",
        })
    }
}

/// 写入、同步、无覆盖发布或失败清理没有完整完成。
#[derive(Debug, Error)]
pub enum PublishNewError<E>
where
    E: Error + 'static,
{
    /// 调用参数违反同目录、有界且路径不同的发布契约。
    #[error(transparent)]
    Contract(#[from] PublishContractError),
    /// 调用方的流式写入逻辑失败。
    #[error("写入产物临时文件 {path} 失败: {source}")]
    Write {
        path: PathBuf,
        maximum_bytes: u64,
        limit_exceeded: bool,
        #[source]
        source: E,
    },
    /// 发布器自身的文件创建或同步操作失败。
    #[error("{operation} {path} 失败: {source}")]
    Io {
        operation: PublishIoOperation,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// 受控根目录拒绝路径、发布或清理操作。
    #[error(transparent)]
    ControlledRoot(#[from] ControlledRootError),
    /// 原操作失败后，临时文件清理也失败。
    #[error(
        "产物操作失败: {operation_error}; 临时文件清理同时失败: {cleanup}",
        operation_error = .operation.as_ref()
    )]
    OperationAndCleanup {
        operation: Box<PublishNewError<E>>,
        #[source]
        cleanup: ControlledRootError,
    },
}

/// 在受控根目录内流式写入并发布一个尚不存在的最终文件。
///
/// 写入闭包只获得标准 [`Write`] 接口。发布器在闭包返回成功后刷新缓冲区、同步文件，
/// 再以不覆盖语义建立最终名称。写入闭包返回的错误类型会原样保存在
/// [`PublishNewError::Write`] 中。
pub fn publish_new_with<E, F>(
    root: &ControlledRoot,
    temporary_relative: &Path,
    target_relative: &Path,
    maximum_bytes: u64,
    write: F,
) -> Result<PublishedArtifact, PublishNewError<E>>
where
    E: Error + 'static,
    F: FnOnce(&mut dyn Write) -> Result<(), E>,
{
    let parent = shared_parent(temporary_relative, target_relative)?;
    if maximum_bytes == 0 {
        return Err(PublishContractError::ZeroMaximumBytes.into());
    }

    root.ensure_directory(parent)?;
    root.prepare_new_file(target_relative)?;
    let temporary = root.prepare_new_file(temporary_relative)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|source| PublishNewError::Io {
            operation: PublishIoOperation::CreateTemporary,
            path: temporary.clone(),
            source,
        })?;
    let mut writer = BoundedSha256Writer::new(BufWriter::new(file), maximum_bytes);

    if let Err(source) = write(&mut writer) {
        let operation = PublishNewError::Write {
            path: temporary,
            maximum_bytes: writer.maximum_bytes(),
            limit_exceeded: writer.limit_exceeded(),
            source,
        };
        return Err(cleanup_after_error(
            root,
            temporary_relative,
            writer.inner().get_ref(),
            operation,
        ));
    }
    if let Err(source) = flush_and_sync(&mut writer) {
        let operation = PublishNewError::Io {
            operation: PublishIoOperation::SynchronizeTemporary,
            path: temporary,
            source,
        };
        return Err(cleanup_after_error(
            root,
            temporary_relative,
            writer.inner().get_ref(),
            operation,
        ));
    }
    let size_bytes = writer.written_bytes();
    let sha256 = writer.sha256();
    match root.rename_new_file(
        writer.inner().get_ref(),
        temporary_relative,
        target_relative,
    ) {
        Ok(path) => Ok(PublishedArtifact {
            path,
            size_bytes,
            sha256,
        }),
        Err(source) => Err(cleanup_after_error(
            root,
            temporary_relative,
            writer.inner().get_ref(),
            PublishNewError::ControlledRoot(source),
        )),
    }
}

fn flush_and_sync(writer: &mut BoundedSha256Writer<BufWriter<File>>) -> io::Result<()> {
    writer.flush()?;
    writer.inner().get_ref().sync_all()
}

fn shared_parent<'a>(
    temporary_relative: &'a Path,
    target_relative: &'a Path,
) -> Result<&'a Path, PublishContractError> {
    if temporary_relative == target_relative {
        return Err(PublishContractError::SamePath);
    }
    let temporary_parent = temporary_relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    let target_parent = target_relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    match (temporary_parent, target_parent) {
        (Some(temporary_parent), Some(target_parent)) if temporary_parent == target_parent => {
            Ok(target_parent)
        }
        _ => Err(PublishContractError::DifferentParent),
    }
}

fn cleanup_after_error<E>(
    root: &ControlledRoot,
    temporary_relative: &Path,
    file: &File,
    operation: PublishNewError<E>,
) -> PublishNewError<E>
where
    E: Error + 'static,
{
    match root.remove_file_if_exists(temporary_relative, Some(file)) {
        Ok(_) => operation,
        Err(cleanup) => PublishNewError::OperationAndCleanup {
            operation: Box::new(operation),
            cleanup,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    use suzushiro_controlled_root::{ControlledRoot, ControlledRootError};

    use super::{PublishContractError, PublishNewError, publish_new_with};

    #[test]
    fn recursive_publish_errors_support_standard_error_and_display() {
        let operation = PublishNewError::Write {
            path: PathBuf::from("temporary"),
            maximum_bytes: 16,
            limit_exceeded: false,
            source: io::Error::other("write failure"),
        };
        let error = PublishNewError::OperationAndCleanup {
            operation: Box::new(operation),
            cleanup: ControlledRootError::UnsafePath {
                path: PathBuf::from("temporary"),
                message: "cleanup rejected".into(),
            },
        };
        let outer = PublishNewError::OperationAndCleanup {
            operation: Box::new(error),
            cleanup: ControlledRootError::UnsafePath {
                path: PathBuf::from("outer"),
                message: "outer cleanup rejected".into(),
            },
        };
        let boxed: Box<dyn std::error::Error> = Box::new(outer);
        assert!(boxed.to_string().contains("write failure"));
        assert!(boxed.to_string().contains("cleanup rejected"));
        assert!(
            boxed
                .source()
                .unwrap()
                .to_string()
                .contains("outer cleanup rejected")
        );
    }

    #[test]
    fn publishes_bounded_content_with_size_and_digest() {
        let fixture = TestDirectory::new("publish");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/history/.report.tmp");
        let target = Path::new("data/history/report.txt");

        let published = publish_new_with(&root, temporary, target, 1024, |writer| {
            writer.write_all(b"artifact")
        })
        .unwrap();

        assert_eq!(published.size_bytes(), 8);
        assert_eq!(
            published.sha256(),
            "c7c5c1d70c5dec4416ab6158afd0b223ef40c29b1dc1f97ed9428b94d4cadb1c"
        );
        assert_eq!(fs::read(published.path()).unwrap(), b"artifact");
        assert!(!fixture.root.join(temporary).exists());
    }

    #[test]
    fn substituted_temporary_files_are_neither_published_nor_cleaned_up() {
        for fail_write in [false, true] {
            let fixture = TestDirectory::new("replaced-temporary");
            let root = ControlledRoot::open(&fixture.root).unwrap();
            let temporary = Path::new("data/.report.tmp");
            let target = Path::new("data/report.txt");
            let temporary_path = fixture.root.join(temporary);
            let displaced = fixture.root.join("data/displaced.txt");
            let error = publish_new_with(&root, temporary, target, 1024, |writer| {
                writer.write_all(b"intended")?;
                writer.flush()?;
                fs::rename(&temporary_path, &displaced)?;
                fs::write(&temporary_path, b"replacement")?;
                if fail_write {
                    Err(io::Error::other("write failure"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();

            assert!(matches!(
                error,
                PublishNewError::OperationAndCleanup {
                    operation, cleanup: ControlledRootError::UnsafePath { .. },
                } if if fail_write {
                    matches!(*operation, PublishNewError::Write { .. })
                } else {
                    matches!(*operation, PublishNewError::ControlledRoot(ControlledRootError::UnsafePath { .. }))
                }
            ));
            assert!(!fixture.root.join(target).exists());
            assert_eq!(fs::read(&temporary_path).unwrap(), b"replacement");
            assert_eq!(fs::read(&displaced).unwrap(), b"intended");
        }
    }

    #[test]
    fn rejects_invalid_contract_before_creating_directories() {
        let fixture = TestDirectory::new("invalid");
        let root = ControlledRoot::open(&fixture.root).unwrap();

        let zero = publish_new_with(
            &root,
            Path::new("data/.report.tmp"),
            Path::new("data/report.txt"),
            0,
            |_writer| Ok::<(), io::Error>(()),
        )
        .unwrap_err();
        assert!(matches!(
            zero,
            PublishNewError::Contract(PublishContractError::ZeroMaximumBytes)
        ));

        for (temporary, target, expected) in [
            (
                "data/report.txt",
                "data/report.txt",
                PublishContractError::SamePath,
            ),
            (
                "data/.report.tmp",
                "logs/report.txt",
                PublishContractError::DifferentParent,
            ),
        ] {
            let error = publish_new_with(
                &root,
                Path::new(temporary),
                Path::new(target),
                1024,
                |_writer| Ok::<(), io::Error>(()),
            )
            .unwrap_err();
            assert!(matches!(error, PublishNewError::Contract(actual) if actual == expected));
        }
        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    }

    #[test]
    fn removes_temporary_file_after_write_error_or_size_limit() {
        let fixture = TestDirectory::new("write-error");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/.report.tmp");
        let target = Path::new("data/report.txt");

        let write_error = publish_new_with(&root, temporary, target, 1024, |_writer| {
            Err(io::Error::other("write failed"))
        })
        .unwrap_err();
        assert!(matches!(
            write_error,
            PublishNewError::Write {
                limit_exceeded: false,
                ..
            }
        ));
        assert!(!fixture.root.join(temporary).exists());

        let limit_error = publish_new_with(&root, temporary, target, 4, |writer| {
            writer.write_all(b"too large")
        })
        .unwrap_err();
        assert!(matches!(
            limit_error,
            PublishNewError::Write {
                maximum_bytes: 4,
                limit_exceeded: true,
                ..
            }
        ));
        assert!(!fixture.root.join(temporary).exists());
        assert!(!fixture.root.join(target).exists());
    }

    #[test]
    fn preserves_competing_target_and_removes_own_temporary_file() {
        let fixture = TestDirectory::new("target-race");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/.report.tmp");
        let target = Path::new("data/report.txt");
        let target_path = fixture.root.join(target);

        let error = publish_new_with(&root, temporary, target, 1024, |writer| {
            fs::write(&target_path, b"competitor")?;
            writer.write_all(b"ours")
        })
        .unwrap_err();

        assert!(matches!(
            error,
            PublishNewError::ControlledRoot(ControlledRootError::PathConflict { .. })
        ));
        assert_eq!(fs::read(target_path).unwrap(), b"competitor");
        assert!(!fixture.root.join(temporary).exists());
    }

    #[test]
    fn does_not_remove_a_competing_temporary_file_when_creation_loses() {
        let fixture = TestDirectory::new("temporary-race");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        root.ensure_directory(Path::new("data")).unwrap();
        let temporary = Path::new("data/.report.tmp");
        let target = Path::new("data/report.txt");
        fs::write(fixture.root.join(temporary), b"competitor").unwrap();

        let error = publish_new_with(&root, temporary, target, 1024, |writer| {
            writer.write_all(b"ours")
        })
        .unwrap_err();

        assert!(matches!(
            error,
            PublishNewError::ControlledRoot(ControlledRootError::PathConflict { .. })
        ));
        assert_eq!(
            fs::read(fixture.root.join(temporary)).unwrap(),
            b"competitor"
        );
        assert!(!fixture.root.join(target).exists());
    }

    #[cfg(unix)]
    #[test]
    fn preserves_write_and_cleanup_errors_when_temporary_path_changes_kind() {
        let fixture = TestDirectory::new("cleanup-error");
        let root = ControlledRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/.report.tmp");
        let target = Path::new("data/report.txt");
        let temporary_path = fixture.root.join(temporary);

        let error = publish_new_with(&root, temporary, target, 1024, |_writer| {
            fs::remove_file(&temporary_path)?;
            fs::create_dir(&temporary_path)?;
            Err(io::Error::other("write failed"))
        })
        .unwrap_err();

        assert!(matches!(
            error,
            PublishNewError::OperationAndCleanup {
                operation,
                cleanup: ControlledRootError::UnsafePath { .. },
            } if matches!(*operation, PublishNewError::Write { .. })
        ));
        assert!(temporary_path.is_dir());
        assert!(!fixture.root.join(target).exists());
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
                .join("suzushiro/scratch/artifact-publish-tests")
                .join(format!(
                    "{label}-{}-{:032x}",
                    std::process::id(),
                    u128::from_le_bytes(random)
                ));
            let root = parent.join("root");
            fs::create_dir_all(&root).expect("应建立独立产物发布样本");
            Self { parent, root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }
}

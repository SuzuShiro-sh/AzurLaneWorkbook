//! 从受控工具根目录读取固定句柄、有大小上限且可核验的文件快照。

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::SystemTime;

use thiserror::Error;

use super::tool_root::{ToolRoot, ToolRootError};
use suzushiro_content_digest::sha256_bytes;

/// 同一次受控读取产生的稳定内容、长度和摘要。
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct BoundedFileSnapshot {
    bytes: Vec<u8>,
    sha256: String,
    modified: SystemTime,
}

impl BoundedFileSnapshot {
    /// 返回与内容快照同一轮校验的最终修改时间。
    pub(crate) const fn modified(&self) -> SystemTime {
        self.modified
    }

    /// 将快照拆成内容和摘要，避免需要正文的调用方再次复制缓冲区。
    pub(crate) fn into_parts(self) -> (Vec<u8>, String) {
        (self.bytes, self.sha256)
    }
}

/// 从工具根目录读取单个受控相对路径，并拒绝超限或读取期间变化的文件。
pub(crate) fn read_bounded_file_snapshot(
    tool_root: &ToolRoot,
    relative: &Path,
    maximum_bytes: u64,
) -> Result<BoundedFileSnapshot, FileSnapshotError> {
    let absolute = tool_root
        .existing_file(relative)
        .map_err(FileSnapshotError::Path)?;
    let mut file = File::open(&absolute).map_err(|source| FileSnapshotError::Io {
        operation: "打开受控文件",
        source,
    })?;
    tool_root
        .ensure_open_file_matches(&file, &absolute)
        .map_err(FileSnapshotError::Path)?;

    let before = read_file_state(&file, "读取受控文件初始元数据")?;
    if before.len > maximum_bytes {
        return Err(FileSnapshotError::TooLarge {
            actual: before.len,
            maximum: maximum_bytes,
        });
    }

    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| FileSnapshotError::Io {
            operation: "读取受控文件内容",
            source,
        })?;
    let actual = bytes.len() as u64;
    if actual > maximum_bytes {
        return Err(FileSnapshotError::TooLarge {
            actual,
            maximum: maximum_bytes,
        });
    }

    let after = read_file_state(&file, "读取受控文件结束元数据")?;
    tool_root
        .ensure_open_file_matches(&file, &absolute)
        .map_err(FileSnapshotError::Path)?;

    let current_absolute = tool_root
        .existing_file(relative)
        .map_err(FileSnapshotError::Path)?;
    let current_file = File::open(&current_absolute).map_err(|source| FileSnapshotError::Io {
        operation: "重新打开受控文件",
        source,
    })?;
    tool_root
        .ensure_open_file_matches(&current_file, &current_absolute)
        .map_err(FileSnapshotError::Path)?;
    let current = read_file_state(&current_file, "复核受控文件元数据")?;
    if before != after || after != current || actual != before.len {
        return Err(FileSnapshotError::Changed);
    }

    Ok(BoundedFileSnapshot {
        sha256: sha256_bytes(&bytes),
        bytes,
        modified: after.modified,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct FileState {
    len: u64,
    modified: SystemTime,
    identity: FileIdentity,
}

fn read_file_state(file: &File, operation: &'static str) -> Result<FileState, FileSnapshotError> {
    let metadata = file
        .metadata()
        .map_err(|source| FileSnapshotError::Io { operation, source })?;
    let identity = FileIdentity::from_file(file, &metadata)
        .map_err(|source| FileSnapshotError::Io { operation, source })?;
    let modified = metadata
        .modified()
        .map_err(|source| FileSnapshotError::Io { operation, source })?;
    Ok(FileState {
        len: metadata.len(),
        modified,
        identity,
    })
}

#[derive(Debug, Eq, PartialEq)]
enum FileIdentity {
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(windows)]
    Windows { volume_serial: u32, file_index: u64 },
    #[cfg(not(any(unix, windows)))]
    Unavailable,
}

impl FileIdentity {
    fn from_file(_file: &File, metadata: &std::fs::Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            Ok(Self::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            use windows_sys::Win32::Foundation::HANDLE;
            use windows_sys::Win32::Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
            };

            let _ = metadata;
            let mut information = BY_HANDLE_FILE_INFORMATION::default();
            let succeeded = unsafe {
                GetFileInformationByHandle(_file.as_raw_handle() as HANDLE, &raw mut information)
            };
            if succeeded == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self::Windows {
                volume_serial: information.dwVolumeSerialNumber,
                file_index: (u64::from(information.nFileIndexHigh) << 32)
                    | u64::from(information.nFileIndexLow),
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = _file;
            let _ = metadata;
            Ok(Self::Unavailable)
        }
    }
}

/// 读取受控文件快照时可由业务适配器重新解释的中性失败类别。
#[derive(Debug, Error)]
pub(crate) enum FileSnapshotError {
    /// 文件路径或已打开句柄没有通过工具根目录边界校验。
    #[error(transparent)]
    Path(#[from] ToolRootError),
    /// 文件系统操作失败。
    #[error("{operation}失败: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    /// 文件内容超过调用方声明的读取上限。
    #[error("文件超过 {maximum} 字节上限，实际至少为 {actual} 字节")]
    TooLarge { actual: u64, maximum: u64 },
    /// 文件长度或修改时间在读取期间发生变化。
    #[error("文件在读取期间发生变化")]
    Changed,
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{FileSnapshotError, read_bounded_file_snapshot};
    use crate::adapters::tool_root::ToolRoot;
    #[cfg(unix)]
    use crate::adapters::tool_root::ToolRootError;
    use suzushiro_content_digest::sha256_bytes;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn reads_content_size_and_digest_from_the_same_bounded_snapshot() {
        let fixture = TestDirectory::new("valid");
        let relative = Path::new("data/logs/example.log");
        fs::create_dir_all(fixture.root.join("data/logs")).unwrap();
        fs::write(fixture.root.join(relative), b"stable content").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let snapshot = read_bounded_file_snapshot(&root, relative, 64).unwrap();
        let (bytes, digest) = snapshot.into_parts();

        assert_eq!(bytes, b"stable content");
        assert_eq!(digest, sha256_bytes(b"stable content"));
    }

    #[test]
    fn rejects_an_initial_file_larger_than_the_limit() {
        let fixture = TestDirectory::new("too-large");
        let relative = Path::new("data/logs/example.log");
        fs::create_dir_all(fixture.root.join("data/logs")).unwrap();
        fs::write(fixture.root.join(relative), b"12345").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert!(matches!(
            read_bounded_file_snapshot(&root, relative, 4),
            Err(FileSnapshotError::TooLarge {
                actual: 5,
                maximum: 4
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symbolic_link_before_opening_the_snapshot() {
        use std::os::unix::fs::symlink;

        let fixture = TestDirectory::new("symlink");
        let relative = Path::new("data/logs/example.log");
        fs::create_dir_all(fixture.root.join("data/logs")).unwrap();
        fs::write(fixture.root.join("source.log"), b"content").unwrap();
        symlink(fixture.root.join("source.log"), fixture.root.join(relative)).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert!(matches!(
            read_bounded_file_snapshot(&root, relative, 64),
            Err(FileSnapshotError::Path(ToolRootError::UnsafePath { .. }))
        ));
    }

    struct TestDirectory {
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let root = home
                .join("suzushiro/scratch/azlw-file-snapshot-tests")
                .join(format!(
                    "{label}-{}",
                    NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self { root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

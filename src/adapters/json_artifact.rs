//! 将通用新建产物发布协议适配为有界流式 JSON 证据文件。

use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use suzushiro_artifact_publish::{
    PublishContractError, PublishIoOperation, PublishNewError, PublishedArtifact, publish_new_with,
};
use thiserror::Error;

use super::tool_root::{ToolRoot, ToolRootError};

/// JSON 证据文件的编码、同步、发布或失败清理没有完整完成。
#[derive(Debug, Error)]
pub(crate) enum JsonArtifactError {
    #[error("JSON 产物路径无效: {message}")]
    InvalidPaths { message: String },
    #[error("编码 JSON 产物 {path} 失败: {source}")]
    Encode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{operation} {path} 失败: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    ToolRoot(#[from] ToolRootError),
    #[error("JSON 产物操作失败: {operation}; 临时文件清理同时失败: {cleanup}")]
    OperationAndCleanup {
        operation: Box<JsonArtifactError>,
        #[source]
        cleanup: ToolRootError,
    },
}

/// JSON 调用方沿用的发布结果名称，实际契约由通用产物模块提供。
pub(crate) type PublishedJson = PublishedArtifact;

/// 流式编码 JSON，完整同步后发布到尚不存在的最终路径。
pub(crate) fn write_new_pretty_json<T: Serialize + ?Sized>(
    tool_root: &ToolRoot,
    temporary_relative: &Path,
    target_relative: &Path,
    maximum_bytes: u64,
    value: &T,
) -> Result<PublishedJson, JsonArtifactError> {
    publish_new_with(
        tool_root,
        temporary_relative,
        target_relative,
        maximum_bytes,
        |writer| serde_json::to_writer_pretty(writer, value),
    )
    .map_err(map_publish_error)
}

fn map_publish_error(error: PublishNewError<serde_json::Error>) -> JsonArtifactError {
    match error {
        PublishNewError::Contract(contract) => JsonArtifactError::InvalidPaths {
            message: match contract {
                PublishContractError::ZeroMaximumBytes => "JSON 产物字节上限必须大于零".to_owned(),
                PublishContractError::SamePath | PublishContractError::DifferentParent => {
                    contract.to_string()
                }
            },
        },
        PublishNewError::Write {
            path,
            maximum_bytes,
            limit_exceeded,
            source,
        } => JsonArtifactError::Encode {
            path,
            source: if limit_exceeded {
                serde_json::Error::io(io::Error::other(format!(
                    "JSON 产物超过 {maximum_bytes} 字节上限"
                )))
            } else {
                source
            },
        },
        PublishNewError::Io {
            operation,
            path,
            source,
        } => JsonArtifactError::Io {
            operation: match operation {
                PublishIoOperation::CreateTemporary => "排他创建 JSON 临时文件",
                PublishIoOperation::SynchronizeTemporary => "同步 JSON 临时文件",
            },
            path,
            source,
        },
        PublishNewError::ControlledRoot(source) => JsonArtifactError::ToolRoot(source),
        PublishNewError::OperationAndCleanup { operation, cleanup } => {
            JsonArtifactError::OperationAndCleanup {
                operation: Box::new(map_publish_error(*operation)),
                cleanup,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde::Serialize;

    use super::{JsonArtifactError, write_new_pretty_json};
    use crate::adapters::tool_root::ToolRoot;
    #[cfg(unix)]
    use crate::adapters::tool_root::ToolRootError;

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn streams_and_publishes_pretty_json_without_leaving_temporary_file() {
        let fixture = TestDirectory::new("publish");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/history/.report.tmp");
        let target = Path::new("data/history/report.json");

        let published = write_new_pretty_json(
            &root,
            temporary,
            target,
            1024,
            &serde_json::json!({"status": "passed"}),
        )
        .unwrap();

        assert_eq!(published.size_bytes(), 24);
        assert_eq!(
            published.sha256(),
            "0d8fdd87211df1785979fb9c8098f25c3f8954cd10834f44cff41f1090f2e056"
        );
        assert_eq!(
            fs::read_to_string(published.into_path()).unwrap(),
            "{\n  \"status\": \"passed\"\n}"
        );
        assert!(!fixture.root.join(temporary).exists());
    }

    #[test]
    fn removes_temporary_file_when_encoding_exceeds_bound() {
        let fixture = TestDirectory::new("bound");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/history/.report.tmp");
        let target = Path::new("data/history/report.json");

        let error = write_new_pretty_json(
            &root,
            temporary,
            target,
            4,
            &serde_json::json!({"value": "too large"}),
        )
        .unwrap_err();

        assert!(matches!(&error, JsonArtifactError::Encode { .. }));
        assert!(error.to_string().contains("JSON 产物超过 4 字节上限"));
        assert!(!fixture.root.join(temporary).exists());
        assert!(!fixture.root.join(target).exists());
    }

    #[test]
    fn rejects_invalid_publication_paths_before_creating_files() {
        let fixture = TestDirectory::new("invalid-paths");
        let root = ToolRoot::open(&fixture.root).unwrap();

        for (temporary, target, maximum_bytes, expected) in [
            (
                "data/.report.tmp",
                "data/report.json",
                0,
                "JSON 产物字节上限必须大于零",
            ),
            (
                "data/report.json",
                "data/report.json",
                1024,
                "临时路径和最终路径必须不同",
            ),
            (
                "data/.report.tmp",
                "logs/report.json",
                1024,
                "临时路径和最终路径必须位于同一个非根目录",
            ),
        ] {
            let error = write_new_pretty_json(
                &root,
                Path::new(temporary),
                Path::new(target),
                maximum_bytes,
                &serde_json::json!({"status": "passed"}),
            )
            .unwrap_err();

            assert!(matches!(
                error,
                JsonArtifactError::InvalidPaths { ref message } if message == expected
            ));
        }

        assert_eq!(fs::read_dir(&fixture.root).unwrap().count(), 0);
    }

    #[test]
    fn removes_temporary_file_when_serializer_fails() {
        let fixture = TestDirectory::new("encode");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/history/.report.tmp");
        let target = Path::new("data/history/report.json");

        let error =
            write_new_pretty_json(&root, temporary, target, 1024, &FailingValue).unwrap_err();

        assert!(matches!(error, JsonArtifactError::Encode { .. }));
        assert!(!fixture.root.join(temporary).exists());
        assert!(!fixture.root.join(target).exists());
    }

    #[cfg(unix)]
    #[test]
    fn preserves_operation_and_cleanup_errors_when_temporary_path_changes_kind() {
        let fixture = TestDirectory::new("encode-cleanup");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/history/.report.tmp");
        let target = Path::new("data/history/report.json");

        let error = write_new_pretty_json(
            &root,
            temporary,
            target,
            1024,
            &ReplaceTemporaryWithDirectory {
                temporary: fixture.root.join(temporary),
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JsonArtifactError::OperationAndCleanup {
                operation,
                cleanup: ToolRootError::UnsafePath { .. },
            } if matches!(*operation, JsonArtifactError::Encode { .. })
        ));
        assert!(fixture.root.join(temporary).is_dir());
        assert!(!fixture.root.join(target).exists());
    }

    #[test]
    fn removes_temporary_file_when_target_appears_before_publication() {
        let fixture = TestDirectory::new("target-race");
        let root = ToolRoot::open(&fixture.root).unwrap();
        let temporary = Path::new("data/history/.report.tmp");
        let target = Path::new("data/history/report.json");
        let target_path = fixture.root.join(target);

        let error = write_new_pretty_json(
            &root,
            temporary,
            target,
            1024,
            &CreateTargetDuringSerialization {
                target: target_path.clone(),
            },
        )
        .unwrap_err();

        assert!(matches!(error, JsonArtifactError::ToolRoot(_)));
        assert_eq!(fs::read(target_path).unwrap(), b"existing");
        assert!(!fixture.root.join(temporary).exists());
    }

    struct FailingValue;

    impl Serialize for FailingValue {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("测试编码失败"))
        }
    }

    struct CreateTargetDuringSerialization {
        target: PathBuf,
    }

    impl Serialize for CreateTargetDuringSerialization {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            fs::write(&self.target, b"existing").map_err(serde::ser::Error::custom)?;
            serializer.serialize_str("payload")
        }
    }

    #[cfg(unix)]
    struct ReplaceTemporaryWithDirectory {
        temporary: PathBuf,
    }

    #[cfg(unix)]
    impl Serialize for ReplaceTemporaryWithDirectory {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            fs::remove_file(&self.temporary).map_err(serde::ser::Error::custom)?;
            fs::create_dir(&self.temporary).map_err(serde::ser::Error::custom)?;
            Err(serde::ser::Error::custom("测试编码失败"))
        }
    }

    struct TestDirectory {
        root: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from)
                .expect("测试需要 HOME 或 USERPROFILE");
            let root = home
                .join("suzushiro/scratch/azlw-json-artifact-tests")
                .join(format!("{label}-{}-{id}", std::process::id()));
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

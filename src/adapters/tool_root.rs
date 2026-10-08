//! 将通用受控根目录能力适配到项目固定的数据和工作簿布局。

use std::ffi::OsStr;
use std::fs;
use std::ops::Deref;
use std::path::{Path, PathBuf};

use suzushiro_controlled_root::ControlledRoot;
pub use suzushiro_controlled_root::{
    ControlledRootError as ToolRootError, has_link_semantics, validate_portable_component,
};

/// 程序目录下集中保存设置、运行时、布局和用户数据的资源目录名。
pub const RESOURCE_DIRECTORY: &str = ".suzushiro";

/// 返回安装目录内的资源根路径。
pub fn resource_directory(install_directory: impl AsRef<Path>) -> PathBuf {
    install_directory.as_ref().join(RESOURCE_DIRECTORY)
}

/// 打开安装目录内的资源根；目录不存在时先建立。
pub fn open_or_create_resource_root(install_directory: &Path) -> Result<ToolRoot, ToolRootError> {
    let path = resource_directory(install_directory);
    match fs::create_dir(&path) {
        Ok(()) => {}
        Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(ToolRootError::Io {
                operation: "建立资源目录",
                path,
                source,
            });
        }
    }
    ToolRoot::open(&path)
}

/// 保留项目既有调用方式，并承载固定业务目录规则的工具根目录。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolRoot {
    controlled: ControlledRoot,
}

impl ToolRoot {
    /// 打开既有工具根目录，并拒绝根路径本身是链接或重解析点。
    pub fn open(path: &Path) -> Result<Self, ToolRootError> {
        ControlledRoot::open(path).map(|controlled| Self { controlled })
    }

    /// 返回规范化后的工具根目录。
    pub fn as_path(&self) -> &Path {
        self.controlled.as_path()
    }

    /// 解析 `data/workbooks` 内的单个既有 XLSX，并建立受控工作簿引用。
    pub fn existing_workbook(
        &self,
        filename: &str,
    ) -> Result<crate::application::WorkbookRef, ToolRootError> {
        let normalized = self.workbook_relative_path(filename)?;
        self.controlled.existing_file(&normalized)?;
        Ok(crate::application::WorkbookRef::new(normalized))
    }

    /// 只读枚举 `data/workbooks` 内的普通 XLSX 文件，并拒绝链接和嵌套目录。
    pub(crate) fn list_workbook_files(&self) -> Result<Vec<(String, u64)>, ToolRootError> {
        let data_directory = self.as_path().join("data");
        match fs::symlink_metadata(&data_directory) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ToolRootError::Io {
                    operation: "读取工作簿数据目录元数据",
                    path: data_directory,
                    source,
                });
            }
            Ok(metadata) if has_link_semantics(&metadata) || !metadata.is_dir() => {
                return Err(ToolRootError::UnsafePath {
                    path: data_directory,
                    message: "工作簿数据目录必须是普通目录，不能是符号链接或文件".to_owned(),
                });
            }
            Ok(_) => {}
        }

        let workbook_relative = Path::new("data/workbooks");
        let workbook_directory = self.as_path().join(workbook_relative);
        match fs::symlink_metadata(&workbook_directory) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(ToolRootError::Io {
                    operation: "读取工作簿目录元数据",
                    path: workbook_directory,
                    source,
                });
            }
            Ok(metadata) if has_link_semantics(&metadata) || !metadata.is_dir() => {
                return Err(ToolRootError::UnsafePath {
                    path: workbook_directory,
                    message: "工作簿目录必须是普通目录，不能是符号链接或文件".to_owned(),
                });
            }
            Ok(_) => {}
        }
        self.controlled.existing_directory(workbook_relative)?;

        let entries = fs::read_dir(&workbook_directory).map_err(|source| ToolRootError::Io {
            operation: "枚举工作簿目录",
            path: workbook_directory.clone(),
            source,
        })?;
        let mut workbooks = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| ToolRootError::Io {
                operation: "读取工作簿目录项",
                path: workbook_directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| ToolRootError::Io {
                operation: "读取工作簿目录项元数据",
                path: path.clone(),
                source,
            })?;
            if has_link_semantics(&metadata) {
                return Err(ToolRootError::UnsafePath {
                    path,
                    message: "工作簿目录不能包含符号链接或重解析点".to_owned(),
                });
            }
            if metadata.is_dir() {
                return Err(ToolRootError::UnsafePath {
                    path,
                    message: "工作簿目录只能直接包含普通 .xlsx 文件".to_owned(),
                });
            }
            if !metadata.is_file() {
                return Err(ToolRootError::UnsafePath {
                    path,
                    message: "工作簿目录只能包含普通文件".to_owned(),
                });
            }
            let file_name = entry.file_name();
            let name = file_name
                .to_str()
                .ok_or_else(|| ToolRootError::InvalidRelativePath {
                    path: entry.path(),
                    message: "工作簿名称必须是有效 Unicode 文本".to_owned(),
                })?;
            if name.starts_with("~$") || !is_xlsx_filename(name) {
                continue;
            }
            let relative = self.workbook_relative_path(name)?;
            self.controlled.existing_file(&relative)?;
            workbooks.push((name.to_owned(), metadata.len()));
        }
        workbooks.sort_by(|left, right| {
            left.0
                .to_ascii_lowercase()
                .cmp(&right.0.to_ascii_lowercase())
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok(workbooks)
    }

    /// 只读枚举 `data/history` 内的普通直接子项，供历史校验端口使用。
    pub(crate) fn list_history_files(&self) -> Result<Vec<(PathBuf, u64)>, ToolRootError> {
        self.controlled.list_direct_files(Path::new("data/history"))
    }

    /// 只读枚举 `data/logs` 内的普通直接子项，供日志查询端口使用。
    pub(crate) fn list_log_files(&self) -> Result<Vec<(PathBuf, u64)>, ToolRootError> {
        self.controlled.list_direct_files(Path::new("data/logs"))
    }

    /// 将用户传入的工作簿名称转换为受控的工具目录相对路径。
    fn workbook_relative_path(&self, filename: &str) -> Result<PathBuf, ToolRootError> {
        if filename.is_empty()
            || filename.starts_with("~$")
            || filename.contains(['/', '\\'])
            || filename.trim() != filename
            || !filename.to_ascii_lowercase().ends_with(".xlsx")
        {
            return Err(ToolRootError::InvalidRelativePath {
                path: PathBuf::from(filename),
                message:
                    "工作簿必须是 data/workbooks 内的单个 .xlsx 文件名，不能是 Office 临时文件"
                        .to_owned(),
            });
        }
        self.controlled
            .validated_relative_path(&Path::new("data/workbooks").join(filename))
    }
}

impl Deref for ToolRoot {
    type Target = ControlledRoot;

    fn deref(&self) -> &Self::Target {
        &self.controlled
    }
}

fn is_xlsx_filename(filename: &str) -> bool {
    Path::new(filename)
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xlsx"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{RESOURCE_DIRECTORY, ToolRoot, ToolRootError, resource_directory};

    #[test]
    fn resource_directory_is_dot_suzushiro() {
        assert_eq!(RESOURCE_DIRECTORY, ".suzushiro");
        assert_eq!(
            resource_directory(Path::new("install")),
            Path::new("install/.suzushiro")
        );
    }

    #[test]
    fn validates_existing_workbook_filename_boundary() {
        let fixture = TestDirectory::new("workbook");
        fs::create_dir_all(fixture.root.join("data/workbooks")).unwrap();
        fs::write(fixture.root.join("data/workbooks/plan.xlsx"), b"fixture").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        let workbook = root.existing_workbook("plan.xlsx").unwrap();
        assert_eq!(
            workbook.relative_path(),
            Path::new("data/workbooks/plan.xlsx")
        );
        assert!(root.existing_workbook("missing.xlsx").is_err());
        for invalid in ["", "plan", "../plan.xlsx", "nested/plan.xlsx", " plan.xlsx"] {
            assert!(root.existing_workbook(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn lists_workbook_files_with_stable_order_and_sizes() {
        let fixture = TestDirectory::new("workbook-list");
        fs::create_dir_all(fixture.root.join("data/workbooks")).unwrap();
        fs::write(fixture.root.join("data/workbooks/zeta.xlsx"), b"123").unwrap();
        fs::write(fixture.root.join("data/workbooks/Alpha.XLSX"), b"12").unwrap();
        fs::write(fixture.root.join("data/workbooks/notes.txt"), b"ignored").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert_eq!(
            root.list_workbook_files().unwrap(),
            [("Alpha.XLSX".to_owned(), 2), ("zeta.xlsx".to_owned(), 3)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn ignores_non_workbook_names_before_portable_path_validation() {
        let fixture = TestDirectory::new("workbook-list-non-workbook");
        let directory = fixture.root.join("data/workbooks");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("plan.xlsx"), b"xlsx").unwrap();
        fs::write(directory.join("NUL.txt"), b"ignored").unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert_eq!(
            root.list_workbook_files().unwrap(),
            [("plan.xlsx".to_owned(), 4)]
        );
    }

    #[test]
    fn treats_a_missing_workbook_directory_as_empty() {
        let fixture = TestDirectory::new("workbook-list-empty");
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert!(root.list_workbook_files().unwrap().is_empty());
    }

    #[test]
    fn rejects_unsafe_workbook_directory_entries() {
        let fixture = TestDirectory::new("workbook-list-unsafe");
        let workbook_directory = fixture.root.join("data/workbooks");
        fs::create_dir_all(&workbook_directory).unwrap();
        fs::create_dir(workbook_directory.join("nested")).unwrap();
        let root = ToolRoot::open(&fixture.root).unwrap();

        assert!(matches!(
            root.list_workbook_files(),
            Err(ToolRootError::UnsafePath { .. })
        ));
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
                .join("suzushiro/scratch/azlw-tool-root-tests")
                .join(format!(
                    "{label}-{}-{:032x}",
                    std::process::id(),
                    u128::from_le_bytes(random)
                ));
            let root = parent.join("tool-root");
            fs::create_dir_all(&root).expect("应建立独立工具根样本");
            Self { parent, root }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.parent);
        }
    }
}

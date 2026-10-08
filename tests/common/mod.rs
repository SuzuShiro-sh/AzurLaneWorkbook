//! 集成测试共用的安装目录与 `.suzushiro` 资源根。

#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use azur_lane_workbook::adapters::tool_root::resource_directory;

pub fn resource_root(install: &Path) -> PathBuf {
    let path = resource_directory(install);
    fs::create_dir_all(&path).unwrap();
    path
}

pub fn copy_executable(install: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_BIN_EXE_AzurLaneWorkbook"));
    let destination = install.join("AzurLaneWorkbook.exe");
    fs::copy(source, &destination).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&destination).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(&destination, permissions).unwrap();
    }
    destination
}

pub fn prepare_install(install: &Path) -> PathBuf {
    let resources = resource_root(install);
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("workbook-layout.xlsx"),
        resources.join("workbook-layout.xlsx"),
    )
    .unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("settings.json"),
        resources.join("settings.json"),
    )
    .unwrap();
    copy_executable(install)
}

static NEXT_DIRECTORY_ID: AtomicU64 = AtomicU64::new(1);

pub struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    /// 在各测试套件独立的 scratch 目录中创建进程内唯一的测试目录。
    pub fn new(suite: &str, label: &str) -> Self {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .expect("测试环境必须提供 HOME 或 USERPROFILE");
        let parent = PathBuf::from(home).join("suzushiro/scratch").join(suite);
        fs::create_dir_all(&parent).unwrap();
        let identifier = NEXT_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!("{label}-{}-{identifier}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if std::thread::panicking() {
                eprintln!("清理 CLI 测试目录 {} 失败: {error}", self.path.display());
            } else {
                panic!("清理 CLI 测试目录 {} 失败: {error}", self.path.display());
            }
        }
    }
}

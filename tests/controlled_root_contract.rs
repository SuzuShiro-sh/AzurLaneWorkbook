//! 固定受控根目录的规范路径、排他发布、清理和相对路径边界契约。

use std::fs;
use std::path::Path;

use azur_lane_workbook::adapters::tool_root::{ToolRoot, ToolRootError};

mod common;

#[test]
fn controlled_root_publishes_without_replacing_existing_files() {
    let fixture = TestDirectory::new("publish");
    let root = ToolRoot::open(&fixture.root).unwrap();
    assert_eq!(root.as_path(), fs::canonicalize(&fixture.root).unwrap());

    root.ensure_directory(Path::new("data/output")).unwrap();
    let temporary_relative = Path::new("data/output/.report.tmp");
    let temporary = root.prepare_new_file(temporary_relative).unwrap();
    fs::write(&temporary, b"first").unwrap();
    let temporary_file = fs::File::open(&temporary).unwrap();

    let target_relative = Path::new("data/output/report.json");
    let target = root
        .rename_new_file(&temporary_file, temporary_relative, target_relative)
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"first");
    assert!(!temporary.exists());

    let second_relative = Path::new("data/output/.second.tmp");
    let second = root.prepare_new_file(second_relative).unwrap();
    fs::write(&second, b"second").unwrap();
    let second_file = fs::File::open(&second).unwrap();
    assert!(matches!(
        root.rename_new_file(&second_file, second_relative, target_relative),
        Err(ToolRootError::PathConflict { .. })
    ));
    assert_eq!(fs::read(&target).unwrap(), b"first");
    assert_eq!(fs::read(&second).unwrap(), b"second");

    assert!(
        root.remove_file_if_exists(second_relative, Some(&second_file))
            .unwrap()
    );
    assert!(
        !root
            .remove_file_if_exists(second_relative, Some(&second_file))
            .unwrap()
    );
    assert!(
        root.remove_directory_if_exists(Path::new("data/output"))
            .unwrap()
    );
}

#[test]
fn controlled_root_rejects_escaping_and_ambiguous_paths_before_writing() {
    let fixture = TestDirectory::new("invalid-paths");
    let root = ToolRoot::open(&fixture.root).unwrap();

    for invalid in [
        Path::new("../outside"),
        Path::new("data/value:stream"),
        Path::new("data/NUL.txt"),
        fixture.root.join("absolute").as_path(),
    ] {
        assert!(matches!(
            root.prepare_new_file(invalid),
            Err(ToolRootError::InvalidRelativePath { .. })
        ));
    }
    assert!(!fixture.parent.path().join("outside").exists());
}

struct TestDirectory {
    parent: common::TestDirectory,
    root: std::path::PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let parent = common::TestDirectory::new("azlw-controlled-root-contract", label);
        let root = parent.path().join("controlled-root");
        fs::create_dir_all(&root).expect("应建立独立受控根目录样本");
        Self { parent, root }
    }
}

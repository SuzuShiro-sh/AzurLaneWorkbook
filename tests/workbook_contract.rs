//! 验证代表性工作簿包含全部受支持 Excel 特性。

// 项目要求显式声明单位返回类型，因此仅在本文件关闭对应风格检查。
#![allow(clippy::unused_unit)]

mod common;

use azur_lane_workbook::adapters::workbook::{
    AtomicTextCellEditEvidence, TextCellEditEvidence, WorkbookFeatureEvidence,
    create_representative_workbook, edit_text_cell_atomically, edit_text_cell_to_new_file,
    inspect_representative_workbook,
};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// 代表性工作簿必须能由独立读取器重开并通过全部结构断言。
#[test]
fn representative_workbook_contains_required_features() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "representative-features");
    let workbook_path: PathBuf = directory.path().join("representative.xlsx");

    create_representative_workbook(&workbook_path).expect("代表性工作簿应建立成功");
    let evidence: WorkbookFeatureEvidence =
        inspect_representative_workbook(&workbook_path).expect("全部已知特性应通过检查");

    assert_eq!(evidence.sheet_names, ["数据", "隐藏配置"]);
    assert_eq!(evidence.text_id, "000123");
    assert_eq!(evidence.formula.trim_start_matches('='), "B2*2");
    assert!(evidence.has_comment);
    assert!(evidence.has_table);
    assert!(evidence.has_hidden_sheet);
    assert!(evidence.has_hidden_column);
    assert!(evidence.has_style);
    assert!(evidence.has_frozen_panes);
    assert!(evidence.has_auto_filter);
    assert!(evidence.has_formula);
    assert!(evidence.has_data_validation);
    assert!(evidence.has_protection);
    assert!(evidence.has_opaque_part);
    assert!(evidence.has_opaque_relationship);
    assert!(evidence.zip_entry_count >= 15);
}

/// 缺少目标父目录时必须在写文件前返回明确路径错误。
#[test]
fn representative_workbook_rejects_missing_parent() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "missing-parent");
    let workbook_path: PathBuf = directory.path().join("missing").join("representative.xlsx");

    let error: String = create_representative_workbook(&workbook_path)
        .unwrap_err()
        .to_string();

    assert!(error.contains("父目录必须已经存在"));
    assert!(!workbook_path.exists());
}

#[test]
fn representative_workbook_is_byte_reproducible() -> () {
    let directory =
        common::TestDirectory::new("azlw-workbook-tests", "representative-reproducible");
    let first_path: PathBuf = directory.path().join("first.xlsx");
    let second_path: PathBuf = directory.path().join("second.xlsx");

    create_representative_workbook(&first_path).expect("第一份代表性工作簿应建立成功");
    create_representative_workbook(&second_path).expect("第二份代表性工作簿应建立成功");

    assert_eq!(
        std::fs::read(first_path).expect("应读取第一份工作簿"),
        std::fs::read(second_path).expect("应读取第二份工作簿")
    );
}

/// 定点文本编辑必须保留目标单元格之外的工作表字节和所有其他包部件。
#[test]
fn text_cell_edit_preserves_workbook_features_and_unknown_parts() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "text-cell-edit");
    let source_path: PathBuf = directory.path().join("source.xlsx");
    let destination_path: PathBuf = directory.path().join("edited.xlsx");
    let replacement_value: &str = "000<&789";
    create_representative_workbook(&source_path).expect("代表性工作簿应建立成功");

    let edit: TextCellEditEvidence = edit_text_cell_to_new_file(
        &source_path,
        &destination_path,
        "数据",
        "A2",
        replacement_value,
    )
    .expect("定点文本编辑应通过保真检查");
    let reopened: WorkbookFeatureEvidence =
        inspect_representative_workbook(&destination_path).expect("编辑结果应由独立读取器重开");

    assert_eq!(edit.sheet_name, "数据");
    assert_eq!(edit.cell_reference, "A2");
    assert_eq!(edit.replacement_value, replacement_value);
    assert_eq!(edit.package.changed_parts.len(), 1);
    assert_eq!(edit.package.changed_parts[0], edit.worksheet_part);
    assert_eq!(
        edit.package.unchanged_entry_count + 1,
        edit.package.entry_count
    );
    assert!(edit.preserved_prefix_bytes > 0);
    assert!(edit.preserved_suffix_bytes > 0);
    assert_eq!(reopened.text_id, replacement_value);
    assert_eq!(reopened.formula.trim_start_matches('='), "B2*2");
    assert!(reopened.has_comment);
    assert!(reopened.has_table);
    assert!(reopened.has_hidden_sheet);
    assert!(reopened.has_hidden_column);
    assert!(reopened.has_style);
    assert!(reopened.has_frozen_panes);
    assert!(reopened.has_auto_filter);
    assert!(reopened.has_formula);
    assert!(reopened.has_data_validation);
    assert!(reopened.has_protection);
    assert!(reopened.has_opaque_part);
    assert!(reopened.has_opaque_relationship);
}

/// 任一外部关系必须在目标文件建立前中止编辑。
#[test]
fn text_cell_edit_rejects_external_relationship_before_writing() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "external-relationship");
    let source_path: PathBuf = directory.path().join("external.xlsx");
    let destination_path: PathBuf = directory.path().join("must-not-exist.xlsx");
    write_external_relationship_package(&source_path);

    let error: String =
        edit_text_cell_to_new_file(&source_path, &destination_path, "数据", "A2", "000789")
            .unwrap_err()
            .to_string();

    assert!(error.contains("外部目标"));
    assert!(!destination_path.exists());
}

/// 编辑入口必须在建立目标文件前拒绝 VBA 部件。
#[test]
fn text_cell_edit_rejects_vba_parts_before_writing() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "vba-package");
    let source_path: PathBuf = directory.path().join("macro.xlsx");
    let destination_path: PathBuf = directory.path().join("must-not-exist.xlsx");
    write_macro_package(&source_path, true, false);

    let error: String =
        edit_text_cell_to_new_file(&source_path, &destination_path, "数据", "A2", "000789")
            .unwrap_err()
            .to_string();

    assert!(error.contains("xl/vbaProject.bin"));
    assert!(!destination_path.exists());
}

/// 编辑入口必须在建立目标文件前拒绝宏启用内容类型。
#[test]
fn text_cell_edit_rejects_macro_content_type_before_writing() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "macro-content-type");
    let source_path: PathBuf = directory.path().join("macro.xlsx");
    let destination_path: PathBuf = directory.path().join("must-not-exist.xlsx");
    write_macro_package(&source_path, false, true);

    let error: String =
        edit_text_cell_to_new_file(&source_path, &destination_path, "数据", "A2", "000789")
            .unwrap_err()
            .to_string();

    assert!(error.contains("[Content_Types].xml#macro"));
    assert!(!destination_path.exists());
}

/// 非法单元格引用必须在目标文件建立前返回稳定错误。
#[test]
fn text_cell_edit_rejects_invalid_cell_reference_before_writing() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "invalid-cell-reference");
    let source_path: PathBuf = directory.path().join("source.xlsx");
    let destination_path: PathBuf = directory.path().join("must-not-exist.xlsx");
    create_representative_workbook(&source_path).expect("代表性工作簿应建立成功");

    let error: String =
        edit_text_cell_to_new_file(&source_path, &destination_path, "数据", "a2", "000789")
            .unwrap_err()
            .to_string();

    assert!(error.contains("单元格引用"));
    assert!(!destination_path.exists());
}

/// 已有目标文件必须保持原内容，编辑接口不得覆盖。
#[test]
fn text_cell_edit_does_not_overwrite_existing_destination() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "existing-destination");
    let source_path: PathBuf = directory.path().join("source.xlsx");
    let destination_path: PathBuf = directory.path().join("existing.xlsx");
    let original_destination: &[u8] = b"keep-existing-content";
    create_representative_workbook(&source_path).expect("代表性工作簿应建立成功");
    std::fs::write(&destination_path, original_destination).expect("应建立已有目标文件");

    let error: String =
        edit_text_cell_to_new_file(&source_path, &destination_path, "数据", "A2", "000789")
            .unwrap_err()
            .to_string();

    assert!(error.contains("目标文件已经存在"));
    let after: Vec<u8> = std::fs::read(&destination_path).expect("应读取已有目标文件");
    assert_eq!(after, original_destination);
}

/// 原位编辑必须在同目录验证后替换目标，并移除临时文件。
#[test]
fn atomic_text_cell_edit_replaces_validated_workbook() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "atomic-success");
    let workbook_path: PathBuf = directory.path().join("workbook.xlsx");
    let replacement_value: &str = "000<&atomic";
    create_representative_workbook(&workbook_path).expect("代表性工作簿应建立成功");
    #[cfg(unix)]
    let original_mode: u32 = {
        use std::os::unix::fs::PermissionsExt;

        let permissions: std::fs::Permissions = std::fs::Permissions::from_mode(0o640);
        std::fs::set_permissions(&workbook_path, permissions).expect("应设置原工作簿权限");
        std::fs::metadata(&workbook_path)
            .expect("应读取原工作簿权限")
            .permissions()
            .mode()
            & 0o777
    };

    let atomic: AtomicTextCellEditEvidence =
        edit_text_cell_atomically(&workbook_path, "数据", "A2", replacement_value)
            .expect("原位编辑应成功");
    let reopened: WorkbookFeatureEvidence =
        inspect_representative_workbook(&workbook_path).expect("原子替换后的目标应再次重开");
    let mut remaining_paths: Vec<PathBuf> = std::fs::read_dir(directory.path())
        .expect("应读取测试目录")
        .map(|entry: Result<std::fs::DirEntry, std::io::Error>| {
            entry.expect("测试目录条目应可读取").path()
        })
        .collect();

    assert!(atomic.temporary_file_removed);
    assert_eq!(atomic.edit.replacement_value, replacement_value);
    assert_eq!(atomic.edit.package.changed_parts.len(), 1);
    assert_eq!(reopened.text_id, replacement_value);
    assert!(reopened.has_opaque_part);
    assert!(reopened.has_opaque_relationship);
    remaining_paths.sort();
    assert_eq!(
        remaining_paths,
        [
            directory.path().join(".workbook.xlsx.write.lock"),
            workbook_path.clone()
        ]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode: u32 = std::fs::metadata(&workbook_path)
            .expect("应读取替换后权限")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, original_mode);
    }
}

/// 原位编辑预检失败时必须逐字节保留原文件且不留下临时文件。
#[test]
fn atomic_text_cell_edit_preserves_original_on_preflight_failure() -> () {
    let directory = common::TestDirectory::new("azlw-workbook-tests", "atomic-preflight-failure");
    let workbook_path: PathBuf = directory.path().join("external.xlsx");
    write_external_relationship_package(&workbook_path);
    let original: Vec<u8> = std::fs::read(&workbook_path).expect("应读取原始拒绝样本");

    let error: String = edit_text_cell_atomically(&workbook_path, "数据", "A2", "000789")
        .unwrap_err()
        .to_string();

    let after: Vec<u8> = std::fs::read(&workbook_path).expect("失败后原文件必须存在");
    let remaining_count: usize = std::fs::read_dir(directory.path())
        .expect("应读取测试目录")
        .count();
    assert!(error.contains("外部目标"));
    assert_eq!(after, original);
    assert_eq!(remaining_count, 1);
}

/// Windows 目标缺少删除共享权时必须报告占用并保留原文件。
#[cfg(windows)]
#[test]
fn atomic_text_cell_edit_preserves_locked_windows_workbook() -> () {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let directory = common::TestDirectory::new("azlw-workbook-tests", "atomic-windows-lock");
    let workbook_path: PathBuf = directory.path().join("locked.xlsx");
    create_representative_workbook(&workbook_path).expect("代表性工作簿应建立成功");
    let original: Vec<u8> = std::fs::read(&workbook_path).expect("应读取锁定前工作簿");
    let lock: File = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&workbook_path)
        .expect("应以不共享删除的方式持有工作簿");

    let error: String = edit_text_cell_atomically(&workbook_path, "数据", "A2", "000789")
        .unwrap_err()
        .to_string();

    let after: Vec<u8> = std::fs::read(&workbook_path).expect("锁冲突后原文件必须存在");
    let remaining_count: usize = std::fs::read_dir(directory.path())
        .expect("应读取测试目录")
        .count();
    assert!(error.contains("正在被占用"), "实际错误: {error}");
    assert_eq!(after, original);
    assert_eq!(remaining_count, 2);
    assert!(directory.path().join(".locked.xlsx.write.lock").is_file());
    drop(lock);
}

/// 建立只含必需 OPC 部件和一个外部关系的最小拒绝样本。
fn write_external_relationship_package(path: &Path) -> () {
    let file: File = File::create(path).expect("应建立外部关系样本");
    let mut writer: ZipWriter<File> = ZipWriter::new(file);
    let options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    let entries: [(&str, &[u8]); 4] = [
        (
            "[Content_Types].xml",
            br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"/>"#,
        ),
        (
            "_rels/.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="externalProbe" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink" Target="https://example.invalid/external.xlsx" TargetMode="External"/></Relationships>"#,
        ),
        (
            "xl/workbook.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        ),
    ];
    for (name, bytes) in entries {
        writer
            .start_file(name, options)
            .expect("应开始写入最小样本部件");
        writer.write_all(bytes).expect("应写入最小样本部件");
    }
    let file: File = writer.finish().expect("应结束最小样本写出");
    file.sync_all().expect("应同步最小样本");
}

/// 建立只含宏拒绝条件和必需关系部件的最小包。
fn write_macro_package(path: &Path, include_vba_part: bool, macro_content_type: bool) -> () {
    let file: File = File::create(path).expect("应建立宏拒绝样本");
    let mut writer: ZipWriter<File> = ZipWriter::new(file);
    let options: SimpleFileOptions = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());
    let workbook_content_type = if macro_content_type {
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml"
    } else {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
    };
    let content_types = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="{workbook_content_type}"/></Types>"#
    );
    let entries: [(&str, &[u8]); 4] = [
        ("[Content_Types].xml", content_types.as_bytes()),
        (
            "_rels/.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        ),
        (
            "xl/workbook.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#,
        ),
        (
            "xl/_rels/workbook.xml.rels",
            br#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#,
        ),
    ];
    for (name, bytes) in entries {
        writer
            .start_file(name, options)
            .expect("应开始写入宏拒绝样本部件");
        writer.write_all(bytes).expect("应写入宏拒绝样本部件");
    }
    if include_vba_part {
        writer
            .start_file("xl/vbaProject.bin", options)
            .expect("应开始写入 VBA 部件");
        writer.write_all(b"macro-probe").expect("应写入 VBA 部件");
    }
    let file: File = writer.finish().expect("应结束宏拒绝样本写出");
    file.sync_all().expect("应同步宏拒绝样本");
}

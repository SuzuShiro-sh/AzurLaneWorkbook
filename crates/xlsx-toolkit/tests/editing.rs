//! 独立消费通用库，使用动态建立的工作簿验证编辑与发布边界。
use rust_xlsxwriter::Workbook;
use std::{
    collections::BTreeMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
};
use suzushiro_xlsx_toolkit::{
    XlsxError,
    atomic::{edit_text_cell_atomically, edit_workbook_atomically_with_pre_publish},
    editor::{edit_text_cell_to_new_file, validate_workbook_package},
    package::{PackageAddition, PackageRelationship, PackageSnapshot, rewrite_package_from_bytes},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let nonce = getrandom::u64().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/test-workbooks")
            .join(format!("{nonce:016x}"));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn workbook(&self, link: bool) -> PathBuf {
        let path = self.path("source.xlsx");
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.set_name("Records").unwrap();
        sheet.write_string(0, 0, "original").unwrap();
        sheet.write_string(0, 1, "untouched").unwrap();
        if link {
            sheet
                .write_url(1, 0, "https://example.org/reference")
                .unwrap();
        }
        workbook.save(&path).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("清理当前测试目录");
    }
}
fn allow_reference(relationship: &PackageRelationship) -> bool {
    relationship.relationship_type
        == "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
        && relationship.target == "https://example.org/reference"
}

#[test]
fn edits_new_file_and_atomically_replaces_only_the_requested_cell() {
    let fixture = Fixture::new();
    let source = fixture.workbook(false);
    let before = fs::read(&source).unwrap();
    let output = fixture.path("edited.xlsx");
    let evidence =
        edit_text_cell_to_new_file(&source, &output, "Records", "A1", "changed", |_| false)
            .unwrap();
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(evidence.package.changed_parts, ["xl/worksheets/sheet1.xml"]);
    assert_eq!(
        evidence.package.unchanged_entry_count + 1,
        evidence.package.entry_count
    );
    assert_eq!(
        PackageSnapshot::read(&source)
            .unwrap()
            .part("xl/styles.xml")
            .unwrap(),
        PackageSnapshot::read(&output)
            .unwrap()
            .part("xl/styles.xml")
            .unwrap()
    );
    let atomic = edit_text_cell_atomically(&output, "Records", "A1", "final", |_| false).unwrap();
    assert!(atomic.temporary_file_removed);
    assert_eq!(atomic.edit.replacement_value, "final");
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 3);
}

#[test]
fn caller_policy_controls_hyperlinks_without_allowing_macro_parts() {
    let fixture = Fixture::new();
    let source = fixture.workbook(true);
    assert!(matches!(
        validate_workbook_package(&source, |_| false),
        Err(XlsxError::ExternalRelationship { .. })
    ));
    validate_workbook_package(&source, allow_reference).unwrap();
    edit_text_cell_to_new_file(
        &source,
        &fixture.path("edited.xlsx"),
        "Records",
        "A1",
        "changed",
        allow_reference,
    )
    .unwrap();
    let bytes = rewrite_package_from_bytes(
        &fs::read(&source).unwrap(),
        Cursor::new(Vec::new()),
        &source,
        &source,
        &BTreeMap::new(),
        &[PackageAddition {
            name: "xl/vbaProject.bin".into(),
            bytes: b"fixture".to_vec(),
        }],
    )
    .unwrap()
    .into_inner();
    let macro_path = fixture.path("macro.xlsx");
    fs::write(&macro_path, bytes).unwrap();
    assert!(matches!(
        validate_workbook_package(&macro_path, |_| true),
        Err(XlsxError::UnsupportedPart { .. })
    ));
}

#[test]
fn macro_content_types_are_checked_after_xml_decoding() {
    let fixture = Fixture::new();
    let source = fixture.workbook(false);
    let original = fs::read(&source).unwrap();
    let package = PackageSnapshot::read(&source).unwrap();
    let content_types =
        String::from_utf8(package.part("[Content_Types].xml").unwrap().to_vec()).unwrap();

    for (name, content_type) in [
        ("literal", "application/vnd.ms-office.vbaProject"),
        ("encoded", "application/vnd.ms-office.vba&#x50;roject"),
    ] {
        let replacement = content_types.replace(
            "</Types>",
            &format!(
                r#"<Override PartName="/xl/payload.bin" ContentType="{content_type}"/></Types>"#
            ),
        );
        let path = fixture.path(&format!("{name}.xlsx"));
        let bytes = rewrite_package_from_bytes(
            &original,
            Cursor::new(Vec::new()),
            &source,
            &path,
            &BTreeMap::from([("[Content_Types].xml".to_owned(), replacement.into_bytes())]),
            &[PackageAddition {
                name: "xl/payload.bin".into(),
                bytes: b"fixture".to_vec(),
            }],
        )
        .unwrap()
        .into_inner();
        fs::write(&path, bytes).unwrap();
        assert!(matches!(
            validate_workbook_package(&path, |_| false),
            Err(XlsxError::UnsupportedPart { .. })
        ));
    }
}

#[test]
fn macro_type_text_in_comments_is_not_a_content_type() {
    let fixture = Fixture::new();
    let source = fixture.workbook(false);
    let package = PackageSnapshot::read(&source).unwrap();
    let types = String::from_utf8(package.part("[Content_Types].xml").unwrap().to_vec()).unwrap();
    let replacement = types.replace(
        "</Types>",
        "<!-- application/vnd.ms-office.vbaProject --></Types>",
    );
    let output = fixture.path("comment.xlsx");
    let bytes = rewrite_package_from_bytes(
        &fs::read(&source).unwrap(),
        Cursor::new(Vec::new()),
        &source,
        &output,
        &BTreeMap::from([("[Content_Types].xml".to_owned(), replacement.into_bytes())]),
        &[],
    )
    .unwrap()
    .into_inner();
    fs::write(&output, bytes).unwrap();
    validate_workbook_package(&output, |_| false).unwrap();
}

#[test]
fn rejects_existing_destination_and_invalid_cell_without_changing_inputs() {
    let fixture = Fixture::new();
    let source = fixture.workbook(false);
    let before = fs::read(&source).unwrap();
    assert!(
        edit_text_cell_to_new_file(&source, &source, "Records", "A1", "changed", |_| false)
            .is_err()
    );
    let output = fixture.path("invalid.xlsx");
    assert!(
        edit_text_cell_to_new_file(&source, &output, "Records", "XFE1", "changed", |_| false)
            .is_err()
    );
    assert!(!output.exists());
    assert_eq!(fs::read(&source).unwrap(), before);
}

#[test]
fn failed_pre_publish_check_preserves_source_and_cleans_temporary_file() {
    let fixture = Fixture::new();
    let source = fixture.workbook(false);
    let before = fs::read(&source).unwrap();
    let result = edit_workbook_atomically_with_pre_publish(
        &source,
        |temporary| {
            edit_text_cell_to_new_file(&source, temporary, "Records", "A1", "changed", |_| false)
        },
        |_| {
            Err(XlsxError::SourceChanged {
                path: source.clone(),
                expected: "before".into(),
                actual: "after".into(),
            })
        },
    );
    assert!(matches!(result, Err(XlsxError::SourceChanged { .. })));
    assert_eq!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
}

#[test]
fn concurrent_publishers_recheck_source_under_the_same_lock() {
    let fixture = Fixture::new();
    let source = fixture.workbook(false);
    let before = fs::read(&source).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let outcomes = std::thread::scope(|scope| {
        let handles: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|value| {
                let source = &source;
                let before = &before;
                let barrier = &barrier;
                scope.spawn(move || {
                    edit_workbook_atomically_with_pre_publish(
                        source,
                        |temporary| {
                            let result = edit_text_cell_to_new_file(
                                source,
                                temporary,
                                "Records",
                                "A1",
                                value,
                                |_| false,
                            );
                            // 两份临时结果都基于旧源文件完成，再竞争最终发布。
                            barrier.wait();
                            result
                        },
                        |current| {
                            if fs::read(current).unwrap() != *before {
                                return Err(XlsxError::SourceChanged {
                                    path: current.to_owned(),
                                    expected: "original".into(),
                                    actual: "published".into(),
                                });
                            }
                            Ok(())
                        },
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(XlsxError::SourceChanged { .. })))
            .count(),
        1
    );
    assert_ne!(fs::read(&source).unwrap(), before);
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 2);
}

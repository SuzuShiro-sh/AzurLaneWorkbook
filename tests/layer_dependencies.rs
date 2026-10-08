//! 独立组件不得依赖主应用，领域和应用层不得反向依赖外层。
//!
//! 依赖名取自 `cargo metadata` 的实际包名，因此包含重命名、可选和按目标启用的依赖。
//! 源码只识别 `use` 树，以及以 `crate`、`super`、`self` 开头的路径；`#[cfg(test)]` 的下一项整段跳过。
//! 宏生成的导入不在检查范围内。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const APPLICATION_PACKAGE: &str = "azur-lane-workbook";

#[test]
fn reusable_components_do_not_import_the_application() {
    let metadata = workspace_metadata();
    let packages = dependency_package_names(&metadata);
    assert!(
        packages.len() > 1,
        "cargo metadata 应返回工作区成员，实际只有 {} 个",
        packages.len()
    );
    for (package, dependencies) in &packages {
        if package == APPLICATION_PACKAGE {
            continue;
        }
        assert!(
            !dependencies.contains(APPLICATION_PACKAGE),
            "{package} 的依赖包含主应用: {dependencies:?}"
        );
    }
}

#[test]
fn domain_and_application_do_not_import_outer_layers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_no_imports(
        &root.join("src/domain"),
        &["application", "adapters", "interfaces", "bootstrap"],
    );
    assert_no_imports(
        &root.join("src/application"),
        &["adapters", "interfaces", "bootstrap"],
    );
}

#[test]
fn metadata_reports_renamed_and_target_specific_dependencies() {
    let metadata = r#"
    {
        "packages": [{
            "name": "sample-crate",
            "dependencies": [
                {
                    "name": "azur-lane-workbook",
                    "rename": "app_alias",
                    "kind": "dev",
                    "optional": true,
                    "target": "cfg(windows)"
                },
                {"name": "serde", "rename": null, "kind": null, "optional": false}
            ]
        }]
    }
    "#;
    let packages = dependency_package_names(metadata);
    assert!(packages["sample-crate"].contains(APPLICATION_PACKAGE));
    assert!(packages["sample-crate"].contains("serde"));
}

#[test]
fn import_check_covers_groups_relative_paths_and_test_only_items() {
    let source = r#"
        use crate::{
            adapters::hidden,
            domain::Ship,
        };
        use super::super::interfaces::gui::Screen;
        const LABEL: &str = "use crate::adapters::not_code";
        // use crate::bootstrap::commented;
        #[cfg(test)]
        use crate::bootstrap::test_only;
        #[cfg(test)]
        mod tests {
            use crate::adapters::fixture;
        }
        fn relative() {
            let _value = self::helper();
        }
    "#;
    let module = ["application".to_owned(), "service".to_owned()];
    let found = forbidden_imports(source, &module, &["adapters", "interfaces", "bootstrap"]);
    assert!(
        found.iter().any(|path| path == "adapters::hidden"),
        "{found:?}"
    );
    assert!(
        found.iter().any(|path| path == "interfaces::gui::Screen"),
        "{found:?}"
    );
    assert!(
        !found.iter().any(|path| path.contains("bootstrap")),
        "{found:?}"
    );
    assert!(
        !found
            .iter()
            .any(|path| path.contains("fixture") || path.contains("not_code")),
        "{found:?}"
    );
}

fn workspace_metadata() -> String {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("应能启动 cargo metadata");
    assert!(
        output.status.success(),
        "cargo metadata 失败: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo metadata 应为 UTF-8")
}

fn dependency_package_names(metadata: &str) -> BTreeMap<String, BTreeSet<String>> {
    let value: serde_json::Value =
        serde_json::from_str(metadata).expect("cargo metadata 应为 JSON");
    let packages = value["packages"]
        .as_array()
        .expect("cargo metadata 应包含 packages");
    packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().expect("包名应为字符串").to_owned();
            let dependencies = package["dependencies"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|dependency| dependency["name"].as_str().map(str::to_owned))
                .collect();
            (name, dependencies)
        })
        .collect()
}

fn assert_no_imports(directory: &Path, forbidden: &[&str]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for file in rust_files(directory) {
        let source = fs::read_to_string(&file).expect("应能读取分层源码");
        let module = module_path(root, &file);
        let found = forbidden_imports(&source, &module, forbidden);
        assert!(found.is_empty(), "{} 引用了外层: {found:?}", file.display());
    }
}

#[test]
fn production_boundaries_stay_inside_their_owners() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let generation = &[&["adapters", "workbook", "generation"][..]];
    assert_no_prefixes(&root.join("src/interfaces"), &[&["adapters"]]);
    assert_no_prefixes(&root.join("src/adapters/workbook/reader"), generation);
    assert_no_prefixes(&root.join("src/adapters/workbook/reader.rs"), generation);
    // 执行结果写回的生产代码在这个目录，不在 generation。
    assert_no_prefixes(
        &root.join("src/adapters/workbook/execution_results"),
        generation,
    );
    assert_no_prefixes(
        &root.join("src/adapters/device/runtime"),
        &[&["adapters", "device", "probe"]],
    );
    assert_no_prefixes(
        &root.join("src/adapters/device/session.rs"),
        &[&["adapters", "device", "probe"]],
    );
    assert_production_session_does_not_enter_audit(
        &root.join("src/adapters/device/probe/runner.rs"),
    );
    assert_production_session_does_not_enter_audit(&root.join("src/adapters/device/probe/runner"));
    assert_production_session_does_not_enter_audit(
        &root.join("src/adapters/device/probe/runtime_session.rs"),
    );
    assert_production_session_does_not_call_audit(&root.join("src/adapters/device/probe.rs"));
    assert_source_does_not_call(
        &root.join("src/cli/runner.rs"),
        &["manage_agent", "adapters::device"],
    );
    assert_source_does_not_call(
        &root.join("src/bootstrap/gui_tasks.rs"),
        &["manage_agent", "adapters::device"],
    );
}

fn assert_source_does_not_call(path: &Path, names: &[&str]) {
    let source = fs::read_to_string(path).expect("应能读取调用方源码");
    let found: Vec<_> = names
        .iter()
        .copied()
        .filter(|name| source.contains(name))
        .collect();
    assert!(
        found.is_empty(),
        "{} 直接调用了设备探测: {found:?}",
        path.display()
    );
}

fn assert_production_session_does_not_call_audit(path: &Path) {
    let source = fs::read_to_string(path).expect("应能读取生产会话源码");
    let found = audit_only_calls(&source);
    assert!(
        found.is_empty(),
        "{} 调用了审计入口: {found:?}",
        path.display()
    );
}

/// 生产会话可以用路径扫到的形式进入审计模块时必须失败。审计编排文件本身不在此列。
fn assert_production_session_does_not_enter_audit(path: &Path) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let audit = &[&["adapters", "device", "probe", "runner", "audit"][..]];
    let files = if path.is_dir() {
        rust_files(path)
            .into_iter()
            .filter(|file| file.file_name().and_then(|name| name.to_str()) != Some("audit.rs"))
            .collect()
    } else {
        vec![path.to_path_buf()]
    };
    assert!(
        !files.is_empty(),
        "{} 没有可扫描的生产会话文件",
        path.display()
    );
    for file in files {
        let source = fs::read_to_string(&file).expect("应能读取生产会话源码");
        let module = module_path(root, &file);
        let found = forbidden_prefixes(&source, &module, audit);
        assert!(
            found.is_empty(),
            "{} 进入了审计模块: {found:?}",
            file.display()
        );
        assert_production_session_does_not_call_audit(&file);
    }
}

fn audit_only_calls(source: &str) -> BTreeSet<String> {
    [
        "open_audited_agent",
        "probe_oversized_unauthenticated_frame",
    ]
    .into_iter()
    .filter(|name| source.contains(name))
    .map(|name| name.to_owned())
    .collect()
}

#[test]
fn audit_call_check_rejects_a_production_session_negative_probe() {
    let source =
        "session.open_audited_agent()?;\nAgentClient::probe_oversized_unauthenticated_frame();";
    let found = audit_only_calls(source);
    assert!(found.contains("open_audited_agent"));
    assert!(found.contains("probe_oversized_unauthenticated_frame"));
}

#[test]
fn prefix_check_rejects_reader_and_writeback_generation_paths() {
    let source = r#"
        use crate::adapters::workbook::generation::XlsxWorkbookGenerationPort;
        fn read_local() {
            let _value = super::generation::build();
        }
    "#;
    let reader = [
        "adapters".to_owned(),
        "workbook".to_owned(),
        "reader".to_owned(),
    ];
    let generation = &[&["adapters", "workbook", "generation"][..]];
    let found = forbidden_prefixes(source, &reader, generation);
    assert!(
        found
            .iter()
            .any(|path| path.contains("generation::XlsxWorkbookGenerationPort")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|path| path.contains("generation::build")),
        "{found:?}"
    );

    let writeback_source = r#"
        use super::super::generation::projection_writer::reject_cell_formulas;
        fn write_local() {
            let _value = crate::adapters::workbook::generation::publish();
        }
    "#;
    let writeback = [
        "adapters".to_owned(),
        "workbook".to_owned(),
        "execution_results".to_owned(),
        "writer".to_owned(),
    ];
    let found = forbidden_prefixes(writeback_source, &writeback, generation);
    assert!(
        found
            .iter()
            .any(|path| path.contains("generation::projection_writer")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|path| path.contains("generation::publish")),
        "{found:?}"
    );
}

#[test]
fn prefix_check_rejects_an_interface_import_of_adapters() {
    let source = r#"
        use crate::adapters::workbook::document::WorkbookDocument;
        fn local() {
            let _value = super::super::adapters::settings::load();
        }
    "#;
    let module = ["interfaces".to_owned(), "gui_controller".to_owned()];
    let found = forbidden_prefixes(source, &module, &[&["adapters"]]);
    assert!(
        found
            .iter()
            .any(|path| path == "adapters::workbook::document::WorkbookDocument"),
        "{found:?}"
    );
    assert!(
        found.iter().any(|path| path == "adapters::settings::load"),
        "{found:?}"
    );
}

#[test]
fn prefix_check_rejects_a_production_session_path_into_audit() {
    let source = r#"
        use self::audit::execute;
        fn local() {
            let _value = super::runner::audit::execute();
        }
    "#;
    let runner = [
        "adapters".to_owned(),
        "device".to_owned(),
        "probe".to_owned(),
        "runner".to_owned(),
    ];
    let audit = &[&["adapters", "device", "probe", "runner", "audit"][..]];
    let from_runner = forbidden_prefixes(source, &runner, audit);
    assert!(
        from_runner
            .iter()
            .any(|path| path.contains("runner::audit::execute")),
        "{from_runner:?}"
    );
    let runtime_session = [
        "adapters".to_owned(),
        "device".to_owned(),
        "probe".to_owned(),
        "runtime_session".to_owned(),
    ];
    let from_runtime = forbidden_prefixes(source, &runtime_session, audit);
    assert!(
        from_runtime
            .iter()
            .any(|path| path.contains("runner::audit::execute")),
        "{from_runtime:?}"
    );
}

#[test]
fn prefix_check_rejects_a_session_import_of_the_probe_module() {
    let source = r#"
        use crate::adapters::device::probe::RuntimeProbeError;
        fn local() {
            let _value = super::probe::run_runtime_probe();
        }
    "#;
    let module = [
        "adapters".to_owned(),
        "device".to_owned(),
        "runtime".to_owned(),
    ];
    let found = forbidden_prefixes(source, &module, &[&["adapters", "device", "probe"]]);
    assert!(
        found
            .iter()
            .any(|path| path == "adapters::device::probe::RuntimeProbeError"),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|path| path == "adapters::device::probe::run_runtime_probe"),
        "{found:?}"
    );
}

fn assert_no_prefixes(path: &Path, prefixes: &[&[&str]]) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = if path.is_dir() {
        rust_files(path)
    } else {
        vec![path.to_path_buf()]
    };
    for file in files {
        if file.file_name().and_then(|name| name.to_str()) == Some("tests.rs") {
            continue;
        }
        let source = fs::read_to_string(&file).expect("应能读取分层源码");
        let module = module_path(root, &file);
        let found = forbidden_prefixes(&source, &module, prefixes);
        assert!(
            found.is_empty(),
            "{} 越过所有者边界: {found:?}",
            file.display()
        );
    }
}

fn forbidden_prefixes(source: &str, module: &[String], prefixes: &[&[&str]]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    scan_resolved_paths(source, module, |resolved| {
        if prefixes.iter().any(|prefix| {
            resolved.len() >= prefix.len()
                && resolved
                    .iter()
                    .zip(*prefix)
                    .all(|(segment, expected)| segment == expected)
        }) {
            found.insert(resolved.join("::"));
        }
    });
    found
}

fn module_path(root: &Path, file: &Path) -> Vec<String> {
    let relative = file
        .strip_prefix(root.join("src"))
        .expect("分层源码应位于 src");
    let mut parts: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.last_mut() {
        if last == "lib.rs" || last == "main.rs" || last == "mod.rs" {
            parts.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_owned();
        }
    }
    parts
}

fn forbidden_imports(source: &str, module: &[String], forbidden: &[&str]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    scan_resolved_paths(source, module, |resolved| {
        if resolved
            .first()
            .is_some_and(|segment| forbidden.contains(&segment.as_str()))
        {
            found.insert(resolved.join("::"));
        }
    });
    found
}

fn scan_resolved_paths(source: &str, module: &[String], mut visit: impl FnMut(&[String])) {
    let mut scanner = Scanner::new(source);
    while !scanner.eof() {
        scanner.skip_trivia();
        if scanner.eof() {
            break;
        }
        if scanner.consume_cfg_test_attribute() {
            scanner.skip_item();
            continue;
        }
        if scanner.consume_attribute() {
            continue;
        }
        scanner.consume_visibility();
        if scanner.skip_literal() {
            continue;
        }
        if scanner.consume_keyword("use") {
            for path in scanner.parse_use_tree() {
                let resolved = resolve_path(module, &path, true);
                visit(&resolved);
            }
            continue;
        }
        if scanner.at_keyword("crate") || scanner.at_keyword("super") || scanner.at_keyword("self")
        {
            let path = scanner.parse_path();
            let resolved = resolve_path(module, &path, false);
            visit(&resolved);
            continue;
        }
        scanner.bump();
    }
}

fn resolve_path(module: &[String], path: &[String], use_tree: bool) -> Vec<String> {
    let mut base = Vec::new();
    let mut rest = path;
    match rest.first().map(String::as_str) {
        Some("crate") => rest = &rest[1..],
        Some("self") => {
            base.extend_from_slice(module);
            rest = &rest[1..];
        }
        Some("super") => {
            base.extend_from_slice(module);
            while rest.first().map(String::as_str) == Some("super") {
                base.pop();
                rest = &rest[1..];
            }
        }
        _ if use_tree => {}
        _ => base.extend_from_slice(module),
    }
    base.extend(rest.iter().cloned());
    base
}

fn rust_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory).expect("应能读取分层目录") {
        let path = entry.expect("应能读取目录项").path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
            files.push(path);
        }
    }
    files
}

struct Scanner<'a> {
    source: &'a [u8],
    index: usize,
}

impl<'a> Scanner<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source: source.as_bytes(),
            index: 0,
        }
    }

    fn eof(&self) -> bool {
        self.index >= self.source.len()
    }

    fn peek(&self) -> Option<u8> {
        self.source.get(self.index).copied()
    }

    fn bump(&mut self) {
        if self.index < self.source.len() {
            self.index += 1;
        }
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.bump(),
                Some(b'/') if self.source.get(self.index + 1) == Some(&b'/') => {
                    self.index += 2;
                    while !matches!(self.peek(), Some(b'\n') | None) {
                        self.bump();
                    }
                }
                Some(b'/') if self.source.get(self.index + 1) == Some(&b'*') => {
                    self.index += 2;
                    while self.peek().is_some()
                        && !(self.peek() == Some(b'*')
                            && self.source.get(self.index + 1) == Some(&b'/'))
                    {
                        self.bump();
                    }
                    self.bump();
                    self.bump();
                }
                _ => break,
            }
        }
    }

    fn consume_cfg_test_attribute(&mut self) -> bool {
        let start = self.index;
        if !self.consume_attribute() {
            return false;
        }
        let text = std::str::from_utf8(&self.source[start..self.index]).unwrap_or("");
        text.chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            == "#[cfg(test)]"
    }

    fn consume_attribute(&mut self) -> bool {
        self.skip_trivia();
        if self.peek() != Some(b'#') {
            return false;
        }
        self.bump();
        self.skip_trivia();
        if self.peek() != Some(b'[') {
            return false;
        }
        self.skip_balanced(b'[', b']');
        true
    }

    fn consume_visibility(&mut self) {
        self.skip_trivia();
        if !self.consume_keyword("pub") {
            return;
        }
        self.skip_trivia();
        if self.peek() == Some(b'(') {
            self.skip_balanced(b'(', b')');
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        self.skip_trivia();
        if !self.at_keyword(keyword) {
            return false;
        }
        self.index += keyword.len();
        true
    }

    fn at_keyword(&self, keyword: &str) -> bool {
        let bytes = keyword.as_bytes();
        self.source.get(self.index..self.index + bytes.len()) == Some(bytes)
            && !self
                .source
                .get(self.index + bytes.len())
                .is_some_and(u8::is_ascii_alphanumeric)
            && self.source.get(self.index + bytes.len()) != Some(&b'_')
            && (self.index == 0
                || !self.source[self.index - 1].is_ascii_alphanumeric()
                    && self.source[self.index - 1] != b'_')
    }

    fn skip_item(&mut self) {
        self.skip_trivia();
        self.consume_visibility();
        let mut depth = 0_i32;
        let mut saw_body = false;
        while !self.eof() {
            self.skip_trivia();
            if self.eof() {
                break;
            }
            if self.skip_literal() {
                continue;
            }
            match self.peek() {
                Some(b'{') => {
                    depth += 1;
                    saw_body = true;
                    self.bump();
                }
                Some(b'}') => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                }
                Some(b';') if depth == 0 => {
                    self.bump();
                    if !saw_body {
                        break;
                    }
                }
                Some(b';') if saw_body && depth == 0 => {
                    self.bump();
                    break;
                }
                _ => self.bump(),
            }
            if saw_body && depth == 0 {
                break;
            }
        }
    }

    fn parse_use_tree(&mut self) -> Vec<Vec<String>> {
        let paths = self.parse_tree();
        self.skip_trivia();
        if self.peek() == Some(b';') {
            self.bump();
        }
        paths
    }

    fn parse_tree(&mut self) -> Vec<Vec<String>> {
        let prefix = self.parse_path();
        self.skip_trivia();
        self.consume_alias();
        self.skip_trivia();
        if self.peek() != Some(b'{') {
            return vec![prefix];
        }
        self.bump();
        let mut paths = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'}') || self.eof() {
                self.bump();
                break;
            }
            for mut child in self.parse_tree() {
                let mut full = prefix.clone();
                if child.first().map(String::as_str) == Some("self") {
                    child.remove(0);
                }
                full.extend(child);
                paths.push(full);
            }
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.bump();
            }
        }
        paths
    }

    fn parse_path(&mut self) -> Vec<String> {
        let mut path = Vec::new();
        loop {
            self.skip_trivia();
            let Some(segment) = self.read_ident() else {
                break;
            };
            path.push(segment);
            self.skip_trivia();
            if self.peek() == Some(b':') && self.source.get(self.index + 1) == Some(&b':') {
                self.index += 2;
                continue;
            }
            break;
        }
        path
    }

    fn consume_alias(&mut self) {
        self.skip_trivia();
        if self.consume_keyword("as") {
            self.skip_trivia();
            let _ = self.read_ident();
        }
    }

    fn read_ident(&mut self) -> Option<String> {
        self.skip_trivia();
        let start = self.index;
        let first = self.peek()?;
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return None;
        }
        self.bump();
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.bump();
        }
        Some(String::from_utf8_lossy(&self.source[start..self.index]).into_owned())
    }

    fn skip_balanced(&mut self, open: u8, close: u8) {
        if self.peek() != Some(open) {
            return;
        }
        let mut depth = 0_i32;
        while !self.eof() {
            if self.skip_literal() {
                continue;
            }
            match self.peek() {
                Some(byte) if byte == open => {
                    depth += 1;
                    self.bump();
                }
                Some(byte) if byte == close => {
                    depth -= 1;
                    self.bump();
                    if depth == 0 {
                        break;
                    }
                }
                _ => self.bump(),
            }
        }
    }

    fn skip_literal(&mut self) -> bool {
        self.skip_trivia();
        match self.peek() {
            Some(b'b' | b'c' | b'r')
                if self.source.get(self.index + 1) == Some(&b'"')
                    || self.source.get(self.index + 1) == Some(&b'#')
                    || self.peek() == Some(b'c')
                        && self.source.get(self.index + 1) == Some(&b'\'') =>
            {
                if self.peek() == Some(b'c') && self.source.get(self.index + 1) == Some(&b'\'') {
                    self.bump();
                    return self.skip_char_literal();
                }
                if self.peek() == Some(b'b') || self.peek() == Some(b'c') {
                    self.bump();
                }
                self.skip_string_literal()
            }
            Some(b'"') => self.skip_string_literal(),
            Some(b'\'') => self.skip_char_literal(),
            _ => false,
        }
    }

    fn skip_string_literal(&mut self) -> bool {
        let mut hashes = 0;
        if self.peek() == Some(b'r') {
            self.bump();
            while self.peek() == Some(b'#') {
                hashes += 1;
                self.bump();
            }
        }
        if self.peek() != Some(b'"') {
            return false;
        }
        self.bump();
        if hashes == 0 {
            while let Some(byte) = self.peek() {
                self.bump();
                if byte == b'\\' {
                    self.bump();
                    continue;
                }
                if byte == b'"' {
                    break;
                }
            }
            return true;
        }
        loop {
            if self.peek() != Some(b'"') {
                if self.eof() {
                    break;
                }
                self.bump();
                continue;
            }
            let tail = self.index + 1;
            if self
                .source
                .get(tail..tail + hashes)
                .is_some_and(|bytes| bytes.iter().all(|byte| *byte == b'#'))
            {
                self.index = tail + hashes;
                break;
            }
            self.bump();
        }
        true
    }

    fn skip_char_literal(&mut self) -> bool {
        if self.peek() != Some(b'\'') {
            return false;
        }
        self.bump();
        if self.peek() == Some(b'\\') {
            self.bump();
        }
        self.bump();
        if self.peek() == Some(b'\'') {
            self.bump();
        }
        true
    }
}

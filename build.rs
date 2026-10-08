//! 从共享 JSON 契约生成 Rust 正式运行态使用的 RPC v1 常量。

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value};

fn main() {
    println!("cargo:rerun-if-changed=build/windows.manifest");
    println!("cargo:rerun-if-changed=assets/gui/icon.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let manifest_dir =
            PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
        let manifest = manifest_dir.join("build/windows.manifest");
        println!("cargo:rustc-link-arg-bin=AzurLaneWorkbook=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=AzurLaneWorkbook=/MANIFESTINPUT:{}",
            manifest.display()
        );
        embed_application_icon(&manifest_dir);
    }
    if let Err(error) = generate_runtime_rpc_contract() {
        panic!("failed to generate runtime RPC contract: {error}");
    }
}

fn embed_application_icon(manifest_dir: &Path) {
    let icon = manifest_dir.join("assets/gui/icon.ico");
    if !icon.is_file() {
        panic!("缺少程序图标: {}", icon.display());
    }
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let rc_path = out_dir.join("azur-lane-workbook-icon.rc");
    let res_path = out_dir.join("azur-lane-workbook-icon.res");
    let icon_literal = icon.display().to_string().replace('\\', "\\\\");
    fs::write(&rc_path, format!("1 ICON \"{icon_literal}\"\r\n")).expect("写入程序图标资源脚本");
    let rc = windows_sdk_rc();
    let output = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res_path)
        .arg(&rc_path)
        .output()
        .unwrap_or_else(|error| panic!("启动 {} 失败: {error}", rc.display()));
    if !output.status.success() {
        panic!(
            "{} 编译程序图标失败 ({}):\n{}",
            rc.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    println!(
        "cargo:rustc-link-arg-bin=AzurLaneWorkbook={}",
        res_path.display()
    );
}

fn windows_sdk_rc() -> PathBuf {
    let mut compilers = Vec::new();
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        let Some(root) = env::var_os(variable) else {
            continue;
        };
        let bins = PathBuf::from(root).join(r"Windows Kits\10\bin");
        let Ok(versions) = fs::read_dir(&bins) else {
            continue;
        };
        for version in versions.filter_map(Result::ok) {
            let compiler = version.path().join(r"x64\rc.exe");
            if compiler.is_file() {
                compilers.push(compiler);
            }
        }
    }
    compilers.sort();
    compilers.pop().unwrap_or_else(|| {
        panic!("未找到 Windows SDK rc.exe，无法把 assets/gui/icon.ico 嵌入 AzurLaneWorkbook.exe")
    })
}

fn generate_runtime_rpc_contract() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(required_environment("CARGO_MANIFEST_DIR")?);
    let output_dir = PathBuf::from(required_environment("OUT_DIR")?);
    let contract_path = manifest_dir.join("contracts/runtime-rpc-v1.json");
    println!("cargo:rerun-if-changed={}", contract_path.display());

    let document: Value = serde_json::from_slice(&fs::read(&contract_path)?)?;
    let root = required_object(&document, "contract root")?;
    let schema_version = required_u64(root, "schema_version")?;
    if schema_version != 1 {
        return Err(invalid_data(format!(
            "unsupported runtime RPC contract schema version {schema_version}"
        ))
        .into());
    }
    let constants = required_object(
        root.get("constants")
            .ok_or_else(|| invalid_data("missing constants object"))?,
        "constants",
    )?;

    let mut generated = String::from("// @generated from contracts/runtime-rpc-v1.json.\n\n");
    write_u32(
        &mut generated,
        constants,
        "protocol_version",
        "PROTOCOL_VERSION",
    )?;
    let agent_version = required_str(constants, "agent_version")?;
    writeln!(
        generated,
        "pub const EXPECTED_AGENT_VERSION: &str = {agent_version:?};"
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_request_bytes",
        "MAX_REQUEST_BYTES",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_response_bytes",
        "MAX_RESPONSE_BYTES",
    )?;
    write_u32(
        &mut generated,
        constants,
        "minimum_timeout_ms",
        "MIN_TIMEOUT_MS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_timeout_ms",
        "MAX_TIMEOUT_MS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_handshake_attempts",
        "MAX_HANDSHAKE_ATTEMPTS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "unauthenticated_probe_connections",
        "UNAUTHENTICATED_PROBE_CONNECTIONS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_snapshot_items",
        "MAX_SNAPSHOT_ITEMS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_equipment_page_size",
        "MAX_EQUIPMENT_PAGE_SIZE",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_equipment_frame_size",
        "MAX_EQUIPMENT_FRAME_SIZE",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_equipment_catalog_items",
        "MAX_EQUIPMENT_CATALOG_ITEMS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_ship_catalog_page_size",
        "MAX_SHIP_CATALOG_PAGE_SIZE",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_ship_catalog_frame_size",
        "MAX_SHIP_CATALOG_FRAME_SIZE",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_dock_page_size",
        "MAX_DOCK_PAGE_SIZE",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_ship_catalog_items",
        "MAX_SHIP_CATALOG_ITEMS",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_equipment_weapon_batch_size",
        "MAX_EQUIPMENT_WEAPON_BATCH_SIZE",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_skill_effect_batch_size",
        "MAX_SKILL_EFFECT_BATCH_SIZE",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_equipment_reference_batch_size",
        "MAX_EQUIPMENT_REFERENCE_BATCH_SIZE",
    )?;
    write_usize(
        &mut generated,
        constants,
        "ship_equipment_slot_count",
        "SHIP_EQUIPMENT_SLOT_COUNT",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_ship_slot_equipment_type_count",
        "MAX_SHIP_SLOT_EQUIPMENT_TYPES",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_ship_skill_count",
        "MAX_SHIP_SKILLS",
    )?;
    write_u32(
        &mut generated,
        constants,
        "maximum_fleet_team_ship_count",
        "MAX_FLEET_TEAM_SHIPS",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_ship_fleet_membership_count",
        "MAX_SHIP_FLEET_MEMBERSHIPS",
    )?;
    write_usize(
        &mut generated,
        constants,
        "maximum_enhance_material_count",
        "MAX_ENHANCE_MATERIALS",
    )?;
    write_operation_names(&mut generated, root)?;

    fs::write(output_dir.join("runtime_rpc_contract.rs"), generated)?;
    Ok(())
}

fn required_environment(name: &str) -> io::Result<std::ffi::OsString> {
    env::var_os(name).ok_or_else(|| invalid_data(format!("missing environment variable {name}")))
}

fn required_object<'a>(value: &'a Value, name: &str) -> io::Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| invalid_data(format!("{name} must be a JSON object")))
}

fn required_u64(object: &Map<String, Value>, key: &str) -> io::Result<u64> {
    object
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data(format!("{key} must be an unsigned JSON integer")))
}

fn required_str<'a>(object: &'a Map<String, Value>, key: &str) -> io::Result<&'a str> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data(format!("{key} must be a JSON string")))
}

fn write_u32(
    output: &mut String,
    constants: &Map<String, Value>,
    json_key: &str,
    rust_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = required_u64(constants, json_key)?;
    let value = u32::try_from(raw)
        .map_err(|_| invalid_data(format!("{json_key} exceeds the Rust/native uint32 range")))?;
    writeln!(output, "pub const {rust_name}: u32 = {value};")?;
    Ok(())
}

fn write_usize(
    output: &mut String,
    constants: &Map<String, Value>,
    json_key: &str,
    rust_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let raw = required_u64(constants, json_key)?;
    let value = usize::try_from(raw)
        .map_err(|_| invalid_data(format!("{json_key} exceeds the Rust usize range")))?;
    writeln!(output, "pub const {rust_name}: usize = {value};")?;
    Ok(())
}

fn write_operation_names(
    output: &mut String,
    root: &Map<String, Value>,
) -> Result<(), Box<dyn std::error::Error>> {
    let operations = root
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data("operations must be a JSON array"))?;
    if operations.is_empty() {
        return Err(invalid_data("operations must list every runtime RPC operation").into());
    }
    let mut seen = std::collections::BTreeSet::new();
    writeln!(output, "pub const RPC_OPERATION_NAMES: &[&str] = &[")?;
    for operation in operations {
        let name = operation
            .as_str()
            .ok_or_else(|| invalid_data("operations entries must be JSON strings"))?;
        if !is_operation_name(name) {
            return Err(invalid_data(format!("invalid runtime RPC operation name {name}")).into());
        }
        if !seen.insert(name) {
            return Err(
                invalid_data(format!("duplicate runtime RPC operation name {name}")).into(),
            );
        }
        writeln!(output, "    {name:?},")?;
    }
    writeln!(output, "];")?;
    Ok(())
}

fn is_operation_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
        && !name.contains("__")
        && !name.ends_with('_')
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message.into())
}

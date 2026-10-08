//! 负责解析并验证 CTest 清单及 Android x86_64 ELF 产物。

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::contracts::{NativeTestArtifactEvidence, NativeTestRunnerError};
use super::{
    ANDROID_DEVICE_LABEL, ANDROID_LINKER64, CTEST_TIMEOUT_SECONDS, ELF64_HEADER_BYTES,
    ELF64_PROGRAM_HEADER_BYTES, MAXIMUM_ARTIFACT_BYTES, MAXIMUM_PROGRAM_HEADERS,
    MAXIMUM_TEST_ARGUMENTS, MAXIMUM_TOTAL_ARTIFACT_BYTES,
};
use crate::adapters::tool_root::{ToolRoot, validate_portable_component};
use suzushiro_content_digest::sha256_file;

#[cfg(any(target_os = "windows", test))]
#[derive(Deserialize)]
struct CTestInventory {
    kind: String,
    version: CTestVersion,
    #[serde(default)]
    tests: Vec<CTestCase>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Deserialize)]
struct CTestVersion {
    major: u32,
    minor: u32,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Deserialize)]
struct CTestCase {
    name: String,
    command: Vec<String>,
    #[serde(default)]
    properties: Vec<CTestProperty>,
}

#[cfg(any(target_os = "windows", test))]
#[derive(Deserialize)]
struct CTestProperty {
    name: String,
    value: Value,
}

#[cfg(any(target_os = "windows", test))]
pub(super) struct NativeTestPlan {
    pub(super) tests: Vec<NativeTestCommand>,
    pub(super) artifacts: Vec<NativeTestArtifactEvidence>,
    pub(super) local_artifacts: Vec<(String, PathBuf)>,
}

#[cfg(any(target_os = "windows", test))]
pub(super) struct NativeTestCommand {
    pub(super) name: String,
    pub(super) arguments: Vec<String>,
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn parse_ctest_inventory(
    encoded: &str,
    build_root: &ToolRoot,
) -> Result<NativeTestPlan, NativeTestRunnerError> {
    let inventory: CTestInventory =
        serde_json::from_str(encoded).map_err(|error| NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 没有返回有效 JSON: {error}"),
        })?;
    if inventory.kind != "ctestInfo" || inventory.version.major != 1 || inventory.version.minor != 0
    {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!(
                "CTest 清单必须是 ctestInfo 1.0，实际为 {} {}.{}",
                inventory.kind, inventory.version.major, inventory.version.minor
            ),
        });
    }
    if inventory.tests.is_empty() {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: "CTest 清单没有 Android 设备测试".to_owned(),
        });
    }

    let mut names: HashSet<String> = HashSet::new();
    let mut artifact_paths: HashMap<String, PathBuf> = HashMap::new();
    let mut tests: Vec<NativeTestCommand> = Vec::with_capacity(inventory.tests.len());
    for test in inventory.tests {
        validate_test_name(&test.name)?;
        if !names.insert(test.name.clone()) {
            return Err(NativeTestRunnerError::InvalidInventory {
                message: format!("测试名重复: {}", test.name),
            });
        }
        if test.command.is_empty() || test.command.len() > MAXIMUM_TEST_ARGUMENTS + 1 {
            return Err(NativeTestRunnerError::InvalidInventory {
                message: format!(
                    "{} 的命令必须包含一个 ELF 和至多 {MAXIMUM_TEST_ARGUMENTS} 个文件参数",
                    test.name
                ),
            });
        }

        let executable: (String, PathBuf) =
            resolve_inventory_artifact(build_root, &test.command[0])?;
        if executable.0 != test.name {
            return Err(NativeTestRunnerError::InvalidInventory {
                message: format!("测试 {} 的 ELF 文件名不一致: {}", test.name, executable.0),
            });
        }
        validate_test_properties(&test, build_root, &executable.1)?;
        insert_artifact(&mut artifact_paths, executable)?;

        let mut arguments: Vec<String> = Vec::new();
        for raw_argument in &test.command[1..] {
            let artifact = resolve_inventory_artifact(build_root, raw_argument)?;
            arguments.push(artifact.0.clone());
            insert_artifact(&mut artifact_paths, artifact)?;
        }
        tests.push(NativeTestCommand {
            name: test.name,
            arguments,
        });
    }

    let mut local_artifacts: Vec<(String, PathBuf)> = artifact_paths.into_iter().collect();
    local_artifacts.sort_by(|left, right| left.0.cmp(&right.0));
    let mut artifacts: Vec<NativeTestArtifactEvidence> = Vec::with_capacity(local_artifacts.len());
    let mut total_size_bytes = 0_u64;
    for (filename, path) in &local_artifacts {
        validate_x86_64_elf(path, tests.iter().any(|test| test.name == *filename))?;
        let size_bytes = fs::metadata(path)
            .map_err(|source| NativeTestRunnerError::Io {
                path: path.clone(),
                source,
            })?
            .len();
        total_size_bytes = total_size_bytes.checked_add(size_bytes).ok_or_else(|| {
            NativeTestRunnerError::InvalidInventory {
                message: "Native 产物总大小发生整数溢出".to_owned(),
            }
        })?;
        if size_bytes == 0
            || size_bytes > MAXIMUM_ARTIFACT_BYTES
            || total_size_bytes > MAXIMUM_TOTAL_ARTIFACT_BYTES
        {
            return Err(NativeTestRunnerError::InvalidInventory {
                message: format!(
                    "Native 产物大小越界: file={filename}, size={size_bytes}, total={total_size_bytes}"
                ),
            });
        }
        let sha256 = sha256_file(path).map_err(|source| NativeTestRunnerError::Io {
            path: path.clone(),
            source,
        })?;
        artifacts.push(NativeTestArtifactEvidence {
            filename: filename.clone(),
            size_bytes,
            sha256,
            device_sha256_verified: false,
        });
    }

    Ok(NativeTestPlan {
        tests,
        artifacts,
        local_artifacts,
    })
}

#[cfg(any(target_os = "windows", test))]
fn validate_test_name(name: &str) -> Result<(), NativeTestRunnerError> {
    validate_portable_component(Path::new(name), name.as_ref()).map_err(|error| {
        NativeTestRunnerError::InvalidInventory {
            message: format!("测试名 {name:?} 不是安全文件名: {error}"),
        }
    })?;
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!("测试名只允许小写 ASCII 字母、数字和连字符: {name:?}"),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn validate_test_properties(
    test: &CTestCase,
    build_root: &ToolRoot,
    executable: &Path,
) -> Result<(), NativeTestRunnerError> {
    let disabled = unique_property(test, "DISABLED")?.as_bool() == Some(true);
    let labelled = unique_property(test, "LABELS")?
        .as_array()
        .is_some_and(|labels| {
            labels
                .iter()
                .any(|label| label.as_str() == Some(ANDROID_DEVICE_LABEL))
        });
    let timeout = unique_property(test, "TIMEOUT")?.as_f64() == Some(CTEST_TIMEOUT_SECONDS);
    let working_directory = unique_property(test, "WORKING_DIRECTORY")?
        .as_str()
        .ok_or_else(|| NativeTestRunnerError::InvalidInventory {
            message: format!("测试 {} 的 WORKING_DIRECTORY 必须是文本", test.name),
        })?;
    let canonical_working_directory =
        fs::canonicalize(working_directory).map_err(|source| NativeTestRunnerError::Io {
            path: PathBuf::from(working_directory),
            source,
        })?;
    let executable_directory =
        executable
            .parent()
            .ok_or_else(|| NativeTestRunnerError::InvalidInventory {
                message: format!("测试 {} 的 ELF 缺少父目录", test.name),
            })?;
    let working_directory_matches =
        build_root.canonical_paths_equal(&canonical_working_directory, executable_directory);
    if !disabled || !labelled || !timeout || !working_directory_matches {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!(
                "测试 {} 必须主机禁用、仅带 {ANDROID_DEVICE_LABEL} 标签、TIMEOUT=180，且工作目录等于 ELF 目录",
                test.name
            ),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn unique_property<'a>(
    test: &'a CTestCase,
    name: &str,
) -> Result<&'a Value, NativeTestRunnerError> {
    let matches: Vec<&CTestProperty> = test
        .properties
        .iter()
        .filter(|property| property.name == name)
        .collect();
    if matches.len() != 1 {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!("测试 {} 必须包含唯一 {name} 属性", test.name),
        });
    }
    Ok(&matches[0].value)
}

#[cfg(any(target_os = "windows", test))]
fn resolve_inventory_artifact(
    build_root: &ToolRoot,
    raw: &str,
) -> Result<(String, PathBuf), NativeTestRunnerError> {
    let requested = PathBuf::from(raw);
    if !requested.is_absolute() {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 产物路径必须是绝对路径: {raw:?}"),
        });
    }
    let canonical = fs::canonicalize(&requested).map_err(|source| NativeTestRunnerError::Io {
        path: requested.clone(),
        source,
    })?;
    if !build_root.contains_canonical_path(&canonical) {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 产物越出构建目录: {}", canonical.display()),
        });
    }
    let filename = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 产物文件名不是有效 Unicode: {}", canonical.display()),
        })?
        .to_owned();
    validate_artifact_filename(&filename)?;
    let expected_relative = Path::new("tests").join(&filename);
    let verified = build_root
        .existing_file(&expected_relative)
        .map_err(|error| NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 产物不是受控普通文件: {error}"),
        })?;
    if !build_root.canonical_paths_equal(&canonical, &verified) {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!(
                "CTest 产物必须直接位于构建目录 tests/ 下: {}",
                canonical.display()
            ),
        });
    }
    Ok((filename, verified))
}

#[cfg(any(target_os = "windows", test))]
fn insert_artifact(
    artifacts: &mut HashMap<String, PathBuf>,
    artifact: (String, PathBuf),
) -> Result<(), NativeTestRunnerError> {
    match artifacts.get(&artifact.0) {
        Some(existing) if existing != &artifact.1 => Err(NativeTestRunnerError::InvalidInventory {
            message: format!("不同 CTest 产物共享文件名 {}", artifact.0),
        }),
        Some(_) => Ok(()),
        None => {
            artifacts.insert(artifact.0, artifact.1);
            Ok(())
        }
    }
}

#[cfg(any(target_os = "windows", test))]
fn validate_artifact_filename(filename: &str) -> Result<(), NativeTestRunnerError> {
    validate_portable_component(Path::new(filename), filename.as_ref()).map_err(|error| {
        NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 产物文件名 {filename:?} 无效: {error}"),
        }
    })?;
    if filename.is_empty()
        || !filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(NativeTestRunnerError::InvalidInventory {
            message: format!("CTest 产物文件名包含不安全字符: {filename:?}"),
        });
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
pub(super) fn validate_x86_64_elf(
    path: &Path,
    require_android_interpreter: bool,
) -> Result<(), NativeTestRunnerError> {
    let mut file = File::open(path).map_err(|source| NativeTestRunnerError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file_size = file
        .metadata()
        .map_err(|source| NativeTestRunnerError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    let mut header = [0_u8; ELF64_HEADER_BYTES];
    file.read_exact(&mut header)
        .map_err(|source| NativeTestRunnerError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let object_type = u16::from_le_bytes([header[16], header[17]]);
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let version = u32::from_le_bytes(header[20..24].try_into().unwrap());
    let program_header_offset = u64::from_le_bytes(header[32..40].try_into().unwrap());
    let elf_header_size = u16::from_le_bytes([header[52], header[53]]);
    let program_header_size = u16::from_le_bytes([header[54], header[55]]);
    let program_header_count = u16::from_le_bytes([header[56], header[57]]);
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || header[6] != 1
        || object_type != 3
        || machine != 62
        || version != 1
        || elf_header_size as usize != ELF64_HEADER_BYTES
        || program_header_size != ELF64_PROGRAM_HEADER_BYTES
        || program_header_count == 0
        || program_header_count > MAXIMUM_PROGRAM_HEADERS
    {
        return Err(NativeTestRunnerError::InvalidElf {
            path: path.to_path_buf(),
            message: "产物必须是带规范 program headers 的 little-endian ELF64 x86_64 ET_DYN"
                .to_owned(),
        });
    }
    u64::from(program_header_size)
        .checked_mul(u64::from(program_header_count))
        .and_then(|size| program_header_offset.checked_add(size))
        .filter(|end| *end <= file_size)
        .ok_or_else(|| NativeTestRunnerError::InvalidElf {
            path: path.to_path_buf(),
            message: "program headers 越出文件边界".to_owned(),
        })?;

    let mut has_load_segment = false;
    let mut interpreter: Option<Vec<u8>> = None;
    for index in 0..program_header_count {
        let offset = program_header_offset + u64::from(index) * u64::from(program_header_size);
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| NativeTestRunnerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut program_header = [0_u8; ELF64_PROGRAM_HEADER_BYTES as usize];
        file.read_exact(&mut program_header)
            .map_err(|source| NativeTestRunnerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let kind = u32::from_le_bytes(program_header[..4].try_into().unwrap());
        if kind == 1 {
            let data_offset = u64::from_le_bytes(program_header[8..16].try_into().unwrap());
            let data_size = u64::from_le_bytes(program_header[32..40].try_into().unwrap());
            if data_offset
                .checked_add(data_size)
                .is_none_or(|end| end > file_size)
            {
                return Err(NativeTestRunnerError::InvalidElf {
                    path: path.to_path_buf(),
                    message: "PT_LOAD 越出文件边界".to_owned(),
                });
            }
            has_load_segment = true;
            continue;
        }
        if kind != 3 {
            continue;
        }
        if interpreter.is_some() {
            return Err(NativeTestRunnerError::InvalidElf {
                path: path.to_path_buf(),
                message: "产物包含重复 PT_INTERP".to_owned(),
            });
        }
        let data_offset = u64::from_le_bytes(program_header[8..16].try_into().unwrap());
        let data_size = u64::from_le_bytes(program_header[32..40].try_into().unwrap());
        if data_size == 0
            || data_size > 256
            || data_offset
                .checked_add(data_size)
                .is_none_or(|end| end > file_size)
        {
            return Err(NativeTestRunnerError::InvalidElf {
                path: path.to_path_buf(),
                message: "PT_INTERP 越出文件边界".to_owned(),
            });
        }
        let mut value = vec![0_u8; usize::try_from(data_size).unwrap()];
        file.seek(SeekFrom::Start(data_offset))
            .and_then(|_| file.read_exact(&mut value))
            .map_err(|source| NativeTestRunnerError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        interpreter = Some(value);
    }
    let interpreter_matches = interpreter.as_deref() == Some(ANDROID_LINKER64);
    if !has_load_segment
        || (require_android_interpreter && !interpreter_matches)
        || (!require_android_interpreter && interpreter.is_some())
    {
        return Err(NativeTestRunnerError::InvalidElf {
            path: path.to_path_buf(),
            message: if require_android_interpreter {
                "测试 ELF 必须包含 PT_LOAD 和 /system/bin/linker64 PT_INTERP".to_owned()
            } else {
                "fixture 共享库必须包含 PT_LOAD 且不得包含 PT_INTERP".to_owned()
            },
        });
    }
    Ok(())
}

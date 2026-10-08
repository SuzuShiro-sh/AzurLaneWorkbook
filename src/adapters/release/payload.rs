//! 把发布旁路文件附加到主程序，并在工具根目录按策略解出。

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use super::super::tool_root::{ToolRoot, ToolRootError};
use super::{
    MANIFEST_RELATIVE_PATH, RELEASE_FILE_SPECS, ReleaseError, ReleaseFileEntry, ReleaseFilePolicy,
    ReleaseFileSpec, ReleaseManifest,
};
use crate::adapters::MAXIMUM_RELEASE_FILE_BYTES;
use suzushiro_content_digest::{sha256_bytes, sha256_file};

const PAYLOAD_MAGIC: &[u8; 8] = b"AZLWPK01";
const TRAILER_LEN: u64 = 24;
const EXECUTABLE_PATH: &str = "AzurLaneWorkbook.exe";

/// 将资源根内的旁路发布文件打包进安装目录中的主程序。
pub(super) fn embed_sidecar_payload(
    resource_root: &ToolRoot,
    executable: &Path,
) -> Result<(), ReleaseError> {
    let unpacked = fs::read(executable).map_err(|source| ReleaseError::Io {
        stage: "release.pack_payload.read_executable",
        path: executable.to_path_buf(),
        source,
    })?;
    let mut files = BTreeMap::new();
    for specification in sidecar_specs() {
        let path = resource_root.existing_file(Path::new(specification.path()))?;
        let bytes = fs::read(&path).map_err(|source| ReleaseError::Io {
            stage: "release.pack_payload.read_sidecar",
            path: path.clone(),
            source,
        })?;
        files.insert(specification.path().to_owned(), bytes);
    }
    let packed = pack_executable(&unpacked, &files)?;
    fs::write(executable, packed).map_err(|source| ReleaseError::Io {
        stage: "release.pack_payload.write_executable",
        path: executable.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// 若当前程序带有发布载荷，则按文件策略解到工具根目录。
pub fn ensure_extracted_release(
    tool_root: &ToolRoot,
    executable: &Path,
) -> Result<(), ReleaseError> {
    let Some(files) = read_attached_payload(executable)? else {
        return Ok(());
    };
    // 同一资源根的启动者串行检查和解包，锁保持到清单写入完成。
    let _lock = tool_root
        .lock_file(Path::new("release.lock"), std::time::Duration::from_secs(5))
        .map_err(|source| ReleaseError::Io {
            stage: "release.extract_payload.lock",
            path: tool_root.as_path().join("release.lock"),
            source,
        })?;
    let mut wrote = false;
    for specification in sidecar_specs() {
        let Some(bytes) = files.get(specification.path()) else {
            return Err(ReleaseError::InvalidManifest {
                field: format!("payload.{}", specification.path()),
                message: "主程序载荷缺少固定发布文件".to_owned(),
            });
        };
        if extract_sidecar(tool_root, specification, bytes)? {
            wrote = true;
        }
    }
    let manifest_path = Path::new(MANIFEST_RELATIVE_PATH);
    if wrote || tool_root.existing_file(manifest_path).is_err() {
        write_extracted_manifest(tool_root)?;
    }
    Ok(())
}

fn sidecar_specs() -> impl Iterator<Item = ReleaseFileSpec<'static>> {
    RELEASE_FILE_SPECS.iter().copied()
}

fn pack_executable(
    unpacked_exe: &[u8],
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<u8>, ReleaseError> {
    let mut zip_bytes = Vec::new();
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut zip_bytes));
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        for (name, bytes) in files {
            validate_payload_entry(name, bytes.len() as u64)?;
            zip.start_file(name, options).map_err(|source| {
                zip_io_error("release.pack_payload.create_entry", name, source)
            })?;
            zip.write_all(bytes).map_err(|source| ReleaseError::Io {
                stage: "release.pack_payload.write_entry",
                path: PathBuf::from(name),
                source,
            })?;
        }
        zip.finish().map_err(|source| {
            zip_io_error("release.pack_payload.finish", EXECUTABLE_PATH, source)
        })?;
    }
    let zip_offset = unpacked_exe.len() as u64;
    let zip_size = zip_bytes.len() as u64;
    let mut packed =
        Vec::with_capacity(unpacked_exe.len() + zip_bytes.len() + TRAILER_LEN as usize);
    packed.extend_from_slice(unpacked_exe);
    packed.extend_from_slice(&zip_bytes);
    packed.extend_from_slice(&encode_trailer(zip_offset, zip_size));
    Ok(packed)
}

fn read_attached_payload(
    executable: &Path,
) -> Result<Option<BTreeMap<String, Vec<u8>>>, ReleaseError> {
    let bytes = fs::read(executable).map_err(|source| ReleaseError::Io {
        stage: "release.extract_payload.read_executable",
        path: executable.to_path_buf(),
        source,
    })?;
    let Some((zip_offset, zip_size)) = parse_trailer(&bytes) else {
        return Ok(None);
    };
    let zip_end =
        zip_offset
            .checked_add(zip_size)
            .ok_or_else(|| ReleaseError::InvalidManifest {
                field: "payload.zip".to_owned(),
                message: "主程序载荷长度溢出".to_owned(),
            })?;
    if zip_end > bytes.len() as u64 {
        return Err(ReleaseError::InvalidManifest {
            field: "payload.zip".to_owned(),
            message: "主程序载荷范围超出程序文件".to_owned(),
        });
    }
    let zip_slice = &bytes[zip_offset as usize..zip_end as usize];
    let mut archive = ZipArchive::new(Cursor::new(zip_slice)).map_err(|source| {
        zip_io_error("release.extract_payload.open_zip", EXECUTABLE_PATH, source)
    })?;
    let mut files = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|source| {
            zip_io_error(
                "release.extract_payload.read_entry",
                EXECUTABLE_PATH,
                source,
            )
        })?;
        if entry.enclosed_name().is_none() {
            return Err(ReleaseError::InvalidManifest {
                field: "payload.entry".to_owned(),
                message: "主程序载荷包含非法路径".to_owned(),
            });
        }
        let name = entry.name().replace('\\', "/");
        validate_payload_entry(&name, entry.size())?;
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|source| ReleaseError::Io {
                stage: "release.extract_payload.read_entry_bytes",
                path: PathBuf::from(&name),
                source,
            })?;
        if content.len() as u64 != entry.size() {
            return Err(ReleaseError::InvalidManifest {
                field: format!("payload.{name}"),
                message: "主程序载荷条目长度与声明不一致".to_owned(),
            });
        }
        files.insert(name, content);
    }
    Ok(Some(files))
}

fn extract_sidecar(
    tool_root: &ToolRoot,
    specification: ReleaseFileSpec<'_>,
    bytes: &[u8],
) -> Result<bool, ReleaseError> {
    let relative = Path::new(specification.path());
    let expected = sha256_bytes(bytes);
    match tool_root.existing_file(relative) {
        Ok(path) => {
            let actual = sha256_file(&path).map_err(|source| ReleaseError::Io {
                stage: "release.extract_payload.hash_existing",
                path: path.clone(),
                source,
            })?;
            if actual == expected {
                return Ok(false);
            }
            if specification.policy() == ReleaseFilePolicy::MutableConfig {
                return Ok(false);
            }
            tool_root.remove_file_if_exists(relative, None)?;
        }
        Err(source) => {
            if !is_not_found(&source) {
                return Err(source.into());
            }
        }
    }
    write_sidecar(tool_root, relative, bytes)?;
    Ok(true)
}

fn write_sidecar(tool_root: &ToolRoot, relative: &Path, bytes: &[u8]) -> Result<(), ReleaseError> {
    if let Some(parent) = relative
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tool_root.ensure_directory(parent)?;
    }
    let destination = tool_root.prepare_new_file(relative)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|source| ReleaseError::Io {
            stage: "release.extract_payload.create_file",
            path: destination.clone(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| ReleaseError::Io {
        stage: "release.extract_payload.write_file",
        path: destination,
        source,
    })?;
    Ok(())
}

fn write_extracted_manifest(tool_root: &ToolRoot) -> Result<(), ReleaseError> {
    let entries: Vec<ReleaseFileEntry> = RELEASE_FILE_SPECS
        .iter()
        .map(|specification| ReleaseFileEntry::from_file(tool_root, *specification))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = ReleaseManifest::new(entries);
    let bytes = manifest.to_pretty_bytes()?;
    match tool_root.existing_file(Path::new(MANIFEST_RELATIVE_PATH)) {
        Ok(_) => {
            tool_root.remove_file_if_exists(Path::new(MANIFEST_RELATIVE_PATH), None)?;
        }
        Err(source) if is_not_found(&source) => {}
        Err(source) => return Err(source.into()),
    }
    write_sidecar(tool_root, Path::new(MANIFEST_RELATIVE_PATH), &bytes)
}

fn validate_payload_entry(name: &str, size: u64) -> Result<(), ReleaseError> {
    if name == EXECUTABLE_PATH || name == MANIFEST_RELATIVE_PATH {
        return Err(ReleaseError::InvalidManifest {
            field: format!("payload.{name}"),
            message: "主程序载荷不能包含主程序或清单".to_owned(),
        });
    }
    if !sidecar_specs().any(|specification| specification.path() == name) {
        return Err(ReleaseError::InvalidManifest {
            field: format!("payload.{name}"),
            message: "主程序载荷包含未登记发布文件".to_owned(),
        });
    }
    if size > MAXIMUM_RELEASE_FILE_BYTES {
        return Err(ReleaseError::InvalidManifest {
            field: format!("payload.{name}"),
            message: format!("主程序载荷条目超过 {MAXIMUM_RELEASE_FILE_BYTES} 字节上限"),
        });
    }
    Ok(())
}

fn encode_trailer(zip_offset: u64, zip_size: u64) -> [u8; TRAILER_LEN as usize] {
    let mut trailer = [0_u8; TRAILER_LEN as usize];
    trailer[..8].copy_from_slice(PAYLOAD_MAGIC);
    trailer[8..16].copy_from_slice(&zip_offset.to_le_bytes());
    trailer[16..24].copy_from_slice(&zip_size.to_le_bytes());
    trailer
}

fn parse_trailer(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < TRAILER_LEN as usize {
        return None;
    }
    let trailer = &bytes[bytes.len() - TRAILER_LEN as usize..];
    if trailer[..8] != *PAYLOAD_MAGIC {
        return None;
    }
    let zip_offset = u64::from_le_bytes(trailer[8..16].try_into().ok()?);
    let zip_size = u64::from_le_bytes(trailer[16..24].try_into().ok()?);
    Some((zip_offset, zip_size))
}

fn zip_io_error(stage: &'static str, path: &str, source: zip::result::ZipError) -> ReleaseError {
    ReleaseError::Io {
        stage,
        path: PathBuf::from(path),
        source: std::io::Error::other(source),
    }
}

fn is_not_found(error: &ToolRootError) -> bool {
    matches!(
        error,
        ToolRootError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_payload_roundtrip_preserves_sidecar_bytes() {
        let mut files = BTreeMap::new();
        files.insert("settings.json".to_owned(), b"settings".to_vec());
        files.insert(
            "runtime/adb/NOTICE.txt".to_owned(),
            b"notice-bytes".to_vec(),
        );
        let packed = pack_executable(b"unpacked-exe", &files).unwrap();
        let directory = scratch_directory("payload-roundtrip");
        let executable = directory.join(EXECUTABLE_PATH);
        fs::write(&executable, packed).unwrap();
        let restored = read_attached_payload(&executable).unwrap().unwrap();
        assert_eq!(
            restored.get("settings.json").map(Vec::as_slice),
            Some(b"settings".as_slice())
        );
        assert_eq!(
            restored.get("runtime/adb/NOTICE.txt").map(Vec::as_slice),
            Some(b"notice-bytes".as_slice())
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn unpacked_executable_has_no_payload() {
        let directory = scratch_directory("payload-absent");
        let executable = directory.join(EXECUTABLE_PATH);
        fs::write(&executable, b"plain-exe").unwrap();
        assert!(read_attached_payload(&executable).unwrap().is_none());
        let _ = fs::remove_dir_all(&directory);
    }

    fn scratch_directory(label: &str) -> PathBuf {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).unwrap();
        let directory = PathBuf::from(
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap(),
        )
        .join("suzushiro/scratch/azlw-payload-tests")
        .join(format!(
            "{label}-{}-{:032x}",
            std::process::id(),
            u128::from_le_bytes(bytes)
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }
}

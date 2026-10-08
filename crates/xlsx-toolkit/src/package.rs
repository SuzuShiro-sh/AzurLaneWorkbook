//! 受限 OOXML ZIP 包读取和关系解析原语。

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use zip::read::ZipFile;
use zip::write::{FullFileOptions, SimpleFileOptions, StreamWriter};
use zip::{CompressionMethod, System, ZipArchive, ZipWriter};

use suzushiro_controlled_root::has_link_semantics;

use super::XlsxError;

mod ooxml;
mod zip_preflight;

pub use ooxml::{
    PackageRelationship, optional_attribute, parse_relationships, required_attribute,
    resolve_relationship_target,
};
use zip_preflight::open_preflight_zip_archive;

const MAX_ENTRY_COUNT: usize = 4_096;
pub const MAX_PART_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_RAW_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;

/// 通过固定文件句柄读取受限工作簿字节，拒绝最终路径链接并限制内存分配。
pub fn read_bounded_workbook_bytes(
    path: &Path,
    maximum_bytes: u64,
    description: &'static str,
) -> Result<Vec<u8>, XlsxError> {
    let file = open_workbook_file(path).map_err(|source| bounded_open_error(path, source))?;
    let metadata = file.metadata().map_err(|source| XlsxError::Io {
        stage: "检查已打开工作簿文件",
        path: path.to_path_buf(),
        source,
    })?;
    if has_link_semantics(&metadata) || !metadata.is_file() {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: format!("{description}必须是普通文件且不能是链接或重解析点"),
        });
    }
    if metadata.len() == 0 || metadata.len() > maximum_bytes {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: format!("{description}必须为 1 至 {maximum_bytes} 字节"),
        });
    }

    let read_limit = maximum_bytes
        .checked_add(1)
        .ok_or_else(|| XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: format!("{description}读取上限溢出"),
        })?;
    let mut reader = file.take(read_limit);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|source| XlsxError::Io {
            stage: "读取受限工作簿文件",
            path: path.to_path_buf(),
            source,
        })?;
    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if bytes.is_empty() || byte_count > maximum_bytes {
        return Err(XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: format!("{description}必须为 1 至 {maximum_bytes} 字节"),
        });
    }
    Ok(bytes)
}

/// 排他写入并同步新文件，返回创建句柄供后续发布核对文件身份。
pub fn write_new_file_bytes(path: &Path, bytes: &[u8]) -> Result<File, XlsxError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| XlsxError::Io {
            stage: "建立",
            path: path.to_path_buf(),
            source,
        })?;
    let write_result = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| XlsxError::Io {
            stage: "写入并同步",
            path: path.to_path_buf(),
            source,
        });
    if let Err(operation) = write_result {
        drop(file);
        return Err(cleanup_created_file(path, operation));
    }
    Ok(file)
}

/// 以平台原生的“不跟随最终链接”标志打开工作簿文件。
fn open_workbook_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// 将平台“不跟随链接”失败统一为稳定的路径错误。
fn bounded_open_error(path: &Path, source: std::io::Error) -> XlsxError {
    #[cfg(unix)]
    if source.raw_os_error() == Some(libc::ELOOP) {
        return XlsxError::InvalidPath {
            path: path.to_path_buf(),
            message: "最终路径不能是符号链接".to_owned(),
        };
    }
    XlsxError::Io {
        stage: "打开受限工作簿文件",
        path: path.to_path_buf(),
        source,
    }
}

/// 内存中的受限 OOXML 包及其稳定条目顺序。
#[derive(Clone, Debug)]
pub struct PackageSnapshot {
    entries: Vec<PackageEntry>,
    indexes: BTreeMap<String, usize>,
}

/// 单个 ZIP 条目的目录标记和解压内容。
#[derive(Clone, Debug)]
pub struct PackageEntry {
    pub is_directory: bool,
    pub bytes: Vec<u8>,
}

/// 重写包时追加的固定新部件。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageAddition {
    pub name: String,
    pub bytes: Vec<u8>,
}

impl PackageSnapshot {
    /// 读取全部条目并执行路径、加密、压缩和解压大小边界检查。
    pub fn read(path: &Path) -> Result<Self, XlsxError> {
        let bytes = read_bounded_workbook_bytes(path, MAX_RAW_PACKAGE_BYTES, "工作簿文件")?;
        Self::from_bytes(&bytes, path)
    }

    /// 从调用方已经固定的原始字节读取包，供多套解析器共享同一文件快照。
    pub fn from_bytes(bytes: &[u8], path: &Path) -> Result<Self, XlsxError> {
        Self::read_archive(bytes, path)
    }

    /// 在 ZIP 依赖分配中央目录前执行资源预检，再读取全部受限条目。
    fn read_archive(bytes: &[u8], path: &Path) -> Result<Self, XlsxError> {
        let (_, mut archive) = open_preflight_zip_archive(bytes, path, "读取")?;
        if archive.len() > MAX_ENTRY_COUNT {
            return Err(invalid_part(
                "[Content_Types].xml",
                format!("ZIP 条目数 {} 超过上限 {MAX_ENTRY_COUNT}", archive.len()),
            ));
        }

        let mut entries: Vec<PackageEntry> = Vec::with_capacity(archive.len());
        let mut indexes: BTreeMap<String, usize> = BTreeMap::new();
        let mut total_bytes: u64 = 0;
        for index in 0..archive.len() {
            let source_entry =
                archive
                    .by_index(index)
                    .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                        stage: "读取条目",
                        path: path.to_path_buf(),
                        source,
                    })?;
            let name: String = source_entry.name().to_owned();
            validate_part_name(&name, source_entry.is_dir())?;
            if source_entry.encrypted() {
                return Err(invalid_part(&name, "不允许加密 ZIP 条目"));
            }
            if source_entry.size() > MAX_PART_BYTES {
                return Err(invalid_part(
                    &name,
                    format!(
                        "解压大小 {} 超过单部件上限 {MAX_PART_BYTES}",
                        source_entry.size()
                    ),
                ));
            }
            total_bytes = total_bytes
                .checked_add(source_entry.size())
                .ok_or_else(|| invalid_part(&name, "累计解压大小溢出"))?;
            if total_bytes > MAX_PACKAGE_BYTES {
                return Err(invalid_part(
                    &name,
                    format!("累计解压大小超过上限 {MAX_PACKAGE_BYTES}"),
                ));
            }
            if indexes.contains_key(&name) {
                return Err(invalid_part(&name, "ZIP 条目名称重复"));
            }
            let compression: zip::CompressionMethod = source_entry.compression();
            if compression != zip::CompressionMethod::Stored
                && compression != zip::CompressionMethod::Deflated
            {
                return Err(invalid_part(
                    &name,
                    format!("不支持压缩方式 {compression:?}"),
                ));
            }
            let is_directory: bool = source_entry.is_dir();
            let expected_size: usize = usize::try_from(source_entry.size())
                .map_err(|_| invalid_part(&name, "条目大小无法在当前平台表示"))?;
            let mut bytes: Vec<u8> = Vec::with_capacity(expected_size);
            // 多读一个字节即可确认声明不实，避免先完整展开恶意压缩数据。
            let read_limit = source_entry.size() + 1;
            source_entry
                .take(read_limit)
                .read_to_end(&mut bytes)
                .map_err(|source: std::io::Error| XlsxError::Io {
                    stage: "解压条目",
                    path: PathBuf::from(&name),
                    source,
                })?;
            if bytes.len() != expected_size {
                return Err(invalid_part(
                    &name,
                    format!("声明大小 {expected_size} 与实际大小 {} 不一致", bytes.len()),
                ));
            }
            indexes.insert(name.clone(), entries.len());
            entries.push(PackageEntry {
                is_directory,
                bytes,
            });
        }

        let package: Self = Self { entries, indexes };
        package.part("[Content_Types].xml")?;
        package.part("_rels/.rels")?;
        package.part("xl/workbook.xml")?;
        package.part("xl/_rels/workbook.xml.rels")?;
        Ok(package)
    }

    /// 返回非目录部件的原始解压字节。
    pub fn part(&self, name: &str) -> Result<&[u8], XlsxError> {
        let entry: &PackageEntry = self.entry(name)?;
        if entry.is_directory {
            return Err(invalid_part(name, "目标是目录条目"));
        }
        Ok(&entry.bytes)
    }

    /// 返回指定 ZIP 条目，包括目录标记和解压内容。
    pub fn entry(&self, name: &str) -> Result<&PackageEntry, XlsxError> {
        let index: usize = *self
            .indexes
            .get(name)
            .ok_or_else(|| XlsxError::MissingPart {
                part: name.to_owned(),
            })?;
        Ok(&self.entries[index])
    }

    /// 按名称稳定排序返回包内全部条目名。
    pub fn entry_names(&self) -> impl Iterator<Item = &str> + '_ {
        self.indexes.keys().map(String::as_str)
    }

    /// 返回包内全部条目数量。
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

/// 原样复制未修改条目，只重新压缩明确替换和追加的部件。
pub fn rewrite_package<R, W>(
    archive: ZipArchive<R>,
    destination: W,
    source_path: &Path,
    destination_path: &Path,
    replacements: &BTreeMap<String, Vec<u8>>,
    additions: &[PackageAddition],
) -> Result<W, XlsxError>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let archive_comment: Box<[u8]> = archive.comment().to_vec().into_boxed_slice();
    rewrite_package_with_comment(
        archive,
        destination,
        source_path,
        destination_path,
        replacements,
        additions,
        archive_comment,
    )
}

/// 从已固定的受限字节快照安全重写包，并原样保留真实 EOCD 注释。
pub fn rewrite_package_from_bytes<W>(
    bytes: &[u8],
    destination: W,
    source_path: &Path,
    destination_path: &Path,
    replacements: &BTreeMap<String, Vec<u8>>,
    additions: &[PackageAddition],
) -> Result<W, XlsxError>
where
    W: Write + Seek,
{
    let (preflight, archive) = open_preflight_zip_archive(bytes, source_path, "打开待重写包")?;
    let archive_comment = preflight.archive_comment(bytes).to_vec().into_boxed_slice();
    rewrite_package_with_comment(
        archive,
        destination,
        source_path,
        destination_path,
        replacements,
        additions,
        archive_comment,
    )
}

/// 使用调用方确认的原始归档注释复制、替换并追加 ZIP 条目。
fn rewrite_package_with_comment<R, W>(
    mut archive: ZipArchive<R>,
    destination: W,
    source_path: &Path,
    destination_path: &Path,
    replacements: &BTreeMap<String, Vec<u8>>,
    additions: &[PackageAddition],
    archive_comment: Box<[u8]>,
) -> Result<W, XlsxError>
where
    R: Read + Seek,
    W: Write + Seek,
{
    let existing_names: BTreeSet<String> = archive.file_names().map(str::to_owned).collect();
    validate_rewrite_plan(&existing_names, replacements, additions, destination_path)?;

    let entry_count: usize = archive.len();
    let mut writer: ZipWriter<W> = ZipWriter::new(destination);
    writer
        .set_raw_comment(archive_comment)
        .map_err(|source: zip::result::ZipError| XlsxError::Zip {
            stage: "保留归档注释",
            path: destination_path.to_path_buf(),
            source,
        })?;

    for index in 0..entry_count {
        let source_entry: ZipFile<'_, R> =
            archive
                .by_index(index)
                .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                    stage: "读取待复制条目",
                    path: source_path.to_path_buf(),
                    source,
                })?;
        let name: String = source_entry.name().to_owned();
        if let Some(bytes) = replacements.get(&name) {
            if source_entry.is_dir() {
                return Err(invalid_part(&name, "不允许替换目录条目"));
            }
            let comment: String = source_entry.comment().to_owned();
            let mut options: FullFileOptions<'_> = source_entry.options().into_full_options();
            if !comment.is_empty() {
                options = options.with_file_comment(comment);
            }
            writer
                .start_file(&name, options)
                .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                    stage: "开始写入替换部件",
                    path: destination_path.to_path_buf(),
                    source,
                })?;
            writer
                .write_all(bytes)
                .map_err(|source: std::io::Error| XlsxError::Io {
                    stage: "写入替换部件",
                    path: destination_path.to_path_buf(),
                    source,
                })?;
        } else {
            writer
                .raw_copy_file(source_entry)
                .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                    stage: "原样复制未修改部件",
                    path: destination_path.to_path_buf(),
                    source,
                })?;
        }
    }

    for addition in additions {
        let options: SimpleFileOptions = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(zip::DateTime::default());
        writer
            .start_file(&addition.name, options)
            .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                stage: "开始写入追加部件",
                path: destination_path.to_path_buf(),
                source,
            })?;
        writer
            .write_all(&addition.bytes)
            .map_err(|source: std::io::Error| XlsxError::Io {
                stage: "写入追加部件",
                path: destination_path.to_path_buf(),
                source,
            })?;
    }

    writer
        .finish()
        .map_err(|source: zip::result::ZipError| XlsxError::Zip {
            stage: "结束写出",
            path: destination_path.to_path_buf(),
            source,
        })
}

/// 规范化生成产物的 ZIP 元数据，使同一 OOXML 内容在各宿主平台产生相同字节。
pub fn canonicalize_generated_package(bytes: &[u8], path: &Path) -> Result<Vec<u8>, XlsxError> {
    // 复用统一的条目名、压缩方式和解压大小边界，不为生成器另设一套包解析规则。
    drop(PackageSnapshot::from_bytes(bytes, path)?);

    let (preflight, mut archive) = open_preflight_zip_archive(bytes, path, "打开待规范化生成包")?;
    let archive_comment = preflight.archive_comment(bytes).to_vec().into_boxed_slice();
    let entry_count: usize = archive.len();
    let mut writer: ZipWriter<StreamWriter<Vec<u8>>> =
        ZipWriter::new_stream(Vec::with_capacity(bytes.len()));
    writer
        .set_raw_comment(archive_comment)
        .map_err(|source: zip::result::ZipError| XlsxError::Zip {
            stage: "保留待规范化生成包注释",
            path: path.to_path_buf(),
            source,
        })?;

    for index in 0..entry_count {
        let mut source_entry =
            archive
                .by_index(index)
                .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                    stage: "读取待规范化生成部件",
                    path: path.to_path_buf(),
                    source,
                })?;
        let name: String = source_entry.name().to_owned();
        let is_directory: bool = source_entry.is_dir();
        let comment: String = source_entry.comment().to_owned();
        // zip 的公共读取选项不携带原始扩展字段和注释编码，遇到生成器契约外的
        // 元数据时显式失败，避免规范化过程悄悄改变包内容。
        if !name.is_ascii() || source_entry.name_raw() != name.as_bytes() {
            return Err(invalid_part(&name, "生成包部件名必须使用 ASCII 编码"));
        }
        if source_entry
            .extra_data()
            .is_some_and(|extra_data| !extra_data.is_empty())
            || source_entry.extra_data_fields().next().is_some()
        {
            return Err(invalid_part(&name, "生成包部件不允许包含 ZIP 扩展字段"));
        }
        if !comment.is_ascii() {
            return Err(invalid_part(&name, "生成包部件注释必须使用 ASCII 编码"));
        }
        if is_directory && source_entry.size() != 0 {
            return Err(invalid_part(&name, "生成包目录条目不允许包含数据"));
        }
        let permissions: u32 = if is_directory { 0o700 } else { 0o600 };
        let mut options: FullFileOptions<'_> = source_entry
            .options()
            .system(System::Unix)
            .unix_permissions(permissions)
            .last_modified_time(zip::DateTime::default())
            .large_file(false)
            .into_full_options();
        if !comment.is_empty() {
            options = options.with_file_comment(comment);
        }
        let mut content: Vec<u8> = Vec::new();
        if !is_directory {
            let expected_size: usize = usize::try_from(source_entry.size())
                .map_err(|_| invalid_part(&name, "生成部件大小无法在当前平台表示"))?;
            content.reserve(expected_size);
            source_entry
                .read_to_end(&mut content)
                .map_err(|source: std::io::Error| XlsxError::Io {
                    stage: "解压待规范化生成部件",
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        drop(source_entry);

        if is_directory {
            writer
                .add_directory(&name, options)
                .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                    stage: "写入规范化生成目录",
                    path: path.to_path_buf(),
                    source,
                })?;
        } else {
            writer
                .start_file(&name, options)
                .map_err(|source: zip::result::ZipError| XlsxError::Zip {
                    stage: "开始写入规范化生成部件",
                    path: path.to_path_buf(),
                    source,
                })?;
            writer
                .write_all(&content)
                .map_err(|source: std::io::Error| XlsxError::Io {
                    stage: "写入规范化生成部件",
                    path: path.to_path_buf(),
                    source,
                })?;
        }
    }

    writer
        .finish()
        .map(StreamWriter::into_inner)
        .map_err(|source: zip::result::ZipError| XlsxError::Zip {
            stage: "结束规范化生成包",
            path: path.to_path_buf(),
            source,
        })
}

/// 在开始写出前确认替换项存在且追加项不会覆盖现有条目。
fn validate_rewrite_plan(
    existing_names: &BTreeSet<String>,
    replacements: &BTreeMap<String, Vec<u8>>,
    additions: &[PackageAddition],
    destination_path: &Path,
) -> Result<(), XlsxError> {
    for name in replacements.keys() {
        if !existing_names.contains(name) {
            return Err(XlsxError::MissingPart { part: name.clone() });
        }
    }
    let mut planned_names: BTreeSet<&str> = existing_names.iter().map(String::as_str).collect();
    for addition in additions {
        validate_part_name(&addition.name, false)?;
        if !planned_names.insert(&addition.name) {
            return Err(XlsxError::InvalidPath {
                path: destination_path.to_path_buf(),
                message: format!("追加部件 {} 与已有条目重名", addition.name),
            });
        }
    }
    Ok(())
}

/// 清理建立失败的文件，并在清理也失败时保留两层错误上下文。
pub fn cleanup_created_file<E: From<XlsxError> + std::fmt::Display>(
    path: &Path,
    operation: E,
) -> E {
    match std::fs::remove_file(path) {
        Ok(()) => operation,
        Err(source) => XlsxError::CleanupFailed {
            path: path.to_path_buf(),
            operation: operation.to_string(),
            source,
        }
        .into(),
    }
}

/// 检查 ZIP 条目名没有绝对路径、父目录或平台前缀。
fn validate_part_name(name: &str, is_directory: bool) -> Result<(), XlsxError> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return Err(invalid_part(name, "ZIP 条目名为空或包含非法字符"));
    }
    if is_directory != name.ends_with('/') {
        return Err(invalid_part(name, "目录标记与条目名不一致"));
    }
    let normalized_name: &str = name.strip_suffix('/').unwrap_or(name);
    if normalized_name.is_empty() {
        return Err(invalid_part(name, "不允许包根目录条目"));
    }
    let path: &Path = Path::new(normalized_name);
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            _ => return Err(invalid_part(name, "ZIP 条目路径不规范")),
        }
    }
    Ok(())
}

/// 建立带部件上下文的 OOXML 结构错误。
pub fn invalid_part(part: &str, message: impl Into<String>) -> XlsxError {
    XlsxError::InvalidOoxml {
        part: part.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};
    use std::path::Path;

    use zip::write::{FullFileOptions, SimpleFileOptions, StreamWriter};
    use zip::{CompressionMethod, System, ZipArchive, ZipWriter};

    use super::{
        MAX_ENTRY_COUNT, PackageSnapshot, XlsxError, canonicalize_generated_package,
        read_bounded_workbook_bytes,
    };

    #[test]
    fn bounded_reader_rejects_the_file_before_exceeding_its_limit() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let file_size = std::fs::metadata(&path).unwrap().len();

        let bytes = read_bounded_workbook_bytes(&path, file_size, "测试工作簿").unwrap();
        assert_eq!(u64::try_from(bytes.len()).unwrap(), file_size);

        let error = read_bounded_workbook_bytes(&path, file_size - 1, "测试工作簿").unwrap_err();
        assert!(matches!(error, XlsxError::InvalidPath { .. }));
    }

    #[test]
    fn declared_part_size_bounds_actual_decompression() {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "[Content_Types].xml",
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
        writer.write_all(&vec![0; 1024 * 1024]).unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let directory = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        bytes[directory + 24..directory + 28].copy_from_slice(&1_u32.to_le_bytes());
        bytes[22..26].copy_from_slice(&1_u32.to_le_bytes());

        let error =
            PackageSnapshot::from_bytes(&bytes, Path::new("bounded-part.xlsx")).unwrap_err();
        assert!(error.to_string().contains("声明大小 1 与实际大小 2 不一致"));
    }

    #[test]
    fn package_preflight_rejects_classic_entry_count_before_zip_parser() {
        let path = Path::new("classic-entry-count.xlsx");
        let mut bytes = source_package(System::Unix);
        PackageSnapshot::from_bytes(&bytes, path).expect("正常 ZIP 应通过前置检查");
        let eocd = eocd_offset(&bytes);
        let excessive = u16::try_from(MAX_ENTRY_COUNT + 1).unwrap().to_le_bytes();
        bytes[eocd + 8..eocd + 10].copy_from_slice(&excessive);
        bytes[eocd + 10..eocd + 12].copy_from_slice(&excessive);

        let error = PackageSnapshot::from_bytes(&bytes, path).unwrap_err();

        assert!(error.to_string().contains("条目数"));
        assert!(error.to_string().contains(&MAX_ENTRY_COUNT.to_string()));
    }

    #[test]
    fn package_preflight_rejects_zip64_entry_count_before_zip_parser() {
        let path = Path::new("zip64-entry-count.xlsx");
        let source = source_package(System::Unix);
        let eocd = eocd_offset(&source);
        let directory_size = u64::from(u32::from_le_bytes(
            source[eocd + 12..eocd + 16].try_into().unwrap(),
        ));
        let directory_offset = u64::from(u32::from_le_bytes(
            source[eocd + 16..eocd + 20].try_into().unwrap(),
        ));
        let excessive = u64::try_from(MAX_ENTRY_COUNT + 1).unwrap();
        let mut bytes = source[..eocd].to_vec();
        let zip64_eocd_offset = u64::try_from(bytes.len()).unwrap();
        bytes.extend_from_slice(b"PK\x06\x06");
        bytes.extend_from_slice(&44_u64.to_le_bytes());
        bytes.extend_from_slice(&45_u16.to_le_bytes());
        bytes.extend_from_slice(&45_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&excessive.to_le_bytes());
        bytes.extend_from_slice(&excessive.to_le_bytes());
        bytes.extend_from_slice(&directory_size.to_le_bytes());
        bytes.extend_from_slice(&directory_offset.to_le_bytes());
        bytes.extend_from_slice(b"PK\x06\x07");
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&zip64_eocd_offset.to_le_bytes());
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        let mut classic_eocd = source[eocd..].to_vec();
        classic_eocd[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        classic_eocd[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        bytes.extend_from_slice(&classic_eocd);

        let error = PackageSnapshot::from_bytes(&bytes, path).unwrap_err();

        assert!(error.to_string().contains("条目数"));
        assert!(error.to_string().contains(&MAX_ENTRY_COUNT.to_string()));
    }

    #[test]
    fn package_preflight_preserves_supported_archive_prefixes() {
        let path = Path::new("prefixed-package.xlsx");
        let source = source_package(System::Unix);
        let mut bytes = b"fixed-prefix".to_vec();
        bytes.extend_from_slice(&source);

        let package = PackageSnapshot::from_bytes(&bytes, path).unwrap();

        assert_eq!(package.entry_count(), 6);
        assert_eq!(package.part("custom/data.bin").unwrap(), b"payload");
    }

    #[test]
    fn package_preflight_accepts_eocd_comment_signatures_and_trailing_bytes() {
        let path = Path::new("comment-signature.xlsx");
        let mut comment = b"comment-prefix".to_vec();
        comment.extend_from_slice(b"PK\x05\x06");
        comment.extend_from_slice(&0_u16.to_le_bytes());
        comment.extend_from_slice(&0_u16.to_le_bytes());
        let excessive = u16::try_from(MAX_ENTRY_COUNT + 1).unwrap();
        comment.extend_from_slice(&excessive.to_le_bytes());
        comment.extend_from_slice(&excessive.to_le_bytes());
        comment.extend_from_slice(&0_u32.to_le_bytes());
        comment.extend_from_slice(&0_u32.to_le_bytes());
        comment.extend_from_slice(&0_u16.to_le_bytes());
        let mut bytes = source_package_with_archive_comment(System::Unix, &comment);
        bytes.extend_from_slice(b"trailing-data");

        let package = PackageSnapshot::from_bytes(&bytes, path).unwrap();

        assert_eq!(package.entry_count(), 6);
    }

    #[test]
    fn package_preflight_accepts_central_directory_field_signatures() {
        let path = Path::new("central-field-signature.xlsx");
        let options = source_options(System::Unix)
            .into_full_options()
            .with_file_comment("entry-PK\u{5}\u{6}-comment");
        let bytes = package_with_special_entry("custom/data.bin", b"payload", options);

        let package = PackageSnapshot::from_bytes(&bytes, path).unwrap();

        assert_eq!(package.part("custom/data.bin").unwrap(), b"payload");
    }

    #[test]
    fn package_preflight_accepts_prefixed_zip64_extensible_data() {
        let path = Path::new("prefixed-zip64.xlsx");
        let source = source_package(System::Unix);
        let bytes = zip64_archive(&source, b"prefix-bytes", true, b"extensible");

        let package = PackageSnapshot::from_bytes(&bytes, path).unwrap();

        assert_eq!(package.entry_count(), 6);
        assert_eq!(package.part("custom/data.bin").unwrap(), b"payload");
    }

    #[test]
    fn package_preflight_accepts_optional_zip64_locator_without_sentinels() {
        let path = Path::new("optional-zip64.xlsx");
        let source = source_package(System::Unix);
        let bytes = zip64_archive(&source, b"", false, b"optional");

        let package = PackageSnapshot::from_bytes(&bytes, path).unwrap();

        assert_eq!(package.entry_count(), 6);
    }

    #[test]
    fn generated_package_metadata_is_identical_for_dos_and_unix_sources() {
        let dos = source_package(System::Dos);
        let unix = source_package(System::Unix);
        assert_ne!(dos, unix);

        let path = Path::new("generated-package.xlsx");
        let canonical = canonicalize_generated_package(&dos, path).unwrap();
        assert_eq!(
            canonical,
            canonicalize_generated_package(&unix, path).unwrap()
        );

        let mut archive = ZipArchive::new(Cursor::new(canonical)).unwrap();
        assert_eq!(archive.comment(), b"layout-package");
        let expected = [
            (
                "[Content_Types].xml",
                CompressionMethod::Deflated,
                0o100600,
                "",
                b"<Types/>".as_slice(),
            ),
            (
                "_rels/.rels",
                CompressionMethod::Deflated,
                0o100600,
                "",
                b"<Relationships/>".as_slice(),
            ),
            (
                "xl/workbook.xml",
                CompressionMethod::Deflated,
                0o100600,
                "",
                b"<workbook/>".as_slice(),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                CompressionMethod::Deflated,
                0o100600,
                "",
                b"<Relationships/>".as_slice(),
            ),
            (
                "custom/",
                CompressionMethod::Stored,
                0o040700,
                "generated-directory",
                b"".as_slice(),
            ),
            (
                "custom/data.bin",
                CompressionMethod::Stored,
                0o100600,
                "generated-file",
                b"payload".as_slice(),
            ),
        ];
        assert_eq!(archive.len(), expected.len());
        for (index, (name, compression, unix_mode, comment, content)) in
            expected.into_iter().enumerate()
        {
            let mut entry = archive.by_index(index).unwrap();
            assert_eq!(entry.name(), name);
            assert_eq!(entry.compression(), compression);
            assert_eq!(entry.unix_mode(), Some(unix_mode));
            assert_eq!(entry.last_modified(), Some(zip::DateTime::default()));
            assert_eq!(entry.comment(), comment);
            assert!(entry.extra_data().is_none_or(<[u8]>::is_empty));
            assert!(entry.extra_data_fields().next().is_none());

            let mut actual_content = Vec::new();
            entry.read_to_end(&mut actual_content).unwrap();
            assert_eq!(actual_content, content);
        }
    }

    fn eocd_offset(bytes: &[u8]) -> usize {
        bytes
            .windows(4)
            .rposition(|candidate| candidate == b"PK\x05\x06")
            .expect("测试 ZIP 必须包含 EOCD")
    }

    #[test]
    fn generated_package_rejects_metadata_that_cannot_be_preserved_losslessly() {
        let path = Path::new("generated-package.xlsx");
        for (bytes, expected_message) in [
            (package_with_extra_data(), "ZIP 扩展字段"),
            (package_with_non_ascii_comment(), "注释必须使用 ASCII 编码"),
            (package_with_non_empty_directory(), "目录条目不允许包含数据"),
        ] {
            let error = canonicalize_generated_package(&bytes, path).unwrap_err();
            assert!(
                matches!(
                    error,
                    XlsxError::InvalidOoxml { ref message, .. }
                        if message.contains(expected_message)
                ),
                "应拒绝 {expected_message}，实际为 {error}"
            );
        }
    }

    fn source_package(system: System) -> Vec<u8> {
        source_package_with_archive_comment(system, b"layout-package")
    }

    fn source_package_with_archive_comment(system: System, comment: &[u8]) -> Vec<u8> {
        let mut writer: ZipWriter<StreamWriter<Vec<u8>>> = ZipWriter::new_stream(Vec::new());
        writer
            .set_raw_comment(comment.to_vec().into_boxed_slice())
            .unwrap();
        let options = source_options(system);
        for (name, content) in [
            ("[Content_Types].xml", b"<Types/>".as_slice()),
            ("_rels/.rels", b"<Relationships/>".as_slice()),
            ("xl/workbook.xml", b"<workbook/>".as_slice()),
            ("xl/_rels/workbook.xml.rels", b"<Relationships/>".as_slice()),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(content).unwrap();
        }
        writer
            .add_directory(
                "custom/",
                options
                    .into_full_options()
                    .with_file_comment("generated-directory"),
            )
            .unwrap();
        writer
            .start_file(
                "custom/data.bin",
                options
                    .compression_method(CompressionMethod::Stored)
                    .into_full_options()
                    .with_file_comment("generated-file"),
            )
            .unwrap();
        writer.write_all(b"payload").unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn zip64_archive(
        source: &[u8],
        prefix: &[u8],
        use_classic_sentinels: bool,
        extensible: &[u8],
    ) -> Vec<u8> {
        let eocd = eocd_offset(source);
        let directory_size = u64::from(u32::from_le_bytes(
            source[eocd + 12..eocd + 16].try_into().unwrap(),
        ));
        let directory_offset = u64::from(u32::from_le_bytes(
            source[eocd + 16..eocd + 20].try_into().unwrap(),
        ));
        let entry_count = u64::from(u16::from_le_bytes(
            source[eocd + 10..eocd + 12].try_into().unwrap(),
        ));
        let zip64_record_offset = u64::try_from(eocd).unwrap();
        let record_size = 44_u64 + u64::try_from(extensible.len()).unwrap();
        let mut archive = source[..eocd].to_vec();
        archive.extend_from_slice(b"PK\x06\x06");
        archive.extend_from_slice(&record_size.to_le_bytes());
        archive.extend_from_slice(&45_u16.to_le_bytes());
        archive.extend_from_slice(&45_u16.to_le_bytes());
        archive.extend_from_slice(&0_u32.to_le_bytes());
        archive.extend_from_slice(&0_u32.to_le_bytes());
        archive.extend_from_slice(&entry_count.to_le_bytes());
        archive.extend_from_slice(&entry_count.to_le_bytes());
        archive.extend_from_slice(&directory_size.to_le_bytes());
        archive.extend_from_slice(&directory_offset.to_le_bytes());
        archive.extend_from_slice(extensible);
        archive.extend_from_slice(b"PK\x06\x07");
        archive.extend_from_slice(&0_u32.to_le_bytes());
        archive.extend_from_slice(&zip64_record_offset.to_le_bytes());
        archive.extend_from_slice(&1_u32.to_le_bytes());
        let mut classic_eocd = source[eocd..].to_vec();
        if use_classic_sentinels {
            classic_eocd[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
            classic_eocd[10..12].copy_from_slice(&u16::MAX.to_le_bytes());
        }
        archive.extend_from_slice(&classic_eocd);
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&archive);
        bytes
    }

    fn package_with_extra_data() -> Vec<u8> {
        let mut options: FullFileOptions<'_> = source_options(System::Unix).into_full_options();
        options.add_extra_data(0x5455, [0], false).unwrap();
        package_with_special_entry("custom/data.bin", b"payload", options)
    }

    fn package_with_non_ascii_comment() -> Vec<u8> {
        let options = source_options(System::Unix)
            .into_full_options()
            .with_file_comment("布局");
        package_with_special_entry("custom/data.bin", b"payload", options)
    }

    fn package_with_non_empty_directory() -> Vec<u8> {
        package_with_special_entry(
            "custom/",
            b"payload",
            source_options(System::Unix).into_full_options(),
        )
    }

    fn package_with_special_entry(
        name: &str,
        content: &[u8],
        options: FullFileOptions<'_>,
    ) -> Vec<u8> {
        let mut writer: ZipWriter<StreamWriter<Vec<u8>>> = ZipWriter::new_stream(Vec::new());
        let required_options = source_options(System::Unix);
        for (required_name, required_content) in [
            ("[Content_Types].xml", b"<Types/>".as_slice()),
            ("_rels/.rels", b"<Relationships/>".as_slice()),
            ("xl/workbook.xml", b"<workbook/>".as_slice()),
            ("xl/_rels/workbook.xml.rels", b"<Relationships/>".as_slice()),
        ] {
            writer.start_file(required_name, required_options).unwrap();
            writer.write_all(required_content).unwrap();
        }
        writer.start_file(name, options).unwrap();
        writer.write_all(content).unwrap();
        writer.finish().unwrap().into_inner()
    }

    fn source_options(system: System) -> SimpleFileOptions {
        SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .system(system)
            .unix_permissions(0o600)
            .last_modified_time(zip::DateTime::default())
    }
}

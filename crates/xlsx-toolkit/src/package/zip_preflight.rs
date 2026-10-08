//! 在 ZIP 解析器分配条目元数据前核验 EOCD、ZIP64 与中央目录边界。

use std::cell::Cell;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::rc::Rc;

use zip::{ZipArchive, result::ZipError};

use super::{MAX_ENTRY_COUNT, XlsxError, invalid_part};

const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;
const ZIP_EOCD_SIGNATURE: &[u8; 4] = b"PK\x05\x06";
const ZIP_EOCD_BYTES: usize = 22;
const ZIP_CENTRAL_HEADER_SIGNATURE: &[u8; 4] = b"PK\x01\x02";
const ZIP_CENTRAL_HEADER_BYTES: usize = 46;
const ZIP64_EOCD_SIGNATURE: &[u8; 4] = b"PK\x06\x06";
const ZIP64_EOCD_MINIMUM_BODY_BYTES: u64 = 44;
const ZIP64_LOCATOR_SIGNATURE: &[u8; 4] = b"PK\x06\x07";
const ZIP64_LOCATOR_BYTES: usize = 20;

/// ZIP 中央目录在磁盘上的边界和相对归档根的偏移。
#[derive(Clone, Copy, Debug)]
struct ZipDirectoryRecord {
    disk_number: u32,
    directory_disk: u32,
    disk_entries: u64,
    total_entries: u64,
    directory_size: u64,
    directory_offset: u64,
    physical_end: u64,
    relative_record_offset: Option<u64>,
}

/// 交给 ZIP 解析器的已确认归档根和中央目录起点。
#[derive(Clone, Copy, Debug)]
pub struct ZipDirectoryPreflight {
    archive_offset: u64,
    directory_start: usize,
    directory_end: usize,
    metadata_start: usize,
    metadata_end: usize,
    comment_start: usize,
    comment_end: usize,
}

/// 中央目录和结束元数据在物理归档中的确认位置。
#[derive(Clone, Copy, Debug)]
struct ZipDirectoryLocation {
    archive_offset: u64,
    directory_start: usize,
    directory_end: usize,
    metadata_start: usize,
}

impl ZipDirectoryPreflight {
    pub fn archive_comment(self, bytes: &[u8]) -> &[u8] {
        &bytes[self.comment_start..self.comment_end]
    }
}

/// 元数据解析阶段隐藏本地文件数据，避免解析器回退到伪造的 EOCD 记录。
pub struct PreflightZipReader<'a> {
    bytes: &'a [u8],
    position: u64,
    directory_start: usize,
    directory_end: usize,
    metadata_start: usize,
    metadata_end: usize,
    metadata_only: Rc<Cell<bool>>,
}

impl<'a> PreflightZipReader<'a> {
    fn new(
        bytes: &'a [u8],
        directory_start: usize,
        directory_end: usize,
        metadata_start: usize,
        metadata_end: usize,
        metadata_only: Rc<Cell<bool>>,
    ) -> Self {
        Self {
            bytes,
            position: 0,
            directory_start,
            directory_end,
            metadata_start,
            metadata_end,
            metadata_only,
        }
    }
}

impl Read for PreflightZipReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let start = usize::try_from(self.position).unwrap_or(usize::MAX);
        if start >= self.bytes.len() || buffer.is_empty() {
            return Ok(0);
        }
        let length = buffer.len().min(self.bytes.len() - start);
        let end = start + length;
        if self.metadata_only.get() {
            buffer[..length].fill(0);
            copy_visible_range(
                &mut buffer[..length],
                start,
                end,
                self.directory_start,
                self.directory_end,
                self.bytes,
            );
            copy_visible_range(
                &mut buffer[..length],
                start,
                end,
                self.metadata_start,
                self.metadata_end,
                self.bytes,
            );
        } else {
            buffer[..length].copy_from_slice(&self.bytes[start..end]);
        }
        self.position = self
            .position
            .checked_add(u64::try_from(length).unwrap_or(u64::MAX))
            .ok_or_else(|| std::io::Error::other("ZIP 读取位置溢出"))?;
        Ok(length)
    }
}

impl Seek for PreflightZipReader<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let length = i128::try_from(self.bytes.len()).unwrap_or(i128::MAX);
        let current = i128::from(self.position);
        let next = match position {
            SeekFrom::Start(value) => i128::from(value),
            SeekFrom::End(delta) => length + i128::from(delta),
            SeekFrom::Current(delta) => current + i128::from(delta),
        };
        if !(0..=i128::from(u64::MAX)).contains(&next) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ZIP 读取位置超出范围",
            ));
        }
        self.position = u64::try_from(next).map_err(std::io::Error::other)?;
        Ok(self.position)
    }
}

/// 将一个已确认的物理元数据区间映射到 ZIP 解析器的虚拟读取视图。
fn copy_visible_range(
    buffer: &mut [u8],
    buffer_start: usize,
    buffer_end: usize,
    visible_start: usize,
    visible_end: usize,
    bytes: &[u8],
) {
    let start = buffer_start.max(visible_start);
    let end = buffer_end.min(visible_end);
    if start < end {
        let target_start = start - buffer_start;
        let target_end = end - buffer_start;
        buffer[target_start..target_end].copy_from_slice(&bytes[start..end]);
    }
}

/// 在第三方 ZIP 解析器分配条目元数据前核对 EOCD、ZIP64 和中央目录边界。
fn preflight_zip_directory(bytes: &[u8]) -> Result<ZipDirectoryPreflight, XlsxError> {
    let Some(last_offset) = bytes.len().checked_sub(ZIP_EOCD_SIGNATURE.len()) else {
        return Err(invalid_zip_directory("缺少 EOCD 记录"));
    };
    let mut first_error = None;
    for eocd_offset in (0..=last_offset).rev() {
        if !zip_signature_at(bytes, eocd_offset, ZIP_EOCD_SIGNATURE) {
            continue;
        }
        match preflight_eocd_candidate(bytes, eocd_offset) {
            Ok(preflight) => return Ok(preflight),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or_else(|| invalid_zip_directory("缺少 EOCD 记录")))
}

/// 验证一个 EOCD 候选；注释或尾部数据中的伪签名失败后由调用方继续向前搜索。
fn preflight_eocd_candidate(
    bytes: &[u8],
    eocd_offset: usize,
) -> Result<ZipDirectoryPreflight, XlsxError> {
    let eocd_end = eocd_offset
        .checked_add(ZIP_EOCD_BYTES)
        .ok_or_else(|| invalid_zip_directory("EOCD 边界溢出"))?;
    if eocd_end > bytes.len() {
        return Err(invalid_zip_directory("EOCD 记录被截断"));
    }
    let comment_bytes = usize::from(read_zip_u16(bytes, eocd_offset + 20)?);
    let comment_end = eocd_end
        .checked_add(comment_bytes)
        .ok_or_else(|| invalid_zip_directory("EOCD 注释边界溢出"))?;
    if comment_end > bytes.len() {
        return Err(invalid_zip_directory("EOCD 注释边界无效"));
    }

    let classic = ZipDirectoryRecord {
        disk_number: u32::from(read_zip_u16(bytes, eocd_offset + 4)?),
        directory_disk: u32::from(read_zip_u16(bytes, eocd_offset + 6)?),
        disk_entries: u64::from(read_zip_u16(bytes, eocd_offset + 8)?),
        total_entries: u64::from(read_zip_u16(bytes, eocd_offset + 10)?),
        directory_size: u64::from(read_zip_u32(bytes, eocd_offset + 12)?),
        directory_offset: u64::from(read_zip_u32(bytes, eocd_offset + 16)?),
        physical_end: u64::try_from(eocd_offset).map_err(integer_conversion_error)?,
        relative_record_offset: None,
    };
    let uses_zip64 = classic.total_entries == u64::from(u16::MAX)
        || classic.directory_size == u64::from(u32::MAX)
        || classic.directory_offset == u64::from(u32::MAX);
    let locator_offset = eocd_offset.checked_sub(ZIP64_LOCATOR_BYTES);
    let has_locator = locator_offset.is_some_and(|offset| {
        bytes
            .get(offset..offset + ZIP64_LOCATOR_SIGNATURE.len())
            .is_some_and(|value| value == ZIP64_LOCATOR_SIGNATURE)
    });
    let location = match (uses_zip64, has_locator) {
        (true, true) => {
            preflight_zip64_directory(bytes, classic, locator_offset.expect("已确认偏移"))?
        }
        (true, false) => return Err(invalid_zip_directory("ZIP64 归档缺少定位记录")),
        // zip 允许小归档携带可选 ZIP64 结束记录；若紧邻字节只是经典中央目录
        // 变长字段中的同名签名，ZIP64 验证失败后仍按经典记录解释。
        (false, true) => {
            preflight_zip64_directory(bytes, classic, locator_offset.expect("已确认偏移"))
                .or_else(|_| validate_and_translate_zip_directory(bytes, classic))?
        }
        (false, false) => validate_and_translate_zip_directory(bytes, classic)?,
    };
    Ok(ZipDirectoryPreflight {
        archive_offset: location.archive_offset,
        directory_start: location.directory_start,
        directory_end: location.directory_end,
        metadata_start: location.metadata_start,
        metadata_end: eocd_end,
        comment_start: eocd_end,
        comment_end,
    })
}

/// 在定位记录之前寻找边界自洽的 ZIP64 EOCD，支持归档前缀和扩展数据区。
fn preflight_zip64_directory(
    bytes: &[u8],
    classic: ZipDirectoryRecord,
    locator_offset: usize,
) -> Result<ZipDirectoryLocation, XlsxError> {
    let record_disk = read_zip_u32(bytes, locator_offset + 4)?;
    let relative_record_offset = read_zip_u64(bytes, locator_offset + 8)?;
    let total_disks = read_zip_u32(bytes, locator_offset + 16)?;
    if record_disk != 0 || total_disks != 1 {
        return Err(invalid_zip_directory("不支持多磁盘 ZIP64 归档"));
    }

    let first_offset = usize::try_from(relative_record_offset).map_err(integer_conversion_error)?;
    let Some(last_offset) = locator_offset.checked_sub(ZIP64_EOCD_SIGNATURE.len()) else {
        return Err(invalid_zip_directory("ZIP64 EOCD 记录缺失或被截断"));
    };
    if first_offset > last_offset {
        return Err(invalid_zip_directory("ZIP64 EOCD 相对偏移超出定位记录"));
    }
    let mut first_error = None;
    for physical_record_offset in (first_offset..=last_offset).rev() {
        if !zip_signature_at(bytes, physical_record_offset, ZIP64_EOCD_SIGNATURE) {
            continue;
        }
        let result = read_zip64_directory_at(
            bytes,
            classic,
            locator_offset,
            relative_record_offset,
            physical_record_offset,
        )
        .and_then(|directory| validate_and_translate_zip_directory(bytes, directory));
        match result {
            Ok(preflight) => return Ok(preflight),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or_else(|| invalid_zip_directory("ZIP64 EOCD 记录缺失或被截断")))
}

/// 解析指定位置的 ZIP64 EOCD，并核对其与定位记录、经典 EOCD 的一致性。
fn read_zip64_directory_at(
    bytes: &[u8],
    classic: ZipDirectoryRecord,
    locator_offset: usize,
    relative_record_offset: u64,
    physical_record_offset: usize,
) -> Result<ZipDirectoryRecord, XlsxError> {
    let record_size = read_zip_u64(bytes, physical_record_offset + 4)?;
    if record_size < ZIP64_EOCD_MINIMUM_BODY_BYTES {
        return Err(invalid_zip_directory("ZIP64 EOCD 记录长度不足"));
    }
    let physical_record_end = u64::try_from(physical_record_offset)
        .map_err(integer_conversion_error)?
        .checked_add(12)
        .and_then(|value| value.checked_add(record_size))
        .ok_or_else(|| invalid_zip_directory("ZIP64 EOCD 边界溢出"))?;
    if physical_record_end != u64::try_from(locator_offset).map_err(integer_conversion_error)? {
        return Err(invalid_zip_directory("ZIP64 EOCD 与定位记录边界不连续"));
    }
    if read_zip_u16(bytes, physical_record_offset + 14)? < 45 {
        return Err(invalid_zip_directory("ZIP64 解压版本字段无效"));
    }

    let zip64 = ZipDirectoryRecord {
        disk_number: read_zip_u32(bytes, physical_record_offset + 16)?,
        directory_disk: read_zip_u32(bytes, physical_record_offset + 20)?,
        disk_entries: read_zip_u64(bytes, physical_record_offset + 24)?,
        total_entries: read_zip_u64(bytes, physical_record_offset + 32)?,
        directory_size: read_zip_u64(bytes, physical_record_offset + 40)?,
        directory_offset: read_zip_u64(bytes, physical_record_offset + 48)?,
        physical_end: u64::try_from(physical_record_offset).map_err(integer_conversion_error)?,
        relative_record_offset: Some(relative_record_offset),
    };
    validate_classic_zip64_consistency(classic, zip64)?;
    Ok(zip64)
}

/// 应用资源上限、物理偏移换算和中央目录逐项边界检查。
fn validate_and_translate_zip_directory(
    bytes: &[u8],
    directory: ZipDirectoryRecord,
) -> Result<ZipDirectoryLocation, XlsxError> {
    validate_zip_directory_limits(directory)?;
    let mut first_error = None;
    if let Ok(location) = translate_zip_directory(directory) {
        match validate_central_directory(bytes, location.directory_start, directory) {
            Ok(()) => return Ok(location),
            Err(error) => first_error = Some(error),
        }
    }

    // EOCD 的相对中央目录偏移不包含前置自解压数据；若中央目录后还有合法
    // 数字签名或尾部记录，按候选 CDFH 逐项消费来恢复真实物理起点。
    let minimum_start =
        usize::try_from(directory.directory_offset).map_err(integer_conversion_error)?;
    let physical_end = usize::try_from(directory.physical_end).map_err(integer_conversion_error)?;
    let directory_size =
        usize::try_from(directory.directory_size).map_err(integer_conversion_error)?;
    let Some(maximum_start) = physical_end.checked_sub(directory_size) else {
        return Err(first_error.unwrap_or_else(|| invalid_zip_directory("ZIP 中央目录范围无效")));
    };
    if minimum_start > maximum_start {
        return Err(first_error.unwrap_or_else(|| invalid_zip_directory("ZIP 中央目录起点无效")));
    }
    for directory_start in minimum_start..=maximum_start {
        if !zip_signature_at(bytes, directory_start, ZIP_CENTRAL_HEADER_SIGNATURE) {
            continue;
        }
        let archive_offset = u64::try_from(directory_start)
            .map_err(integer_conversion_error)?
            .checked_sub(directory.directory_offset)
            .ok_or_else(|| invalid_zip_directory("ZIP 归档前缀偏移无效"))?;
        let location = ZipDirectoryLocation {
            archive_offset,
            directory_start,
            directory_end: directory_start
                .checked_add(directory_size)
                .ok_or_else(|| invalid_zip_directory("ZIP 中央目录终点溢出"))?,
            metadata_start: physical_end,
        };
        match validate_central_directory(bytes, location.directory_start, directory) {
            Ok(()) => return Ok(location),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or_else(|| invalid_zip_directory("ZIP 中央目录条目无效")))
}

fn validate_classic_zip64_consistency(
    classic: ZipDirectoryRecord,
    zip64: ZipDirectoryRecord,
) -> Result<(), XlsxError> {
    let matches = [
        (
            u64::from(classic.disk_number),
            u64::from(zip64.disk_number),
            u64::from(u16::MAX),
        ),
        (
            u64::from(classic.directory_disk),
            u64::from(zip64.directory_disk),
            u64::from(u16::MAX),
        ),
        (
            classic.disk_entries,
            zip64.disk_entries,
            u64::from(u16::MAX),
        ),
        (
            classic.total_entries,
            zip64.total_entries,
            u64::from(u16::MAX),
        ),
        (
            classic.directory_size,
            zip64.directory_size,
            u64::from(u32::MAX),
        ),
        (
            classic.directory_offset,
            zip64.directory_offset,
            u64::from(u32::MAX),
        ),
    ];
    if matches
        .into_iter()
        .any(|(classic_value, zip64_value, sentinel)| {
            classic_value != sentinel && classic_value != zip64_value
        })
    {
        return Err(invalid_zip_directory("ZIP 与 ZIP64 中央目录元数据矛盾"));
    }
    Ok(())
}

fn validate_zip_directory_limits(directory: ZipDirectoryRecord) -> Result<(), XlsxError> {
    if directory.disk_number != 0
        || directory.directory_disk != 0
        || directory.disk_entries != directory.total_entries
    {
        return Err(invalid_zip_directory("不支持多磁盘 ZIP 归档"));
    }
    if directory.total_entries > u64::try_from(MAX_ENTRY_COUNT).unwrap_or(u64::MAX) {
        return Err(invalid_zip_directory(format!(
            "ZIP 条目数 {} 超过上限 {MAX_ENTRY_COUNT}",
            directory.total_entries
        )));
    }
    if directory.directory_size > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(invalid_zip_directory(format!(
            "ZIP 中央目录大小 {} 超过上限 {MAX_CENTRAL_DIRECTORY_BYTES}",
            directory.directory_size
        )));
    }
    Ok(())
}

fn translate_zip_directory(
    directory: ZipDirectoryRecord,
) -> Result<ZipDirectoryLocation, XlsxError> {
    let relative_directory_end = directory
        .directory_offset
        .checked_add(directory.directory_size)
        .ok_or_else(|| invalid_zip_directory("ZIP 中央目录相对边界溢出"))?;
    let relative_metadata_start = directory
        .relative_record_offset
        .unwrap_or(relative_directory_end);
    if directory
        .relative_record_offset
        .is_some_and(|record_offset| relative_directory_end > record_offset)
    {
        return Err(invalid_zip_directory("ZIP64 中央目录相对边界无效"));
    }
    let archive_offset = directory
        .physical_end
        .checked_sub(relative_metadata_start)
        .ok_or_else(|| invalid_zip_directory("ZIP 中央目录物理边界无效"))?;
    let directory_start = archive_offset
        .checked_add(directory.directory_offset)
        .ok_or_else(|| invalid_zip_directory("ZIP 中央目录起点溢出"))?;
    let directory_end = directory_start
        .checked_add(directory.directory_size)
        .ok_or_else(|| invalid_zip_directory("ZIP 中央目录终点溢出"))?;
    let metadata_start =
        usize::try_from(directory.physical_end).map_err(integer_conversion_error)?;
    if directory_end > directory.physical_end {
        return Err(invalid_zip_directory("ZIP 中央目录超出结束元数据起点"));
    }
    Ok(ZipDirectoryLocation {
        archive_offset,
        directory_start: usize::try_from(directory_start).map_err(integer_conversion_error)?,
        directory_end: usize::try_from(directory_end).map_err(integer_conversion_error)?,
        metadata_start,
    })
}

/// 只扫描中央目录固定头和变长字段边界，先于依赖库确认真实条目数量。
fn validate_central_directory(
    bytes: &[u8],
    directory_start: usize,
    directory: ZipDirectoryRecord,
) -> Result<(), XlsxError> {
    let directory_size =
        usize::try_from(directory.directory_size).map_err(integer_conversion_error)?;
    let directory_end = directory_start
        .checked_add(directory_size)
        .ok_or_else(|| invalid_zip_directory("ZIP 中央目录范围溢出"))?;
    if directory_end > bytes.len() {
        return Err(invalid_zip_directory("ZIP 中央目录超出文件边界"));
    }
    let physical_end = usize::try_from(directory.physical_end).map_err(integer_conversion_error)?;
    if directory_end > physical_end {
        return Err(invalid_zip_directory("ZIP 中央目录超出结束元数据起点"));
    }
    let minimum_bytes = directory
        .total_entries
        .checked_mul(u64::try_from(ZIP_CENTRAL_HEADER_BYTES).unwrap_or(u64::MAX))
        .ok_or_else(|| invalid_zip_directory("ZIP 中央目录最小大小溢出"))?;
    if minimum_bytes > directory.directory_size {
        return Err(invalid_zip_directory("ZIP 中央目录无法容纳声明的条目数"));
    }

    let mut position = directory_start;
    let mut entries = 0_u64;
    while position < directory_end {
        let header_end = position
            .checked_add(ZIP_CENTRAL_HEADER_BYTES)
            .ok_or_else(|| invalid_zip_directory("ZIP 中央目录条目头溢出"))?;
        if header_end > directory_end
            || !zip_signature_at(bytes, position, ZIP_CENTRAL_HEADER_SIGNATURE)
        {
            return Err(invalid_zip_directory("ZIP 中央目录条目头无效或被截断"));
        }
        let file_name_bytes = usize::from(read_zip_u16(bytes, position + 28)?);
        let extra_field_bytes = usize::from(read_zip_u16(bytes, position + 30)?);
        let comment_bytes = usize::from(read_zip_u16(bytes, position + 32)?);
        let variable_bytes = file_name_bytes
            .checked_add(extra_field_bytes)
            .and_then(|value| value.checked_add(comment_bytes))
            .ok_or_else(|| invalid_zip_directory("ZIP 中央目录变长字段溢出"))?;
        position = header_end
            .checked_add(variable_bytes)
            .ok_or_else(|| invalid_zip_directory("ZIP 中央目录条目边界溢出"))?;
        if position > directory_end {
            return Err(invalid_zip_directory("ZIP 中央目录变长字段越界"));
        }
        entries = entries
            .checked_add(1)
            .ok_or_else(|| invalid_zip_directory("ZIP 条目计数溢出"))?;
        if entries > u64::try_from(MAX_ENTRY_COUNT).unwrap_or(u64::MAX) {
            return Err(invalid_zip_directory(format!(
                "ZIP 条目数超过上限 {MAX_ENTRY_COUNT}"
            )));
        }
    }
    if position != directory_end || entries != directory.total_entries {
        return Err(invalid_zip_directory(format!(
            "ZIP 中央目录条目数不一致: expected={}, actual={entries}",
            directory.total_entries
        )));
    }
    Ok(())
}

fn zip_signature_at(bytes: &[u8], offset: usize, signature: &[u8; 4]) -> bool {
    bytes
        .get(offset..offset.saturating_add(signature.len()))
        .is_some_and(|value| value == signature)
}

fn read_zip_u16(bytes: &[u8], offset: usize) -> Result<u16, XlsxError> {
    let value: [u8; 2] = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or_else(|| invalid_zip_directory("ZIP 元数据字段被截断"))?
        .try_into()
        .map_err(|_| invalid_zip_directory("ZIP 元数据字段长度错误"))?;
    Ok(u16::from_le_bytes(value))
}

fn read_zip_u32(bytes: &[u8], offset: usize) -> Result<u32, XlsxError> {
    let value: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or_else(|| invalid_zip_directory("ZIP 元数据字段被截断"))?
        .try_into()
        .map_err(|_| invalid_zip_directory("ZIP 元数据字段长度错误"))?;
    Ok(u32::from_le_bytes(value))
}

fn read_zip_u64(bytes: &[u8], offset: usize) -> Result<u64, XlsxError> {
    let value: [u8; 8] = bytes
        .get(offset..offset.saturating_add(8))
        .ok_or_else(|| invalid_zip_directory("ZIP 元数据字段被截断"))?
        .try_into()
        .map_err(|_| invalid_zip_directory("ZIP 元数据字段长度错误"))?;
    Ok(u64::from_le_bytes(value))
}

fn integer_conversion_error(error: impl std::fmt::Display) -> XlsxError {
    invalid_zip_directory(format!("ZIP 元数据整数转换失败: {error}"))
}

fn invalid_zip_directory(message: impl Into<String>) -> XlsxError {
    invalid_part("ZIP 中央目录", message)
}

/// 以预检确认的归档偏移打开 ZIP；元数据建立完成前不暴露本地文件数据和尾部伪记录。
pub fn open_preflight_zip_archive<'a>(
    bytes: &'a [u8],
    path: &Path,
    stage: &'static str,
) -> Result<(ZipDirectoryPreflight, ZipArchive<PreflightZipReader<'a>>), XlsxError> {
    let preflight = preflight_zip_directory(bytes)?;
    let metadata_only = Rc::new(Cell::new(true));
    let reader = PreflightZipReader::new(
        bytes,
        preflight.directory_start,
        preflight.directory_end,
        preflight.metadata_start,
        preflight.metadata_end,
        Rc::clone(&metadata_only),
    );
    let config = zip::read::Config {
        archive_offset: zip::read::ArchiveOffset::Known(preflight.archive_offset),
    };
    let archive =
        ZipArchive::with_config(config, reader).map_err(|source: ZipError| XlsxError::Zip {
            stage,
            path: path.to_path_buf(),
            source,
        })?;
    metadata_only.set(false);
    Ok((preflight, archive))
}

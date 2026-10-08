//! 从安装登记和运行进程发现 模拟器管理器，并验证候选的实例查询能力。

use super::INSTANCE_QUERY_TIMEOUT;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use super::process_image::query_process_image_path;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_NO_MORE_FILES, ERROR_NO_MORE_ITEMS,
    ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, GetLastError, INVALID_HANDLE_VALUE, WIN32_ERROR,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ENUMERATE_SUB_KEYS, KEY_WOW64_32KEY,
    KEY_WOW64_64KEY, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY,
    RRF_SUBKEY_WOW6464KEY, RRF_ZEROONFAILURE, RegCloseKey, RegEnumKeyExW, RegGetValueW,
    RegOpenKeyExW,
};

use super::EmulatorInstance;
use super::{ADAPTERS, adapter_for_manager};
const INSTALL_LOCATION_VALUE: &str = "InstallLocation";
const UNINSTALL_STRING_VALUE: &str = "UninstallString";
const MAX_REGISTRY_VALUE_BYTES: u32 = 64 * 1024;
use super::EmulatorError;
pub struct DiscoveryRequest<'a> {
    pub manager_hint: Option<&'a Path>,
    pub include_running: bool,
    pub strict_hint: bool,
}

pub struct ResolvedManager {
    pub executable: PathBuf,
    pub instances: BTreeMap<String, EmulatorInstance>,
}

/// 一次发现的可用安装与独立候选失败、降级信息。
pub struct DiscoveryReport {
    pub managers: Vec<ResolvedManager>,
    pub warnings: Vec<String>,
}

const DISCOVERY_QUERY_TIMEOUT: Duration = Duration::from_secs(15);

/// 返回已验证安装。需要展示部分失败信息的调用方使用 discover_report。
pub fn discover_managers(
    options: &DiscoveryRequest<'_>,
) -> Result<Vec<ResolvedManager>, EmulatorError> {
    Ok(discover_report(options)?.managers)
}

/// 非严格提示参与全部安装发现；严格提示只查询指定安装。
pub fn discover_report(options: &DiscoveryRequest<'_>) -> Result<DiscoveryReport, EmulatorError> {
    let started = Instant::now();
    discover_with(
        options,
        || {
            let mut candidates = Vec::new();
            let mut warnings = Vec::new();
            if options.include_running {
                match running_manager_candidates() {
                    Ok((running, diagnostics)) => {
                        candidates.extend(running);
                        warnings.extend(diagnostics);
                    }
                    Err(message) => warnings.push(message),
                }
            }
            let (registered, diagnostics) = registry_manager_candidates();
            candidates.extend(registered);
            warnings.extend(diagnostics);
            // 安装布局允许多个候选名称；不存在的备用路径不属于查询失败。
            candidates.retain(|path| path.try_exists().unwrap_or(true));
            (candidates, warnings)
        },
        || DISCOVERY_QUERY_TIMEOUT.saturating_sub(started.elapsed()),
        validate_candidate_with_timeout,
    )
}

fn discover_with(
    options: &DiscoveryRequest<'_>,
    collect: impl FnOnce() -> (Vec<PathBuf>, Vec<String>),
    remaining: impl Fn() -> Duration,
    mut query: impl FnMut(&Path, Duration) -> Result<(ResolvedManager, Vec<String>), EmulatorError>,
) -> Result<DiscoveryReport, EmulatorError> {
    let mut report = DiscoveryReport {
        managers: Vec::new(),
        warnings: Vec::new(),
    };
    let mut queried = BTreeSet::new();
    let mut installed = BTreeSet::new();
    if let Some(hint) = options.manager_hint {
        queried.insert(hint.to_string_lossy().to_ascii_lowercase());
        match query(hint, remaining().min(INSTANCE_QUERY_TIMEOUT)) {
            Ok((manager, warnings)) => {
                installed.insert(manager.executable.to_string_lossy().to_ascii_lowercase());
                report.managers.push(manager);
                report.warnings.extend(warnings);
                if options.strict_hint {
                    return Ok(report);
                }
            }
            Err(error) if options.strict_hint => return Err(error),
            Err(error) => report.warnings.push(format!("{}: {error}", hint.display())),
        }
    }
    let (candidates, warnings) = collect();
    report.warnings.extend(warnings);
    for candidate in candidates {
        if !queried.insert(candidate.to_string_lossy().to_ascii_lowercase()) {
            continue;
        }
        let timeout = remaining().min(INSTANCE_QUERY_TIMEOUT);
        if timeout.is_zero() {
            report.warnings.push(
                "模拟器发现查询预算已耗尽，部分安装尚未验证，请重新刷新或指定管理器路径".to_owned(),
            );
            break;
        }
        match query(&candidate, timeout) {
            Ok((manager, warnings)) => {
                report.warnings.extend(warnings);
                if installed.insert(manager.executable.to_string_lossy().to_ascii_lowercase()) {
                    report.managers.push(manager);
                }
            }
            Err(error) => report
                .warnings
                .push(format!("{}: {error}", candidate.display())),
        }
    }
    if report.managers.is_empty() {
        return Err(EmulatorError::Discovery {
            message: format!("没有可用的模拟器管理器；{}", report.warnings.join(" | ")),
        });
    }
    Ok(report)
}

/// 提取结构明确的卸载命令路径，由注册的提供方解释安装布局。
pub fn install_root_from_uninstall_command(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let executable = if let Some(quoted) = command.strip_prefix('"') {
        let closing_quote = quoted.find('"')?;
        &quoted[..closing_quote]
    } else {
        command.split_ascii_whitespace().next()?
    };
    let executable = Path::new(executable);
    #[cfg(target_os = "windows")]
    {
        ADAPTERS
            .iter()
            .find_map(|adapter| adapter.uninstall_root(executable))
    }
}

/// 候选必须匹配注册管理器，并通过厂商实例查询验证。
pub fn validate_manager_candidate(candidate: &Path) -> Result<ResolvedManager, EmulatorError> {
    Ok(validate_candidate_with_timeout(candidate, INSTANCE_QUERY_TIMEOUT)?.0)
}

fn validate_candidate_with_timeout(
    candidate: &Path,
    timeout: Duration,
) -> Result<(ResolvedManager, Vec<String>), EmulatorError> {
    if timeout.is_zero() {
        return Err(EmulatorError::Discovery {
            message: "实例查询时间预算已耗尽".to_owned(),
        });
    }
    let started = Instant::now();
    let canonical = fs::canonicalize(candidate).map_err(|source| EmulatorError::Io {
        stage: "emulator.canonicalize_manager",
        path: candidate.to_path_buf(),
        source,
    })?;
    if !canonical.is_file() {
        return Err(EmulatorError::Discovery {
            message: "管理器不是普通文件".to_owned(),
        });
    }
    let budget = timeout.saturating_sub(started.elapsed());
    if budget.is_zero() {
        return Err(EmulatorError::Discovery {
            message: "管理器路径验证耗尽实例查询预算".to_owned(),
        });
    }
    let report = adapter_for_manager(&canonical)?.query_instances(&canonical, budget)?;
    let warnings = report
        .warnings
        .into_iter()
        .map(|message| format!("{}: {message}", canonical.display()))
        .collect();
    Ok((
        ResolvedManager {
            executable: canonical,
            instances: report.instances,
        },
        warnings,
    ))
}

/// 从当前用户和本机的已知卸载登记中收集新版与旧版管理器候选。
fn registry_manager_candidates() -> (Vec<PathBuf>, Vec<String>) {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();
    for (root, root_name) in [(HKEY_LOCAL_MACHINE, "HKLM"), (HKEY_CURRENT_USER, "HKCU")] {
        for (view, view_name) in [(RRF_SUBKEY_WOW6464KEY, "64"), (RRF_SUBKEY_WOW6432KEY, "32")] {
            let subkeys = match emulator_uninstall_subkeys(root, view) {
                Ok(subkeys) => subkeys,
                Err(error) => {
                    diagnostics.push(format!("{root_name} {view_name} 位视图: {error}"));
                    continue;
                }
            };
            for subkey in subkeys {
                match read_registry_string(
                    root,
                    root_name,
                    view,
                    view_name,
                    &subkey,
                    INSTALL_LOCATION_VALUE,
                ) {
                    Ok(Some(location)) => candidates.extend(
                        ADAPTERS
                            .iter()
                            .flat_map(|adapter| adapter.install_candidates(Path::new(&location))),
                    ),
                    Ok(None) => {}
                    Err(error) => diagnostics.push(error.to_string()),
                }
                match read_registry_string(
                    root,
                    root_name,
                    view,
                    view_name,
                    &subkey,
                    UNINSTALL_STRING_VALUE,
                ) {
                    Ok(Some(uninstall)) => match uninstall
                        .to_str()
                        .and_then(install_root_from_uninstall_command)
                    {
                        Some(install_root) => {
                            candidates.extend(ADAPTERS.iter().flat_map(|adapter| adapter.install_candidates(&install_root)));
                        }
                        None => diagnostics.push(format!(
                            "{root_name} {view_name} 位视图 {subkey} 的 UninstallString 无法安全解析"
                        )),
                    },
                    Ok(None) => {}
                    Err(error) => diagnostics.push(error.to_string()),
                }
            }
        }
    }
    (candidates, diagnostics)
}

/// 枚举当前卸载登记，版本字符串不决定是否收集安装候选。
fn emulator_uninstall_subkeys(root: HKEY, view: u32) -> Result<Vec<String>, EmulatorError> {
    const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
    let path: Vec<u16> = UNINSTALL.encode_utf16().chain(std::iter::once(0)).collect();
    let access = KEY_ENUMERATE_SUB_KEYS
        | if view == RRF_SUBKEY_WOW6464KEY {
            KEY_WOW64_64KEY
        } else {
            KEY_WOW64_32KEY
        };
    let mut key: HKEY = ptr::null_mut();
    // 注册表句柄由本函数打开，并在枚举成功或失败后统一关闭。
    let status = unsafe { RegOpenKeyExW(root, path.as_ptr(), 0, access, &mut key) };
    if matches!(status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        return Ok(Vec::new());
    }
    if status != ERROR_SUCCESS {
        return Err(EmulatorError::Registry {
            message: format!("打开卸载登记失败: {status}"),
        });
    }
    let result = (|| {
        let mut matches = Vec::new();
        let mut index = 0;
        loop {
            // Windows 注册表单个键名至多 255 个 UTF-16 单元。
            let mut name = [0_u16; 256];
            let mut count = name.len() as u32;
            let status = unsafe {
                RegEnumKeyExW(
                    key,
                    index,
                    name.as_mut_ptr(),
                    &mut count,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                return Ok(matches);
            }
            if status != ERROR_SUCCESS {
                return Err(EmulatorError::Registry {
                    message: format!("枚举卸载登记失败: {status}"),
                });
            }
            let name = String::from_utf16(&name[..count as usize]).map_err(|error| {
                EmulatorError::Registry {
                    message: format!("卸载登记名称不是有效 UTF-16: {error}"),
                }
            })?;
            if ADAPTERS
                .iter()
                .any(|adapter| adapter.matches_uninstall_key(&name))
            {
                matches.push(format!("{UNINSTALL}\\{name}"));
            }
            index += 1;
        }
    })();
    let close_status = unsafe { RegCloseKey(key) };
    if close_status != ERROR_SUCCESS {
        return Err(EmulatorError::Registry {
            message: format!("关闭卸载登记失败: {close_status}"),
        });
    }
    result
}

/// 有界读取单个 REG_SZ/REG_EXPAND_SZ；键或值不存在表示当前登记没有对应候选。
fn read_registry_string(
    root: HKEY,
    root_name: &'static str,
    view: u32,
    view_name: &'static str,
    subkey_text: &str,
    value_name_text: &str,
) -> Result<Option<OsString>, EmulatorError> {
    let subkey: Vec<u16> = subkey_text
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value_name: Vec<u16> = value_name_text
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let flags: u32 = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_ZEROONFAILURE | view;
    let mut byte_count: u32 = 0;
    // 首次调用只查询所需字节数；所有指针都来自本作用域内仍存活的缓冲区。
    let query_status: WIN32_ERROR = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value_name.as_ptr(),
            flags,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut byte_count,
        )
    };
    if matches!(query_status, ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND) {
        return Ok(None);
    }
    if query_status != ERROR_SUCCESS && query_status != ERROR_MORE_DATA {
        return Err(EmulatorError::Registry {
            message: format!(
                "{root_name} {view_name} 位视图查询 {subkey_text}/{value_name_text} 大小返回 {query_status}"
            ),
        });
    }
    if !(2..=MAX_REGISTRY_VALUE_BYTES).contains(&byte_count) || !byte_count.is_multiple_of(2) {
        return Err(EmulatorError::Registry {
            message: format!(
                "{root_name} {view_name} 位视图 {subkey_text}/{value_name_text} 字节数无效: {byte_count}"
            ),
        });
    }

    let element_count: usize =
        usize::try_from(byte_count / 2).map_err(|_| EmulatorError::Registry {
            message: format!(
                "{root_name} {view_name} 位视图 {subkey_text}/{value_name_text} 长度无法转换为 usize"
            ),
        })?;
    let mut buffer: Vec<u16> = vec![0; element_count];
    let mut actual_bytes: u32 = byte_count;
    // 第二次调用提供按上一查询精确分配的 UTF-16 缓冲区，并要求 API 不再扩容。
    let read_status: WIN32_ERROR = unsafe {
        RegGetValueW(
            root,
            subkey.as_ptr(),
            value_name.as_ptr(),
            flags,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut actual_bytes,
        )
    };
    if read_status != ERROR_SUCCESS {
        return Err(EmulatorError::Registry {
            message: format!(
                "{root_name} {view_name} 位视图读取 {subkey_text}/{value_name_text} 返回 {read_status}"
            ),
        });
    }
    if !(2..=byte_count).contains(&actual_bytes) || !actual_bytes.is_multiple_of(2) {
        return Err(EmulatorError::Registry {
            message: format!(
                "{root_name} {view_name} 位视图 {subkey_text}/{value_name_text} 实际字节数无效: {actual_bytes}"
            ),
        });
    }
    let actual_elements: usize =
        usize::try_from(actual_bytes / 2).map_err(|_| EmulatorError::Registry {
            message: format!(
                "{root_name} {view_name} 位视图 {subkey_text}/{value_name_text} 实际长度无法转换为 usize"
            ),
        })?;
    buffer.truncate(actual_elements);
    while buffer.last() == Some(&0) {
        buffer.pop();
    }
    if buffer.is_empty() {
        return Err(EmulatorError::Registry {
            message: format!("{root_name} {view_name} 位视图 {subkey_text}/{value_name_text} 为空"),
        });
    }
    Ok(Some(OsString::from_wide(&buffer)))
}

/// 枚举 模拟器 自身进程并以最低查询权限读取映像路径，作为卸载登记之外的回退。
pub fn running_manager_candidates() -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!("模拟器运行进程快照创建失败: {}", unsafe {
            GetLastError()
        }));
    }
    let result = enumerate_running_manager_candidates(snapshot);
    let close_error = if unsafe { CloseHandle(snapshot) } == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    match (result, close_error) {
        (Ok(value), None) => Ok(value),
        (Ok(_), Some(error)) => Err(format!("模拟器运行进程快照句柄关闭失败: {error}")),
        (Err(message), None) => Err(message),
        (Err(message), Some(error)) => Err(format!(
            "{message}；模拟器运行进程快照句柄关闭失败: {error}"
        )),
    }
}

/// 在已经取得的系统进程快照中只处理 模拟器 的已知进程名。
fn enumerate_running_manager_candidates(
    snapshot: windows_sys::Win32::Foundation::HANDLE,
) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>())
            .expect("PROCESSENTRY32W size must fit u32"),
        ..PROCESSENTRY32W::default()
    };
    if unsafe { Process32FirstW(snapshot, &mut entry) } == 0 {
        let error = unsafe { GetLastError() };
        return if error == ERROR_NO_MORE_FILES {
            Ok((Vec::new(), Vec::new()))
        } else {
            Err(format!("模拟器运行进程快照首项读取失败: {error}"))
        };
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();
    loop {
        let name_end = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szExeFile.len());
        let process_name = OsString::from_wide(&entry.szExeFile[..name_end]);
        let known_process = process_name.to_str().is_some_and(|name| {
            ADAPTERS.iter().any(|adapter| {
                adapter
                    .process_names()
                    .iter()
                    .any(|known| name.eq_ignore_ascii_case(known))
            })
        });
        if known_process {
            match query_process_image_path(entry.th32ProcessID) {
                Ok(image) => match ADAPTERS
                    .iter()
                    .find_map(|adapter| adapter.process_candidate(&image))
                {
                    Some(candidate) => candidates.push(candidate),
                    None => diagnostics.push(format!(
                        "模拟器 进程 {} 的映像路径层级无法识别",
                        process_name.to_string_lossy()
                    )),
                },
                Err(message) => diagnostics.push(format!(
                    "模拟器 进程 {} (pid={}) 映像路径读取失败: {message}",
                    process_name.to_string_lossy(),
                    entry.th32ProcessID
                )),
            }
        }

        if unsafe { Process32NextW(snapshot, &mut entry) } == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_NO_MORE_FILES {
                return Err(format!("模拟器运行进程快照后续项读取失败: {error}"));
            }
            break;
        }
    }
    Ok((candidates, diagnostics))
}

#[cfg(test)]
mod tests;

//! 通过系统 WinHTTP 读取固定 BWiki HTTPS 接口，不依赖外部命令或浏览器。

use std::io::{Read, Seek, Write};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::adapters::tool_root::ToolRoot;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub(super) struct FetchError {
    pub(super) message: String,
    pub(super) stop_batch: bool,
    retry_after: Option<Duration>,
}

impl FetchError {
    pub(super) fn http_status(status: u32) -> Self {
        Self {
            message: if status == 567 {
                "BWiki HTTP 567：该页面请求被站点安全策略拦截".to_owned()
            } else {
                format!("BWiki HTTP {status}")
            },
            stop_batch: matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 567),
            retry_after: matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 567)
                .then_some(Duration::ZERO),
        }
    }
    fn local(message: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_string(),
            stop_batch: true,
            retry_after: None,
        }
    }
}

pub(super) fn api_error_stops_batch(code: &str) -> bool {
    matches!(
        code,
        "ratelimited" | "maxlag" | "readonly" | "internalerror"
    ) || code.starts_with("internal_api_error")
}

// 同安装目录的所有调用者共享同一把锁；锁持续覆盖响应读取，限制单个在途请求。
#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RequestState {
    next_request_ms: u64,
    failures: u32,
    reason: String,
}

trait Clock {
    fn now(&self) -> SystemTime;
    fn sleep(&self, duration: Duration);
}
struct SystemClock;
impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}
struct RequestControl<'a> {
    root: &'a ToolRoot,
    interval: Duration,
    cancelled: &'a dyn Fn() -> bool,
    progress: &'a mut dyn FnMut(String),
    clock: &'a dyn Clock,
}
impl RequestControl<'_> {
    fn now_ms(&self) -> Result<u64, FetchError> {
        let milliseconds = self
            .clock
            .now()
            .duration_since(UNIX_EPOCH)
            .map_err(FetchError::local)?
            .as_millis();
        u64::try_from(milliseconds).map_err(FetchError::local)
    }
    fn check_cancelled(&self) -> Result<(), FetchError> {
        if (self.cancelled)() {
            Err(FetchError::local("获取方式在线查询已取消"))
        } else {
            Ok(())
        }
    }
    fn lock(&mut self) -> Result<std::fs::File, FetchError> {
        let mut last_report = None;
        loop {
            self.check_cancelled()?;
            match self.root.lock_file(
                Path::new("data/cache/ship-acquisition/request.lock"),
                Duration::ZERO,
            ) {
                Ok(file) => return Ok(file),
                Err(error) => {
                    #[cfg(windows)]
                    let busy = error.raw_os_error()
                        == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32);
                    #[cfg(not(windows))]
                    let busy = error.kind() == std::io::ErrorKind::WouldBlock;
                    if !busy {
                        return Err(FetchError::local(format!("获取 BWiki 请求锁：{error}")));
                    }
                    let second = self.now_ms()? / 1000;
                    if last_report != Some(second) {
                        (self.progress)("等待同安装目录的 BWiki 请求或冷却结束".to_owned());
                        last_report = Some(second);
                    }
                    self.clock.sleep(Duration::from_millis(100));
                }
            }
        }
    }
    fn wait(&mut self, state: &RequestState) -> Result<(), FetchError> {
        let mut last_remaining = None;
        loop {
            self.check_cancelled()?;
            let remaining = state.next_request_ms.saturating_sub(self.now_ms()?);
            if remaining == 0 {
                return Ok(());
            }
            let seconds = remaining.div_ceil(1000);
            if last_remaining != Some(seconds) {
                (self.progress)(format!("BWiki 冷却：{}；剩余 {seconds} 秒", state.reason));
                last_remaining = Some(seconds);
            }
            self.clock.sleep(Duration::from_millis(remaining.min(100)));
        }
    }
}

fn read_state(file: &mut std::fs::File) -> Result<RequestState, FetchError> {
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(FetchError::local)?;
    if bytes.is_empty() {
        return Ok(RequestState::default());
    }
    if bytes.len() > 4096 {
        return Err(FetchError::local("BWiki 请求状态超过大小上限"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| FetchError::local(format!("读取 BWiki 请求状态：{error}")))
}
fn save_state(file: &mut std::fs::File, state: &RequestState) -> Result<(), FetchError> {
    let bytes = serde_json::to_vec(state).map_err(FetchError::local)?;
    file.rewind().map_err(FetchError::local)?;
    file.write_all(&bytes).map_err(FetchError::local)?;
    file.set_len(bytes.len() as u64)
        .map_err(FetchError::local)?;
    file.sync_all().map_err(FetchError::local)
}

fn temporary_response(bytes: Vec<u8>) -> Result<Vec<u8>, FetchError> {
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| FetchError {
        message: format!("BWiki HTTP 200 返回非 JSON 或验证页面：{error}"),
        stop_batch: true,
        retry_after: Some(Duration::ZERO),
    })?;
    if let Some(error) = value.get("error") {
        let code = error["code"].as_str().unwrap_or("");
        if api_error_stops_batch(code) {
            return Err(FetchError {
                message: format!("BWiki API: {error}"),
                stop_batch: true,
                retry_after: Some(Duration::ZERO),
            });
        }
    }
    Ok(bytes)
}

fn fetch_with_retry(
    control: &mut RequestControl<'_>,
    title: &str,
    fetch: &mut impl FnMut(&str) -> Result<Vec<u8>, FetchError>,
) -> Result<Vec<u8>, FetchError> {
    control
        .root
        .ensure_directory(Path::new(super::CACHE_DIRECTORY))
        .map_err(FetchError::local)?;
    for attempt in 1..=5 {
        let mut lock = control.lock()?;
        let mut state = read_state(&mut lock)?;
        control.wait(&state)?;
        control.check_cancelled()?;
        // 发起请求前持久化最小间隔，进程中断后下一个调用者仍需等待。
        state.next_request_ms = control
            .now_ms()?
            .saturating_add(control.interval.as_millis() as u64);
        state.reason = "请求间隔".to_owned();
        save_state(&mut lock, &state)?;
        (control.progress)(format!("BWiki 查询 {title}：第 {attempt}/5 次尝试"));
        let response = fetch(title).and_then(temporary_response);
        match response {
            Ok(bytes) => {
                state.failures = 0;
                state.next_request_ms = control
                    .now_ms()?
                    .saturating_add(control.interval.as_millis() as u64);
                save_state(&mut lock, &state)?;
                return Ok(bytes);
            }
            Err(mut error) => {
                if let Some(retry_after) = error.retry_after {
                    state.failures = state.failures.saturating_add(1);
                    let delay = Duration::from_secs(
                        (30_u64 << state.failures.saturating_sub(1).min(4)).min(300),
                    )
                    .max(retry_after)
                    .max(control.interval);
                    state.next_request_ms = control
                        .now_ms()?
                        .saturating_add(delay.as_millis().min(u64::MAX as u128) as u64);
                    state.reason = error.message.chars().take(512).collect();
                    save_state(&mut lock, &state).map_err(|state_error| {
                        FetchError::local(format!(
                            "保存 BWiki 冷却状态失败：{}；原请求失败：{}",
                            state_error.message, error.message
                        ))
                    })?;
                    (control.progress)(format!(
                        "BWiki 冷却：{}；等待 {} 秒",
                        error.message,
                        delay.as_secs()
                    ));
                    if attempt < 5 {
                        continue;
                    }
                    error.message =
                        format!("{}（已尝试 {attempt} 次，暂停本次在线查询）", error.message);
                }
                return Err(error);
            }
        }
    }
    unreachable!("末次尝试必定返回")
}

pub(super) fn fetch(
    root: &ToolRoot,
    title: &str,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(String),
) -> Result<Vec<u8>, FetchError> {
    let settings =
        crate::adapters::settings::Settings::load(root.as_path()).map_err(FetchError::local)?;
    let mut control = RequestControl {
        root,
        interval: Duration::from_secs(settings.acquisition_request_interval_seconds().into()),
        cancelled,
        progress,
        clock: &SystemClock,
    };
    fetch_with_retry(&mut control, title, &mut fetch_once)
}

#[cfg(windows)]
fn fetch_once(title: &str) -> Result<Vec<u8>, FetchError> {
    fetch_once_at(title, "wiki.biligame.com", 443, true)
}

#[cfg(windows)]
fn fetch_once_at(title: &str, host: &str, port: u16, secure: bool) -> Result<Vec<u8>, FetchError> {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Networking::WinHttp::*;

    struct Handle(*mut c_void);
    impl Handle {
        fn new(value: *mut c_void, operation: &str) -> Result<Self, FetchError> {
            if value.is_null() {
                Err(network_error(operation))
            } else {
                Ok(Self(value))
            }
        }
    }
    impl Drop for Handle {
        fn drop(&mut self) {
            // 句柄始终由本次请求独占，按请求、连接、会话的逆序释放。
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
    fn network_error(operation: &str) -> FetchError {
        let error = std::io::Error::last_os_error();
        let retryable = matches!(
            error.raw_os_error().map(|code| code as u32),
            Some(
                ERROR_WINHTTP_TIMEOUT
                    | ERROR_WINHTTP_CANNOT_CONNECT
                    | ERROR_WINHTTP_CONNECTION_ERROR
                    | ERROR_WINHTTP_NAME_NOT_RESOLVED
            )
        );
        FetchError {
            message: format!("{operation}: {error}"),
            stop_batch: true,
            retry_after: retryable.then_some(Duration::from_secs(5)),
        }
    }
    fn checked(result: i32, operation: &str) -> Result<(), FetchError> {
        if result == 0 {
            Err(network_error(operation))
        } else {
            Ok(())
        }
    }
    let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let agent = wide("AzurLaneWorkbook/0.1 (ship acquisition reference)");
    let access = if host == "127.0.0.1" {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    };
    let host = wide(host);
    let verb = wide("GET");
    let path = wide(&format!(
        "/blhx/api.php?action=parse&prop=text&format=json&redirects=1&maxlag=5&page={}",
        encode_query(title)
    ));
    let started = Instant::now();
    // TLS 验证和代理发现交由 Windows，所有缓冲区在同步调用期间保持有效。
    unsafe {
        let session = Handle::new(
            WinHttpOpen(agent.as_ptr(), access, null(), null(), 0),
            "打开 BWiki 网络会话",
        )?;
        checked(
            WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 5_000),
            "设置 BWiki 超时",
        )?;
        let connection = Handle::new(
            WinHttpConnect(session.0, host.as_ptr(), port, 0),
            "连接 BWiki",
        )?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                path.as_ptr(),
                null(),
                null(),
                null(),
                if secure { WINHTTP_FLAG_SECURE } else { 0 },
            ),
            "建立 BWiki 请求",
        )?;
        checked(
            WinHttpSendRequest(request.0, null(), 0, null(), 0, 0, 0),
            "发送 BWiki 请求",
        )?;
        checked(
            WinHttpReceiveResponse(request.0, null_mut()),
            "接收 BWiki 响应",
        )?;
        let mut status = 0_u32;
        let mut size = size_of::<u32>() as u32;
        checked(
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut size,
                null_mut(),
            ),
            "读取 BWiki HTTP 状态",
        )?;
        let mut header = [0_u16; 128];
        let mut header_size = size_of_val(&header) as u32;
        let server_delay = if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_RETRY_AFTER,
            null(),
            header.as_mut_ptr().cast(),
            &mut header_size,
            null_mut(),
        ) != 0
        {
            let value = String::from_utf16_lossy(
                &header[..header.iter().position(|c| *c == 0).unwrap_or(header.len())],
            );
            retry_after(&value, SystemTime::now())
        } else {
            None
        };
        if status != 200 {
            let mut error = FetchError::http_status(status);
            if error.retry_after.is_some() {
                error.retry_after = server_delay.or(error.retry_after);
            }
            return Err(error);
        }
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 16_384];
        loop {
            if started.elapsed() > Duration::from_secs(20) {
                return Err(FetchError {
                    message: "BWiki 请求超过 20 秒预算".to_owned(),
                    stop_batch: true,
                    retry_after: Some(Duration::from_secs(5)),
                });
            }
            let mut read = 0;
            checked(
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                ),
                "读取 BWiki 正文",
            )?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read as usize]);
            if bytes.len() > 8 * 1024 * 1024 {
                return Err(FetchError {
                    message: "BWiki 响应超过 8 MiB".to_owned(),
                    stop_batch: true,
                    retry_after: None,
                });
            }
        }
        // 部分 API 限流使用 HTTP 200，正文重试同样必须遵守响应头。
        if let Some(delay) = server_delay {
            temporary_response(bytes).map_err(|mut error| {
                error.retry_after = Some(delay);
                error
            })
        } else {
            Ok(bytes)
        }
    }
}

#[cfg(not(windows))]
fn fetch_once(_title: &str) -> Result<Vec<u8>, FetchError> {
    Err(FetchError {
        message: "BWiki 在线更新需要 Windows，当前平台可使用已有缓存".to_owned(),
        stop_batch: true,
        retry_after: None,
    })
}

fn encode_query(value: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            write!(&mut encoded, "%{byte:02X}").expect("写入字符串不会失败");
        }
    }
    encoded
}

#[cfg(windows)]
fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    use windows_sys::Win32::{
        Foundation::{FILETIME, SYSTEMTIME},
        Networking::WinHttp::WinHttpTimeToSystemTime,
        System::Time::SystemTimeToFileTime,
    };
    let wide: Vec<u16> = value.trim().encode_utf16().chain(Some(0)).collect();
    let mut date = SYSTEMTIME::default();
    let mut filetime = FILETIME::default();
    // WinHTTP 负责 HTTP 日期语法，FILETIME 以 1601 年为起点、100 ns 为单位。
    unsafe {
        if WinHttpTimeToSystemTime(wide.as_ptr(), &mut date) == 0
            || SystemTimeToFileTime(&date, &mut filetime) == 0
        {
            return None;
        }
    }
    let ticks = (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime);
    let unix_ticks = ticks.checked_sub(116_444_736_000_000_000)?;
    let date = UNIX_EPOCH.checked_add(Duration::from_nanos(unix_ticks.checked_mul(100)?))?;
    Some(date.duration_since(now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct FakeClock(Cell<u64>);
    impl Clock for FakeClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_millis(self.0.get())
        }
        fn sleep(&self, duration: Duration) {
            self.0.set(self.0.get() + duration.as_millis() as u64);
        }
    }

    #[test]
    fn retries_share_cooldown_and_preserve_server_delay() {
        let fixture = super::super::tests::Fixture::new();
        let source = fixture.source();
        let clock = FakeClock(Cell::new(1000000));
        let mut notes = Vec::new();
        let mut control = RequestControl {
            root: &source.root,
            interval: Duration::from_secs(2),
            cancelled: &|| false,
            progress: &mut |note| notes.push(note),
            clock: &clock,
        };
        let mut calls = Vec::new();
        let error = fetch_with_retry(&mut control, "标枪", &mut |_| {
            calls.push(clock.0.get());
            Err(FetchError::http_status(567))
        })
        .unwrap_err();
        assert_eq!(calls, [1000000, 1030000, 1090000, 1210000, 1450000]);
        assert!(error.stop_batch);
        assert!(error.message.contains("5 次"));
        let mut next = 0;
        fetch_with_retry(&mut control, "拉菲", &mut |_| {
            next = clock.0.get();
            Ok(br#"{"error":{"code":"missingtitle"}}"#.to_vec())
        })
        .unwrap();
        assert_eq!(next, 1750000);
        let mut attempt = 0;
        let mut after_server_delay = 0;
        fetch_with_retry(&mut control, "拉菲", &mut |_| {
            attempt += 1;
            if attempt == 1 {
                Err(FetchError {
                    message: "429".to_owned(),
                    stop_batch: true,
                    retry_after: Some(Duration::from_secs(600)),
                })
            } else {
                after_server_delay = clock.0.get();
                Ok(br#"{"error":{"code":"missingtitle"}}"#.to_vec())
            }
        })
        .unwrap();
        assert_eq!(after_server_delay, 2352000);
    }

    #[test]
    fn transient_bodies_retry_but_missing_and_structural_errors_do_not() {
        let fixture = super::super::tests::Fixture::new();
        let source = fixture.source();
        let clock = FakeClock(Cell::new(1000000));
        let mut control = RequestControl {
            root: &source.root,
            interval: Duration::from_secs(2),
            cancelled: &|| false,
            progress: &mut |_| {},
            clock: &clock,
        };
        let mut responses = [
            b"<html>challenge</html>".to_vec(),
            br#"{"error":{"code":"maxlag"}}"#.to_vec(),
            br#"{"error":{"code":"missingtitle"}}"#.to_vec(),
        ]
        .into_iter();
        let body = fetch_with_retry(&mut control, "标枪", &mut |_| {
            Ok(responses.next().unwrap())
        })
        .unwrap();
        assert!(matches!(
            super::super::parser::parse_response(&body),
            super::super::parser::ParsedPage::Missing
        ));
        assert_eq!(clock.0.get(), 1090000);
        let mut calls = 0;
        fetch_with_retry(&mut control, "标枪", &mut |_| {
            calls += 1;
            Ok(br#"{"parse":{}}"#.to_vec())
        })
        .unwrap();
        assert_eq!(calls, 1);
    }

    #[test]
    fn cancellation_interrupts_cooldown_without_another_request() {
        let fixture = super::super::tests::Fixture::new();
        let source = fixture.source();
        let clock = FakeClock(Cell::new(1000000));
        let mut control = RequestControl {
            root: &source.root,
            interval: Duration::from_secs(2),
            cancelled: &|| clock.0.get() >= 1000500,
            progress: &mut |_| {},
            clock: &clock,
        };
        let mut calls = 0;
        let error = fetch_with_retry(&mut control, "标枪", &mut |_| {
            calls += 1;
            Err(FetchError::http_status(429))
        })
        .unwrap_err();
        assert_eq!(calls, 1);
        assert!(error.message.contains("取消"));
    }

    #[test]
    fn request_process_child() {
        let Some(directory) = std::env::var_os("AZLW_ACQUISITION_REQUEST_TEST_ROOT") else {
            return;
        };
        let root = ToolRoot::open(Path::new(&directory)).unwrap();
        let mut control = RequestControl {
            root: &root,
            interval: Duration::from_millis(100),
            cancelled: &|| false,
            progress: &mut |_| {},
            clock: &SystemClock,
        };
        fetch_with_retry(&mut control, "标枪", &mut |_| {
            let mut evidence = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(root.as_path().join("requests.txt"))
                .unwrap();
            writeln!(
                evidence,
                "start {}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            )
            .unwrap();
            std::thread::sleep(Duration::from_millis(80));
            writeln!(
                evidence,
                "end {}",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis()
            )
            .unwrap();
            Ok(br#"{"error":{"code":"missingtitle"}}"#.to_vec())
        })
        .unwrap();
    }

    #[test]
    fn separate_processes_share_the_request_lock_and_interval() {
        let fixture = super::super::tests::Fixture::new();
        let source = fixture.source();
        let start = || {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "adapters::workbook::ship_acquisition::http::tests::request_process_child",
                    "--nocapture",
                ])
                .env("AZLW_ACQUISITION_REQUEST_TEST_ROOT", source.root.as_path())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut first = start();
        let mut second = start();
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());
        let evidence = std::fs::read_to_string(source.root.as_path().join("requests.txt")).unwrap();
        let records: Vec<_> = evidence
            .lines()
            .map(|line| {
                let (kind, time) = line.split_once(' ').unwrap();
                (kind, time.parse::<u64>().unwrap())
            })
            .collect();
        assert_eq!(
            records.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
            ["start", "end", "start", "end"]
        );
        assert!(records[2].1 - records[1].1 >= 100, "{evidence}");
    }

    #[cfg(windows)]
    #[test]
    fn parses_retry_after_seconds_and_http_dates() {
        let now = UNIX_EPOCH + Duration::from_secs(1445412480);
        assert_eq!(retry_after("120", now), Some(Duration::from_secs(120)));
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:30:00 GMT", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            retry_after("Wed, 21 Oct 2015 07:20:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after("invalid", now), None);
    }
    #[cfg(windows)]
    #[test]
    fn local_winhttp_reads_retry_after_and_response_body() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let mut responses = [
                "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 12\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                "HTTP/1.1 200 OK\r\nRetry-After: 600\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            ]
            .into_iter();
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 2048];
                let _ = stream.read(&mut buffer).unwrap();
                stream
                    .write_all(responses.next().unwrap().as_bytes())
                    .unwrap();
            }
        });
        let limited = fetch_once_at("标枪", "127.0.0.1", port, false).unwrap_err();
        assert!(limited.message.contains("503"), "{}", limited.message);
        assert_eq!(limited.retry_after, Some(Duration::from_secs(12)));
        let body = fetch_once_at("标枪", "127.0.0.1", port, false).unwrap();
        assert_eq!(body, b"hello");
        let challenge = fetch_once_at("标枪", "127.0.0.1", port, false).unwrap_err();
        assert_eq!(challenge.retry_after, Some(Duration::from_secs(600)));
        assert!(challenge.message.contains("非 JSON"));
        server.join().unwrap();
    }
}

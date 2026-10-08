//! 验证桌面窗口生命周期和正式程序图标资源。
#![cfg(target_os = "windows")]
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW,
};
#[test]
fn application_binary_embeds_file_icon() {
    use std::os::windows::ffi::OsStrExt;

    let path: Vec<u16> = Path::new(env!("CARGO_BIN_EXE_AzurLaneWorkbook"))
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let module = unsafe { LoadLibraryExW(path.as_ptr(), null_mut(), LOAD_LIBRARY_AS_DATAFILE) };
    assert!(!module.is_null(), "应能以数据文件打开正式程序");
    let group = unsafe { FindResourceW(module, 1 as _, 14 as _) };
    let icon = unsafe { FindResourceW(module, 1 as _, 3 as _) };
    unsafe {
        FreeLibrary(module);
    }
    assert!(!group.is_null(), "正式程序应嵌入 RT_GROUP_ICON");
    assert!(!icon.is_null(), "正式程序应嵌入 RT_ICON");
}

#[cfg(feature = "native-gui-test")]
mod lifecycle {
    use azur_lane_workbook::interfaces::native_gui::WINDOW_TITLE;
    use std::path::Path;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, PostMessageW, WM_CLOSE,
    };

    #[test]
    fn closes_window_after_cancelling_background_task() {
        let mut child = ChildGuard::spawn(Path::new(env!("CARGO_BIN_EXE_native_gui_fixture")));
        let window = wait_for_window(&mut child, Duration::from_secs(10));
        assert_eq!(matching_windows(child.id()).len(), 1);
        assert_ne!(unsafe { PostMessageW(window, WM_CLOSE, 0, 0) }, 0);
        assert!(child.wait_for_exit(Duration::from_secs(10)).success());
    }
    struct ChildGuard {
        child: Child,
    }

    impl ChildGuard {
        fn spawn(executable: &Path) -> Self {
            let child: Child = Command::new(executable)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("应能启动正式可执行文件");
            Self { child }
        }

        fn id(&self) -> u32 {
            self.child.id()
        }

        fn wait_for_exit(&mut self, timeout: Duration) -> ExitStatus {
            let deadline: Instant = Instant::now() + timeout;
            loop {
                if let Some(status) = self.child.try_wait().expect("应能读取进程状态") {
                    return status;
                }
                assert!(Instant::now() < deadline, "等待窗口进程退出超时");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                match self.child.kill() {
                    Ok(()) => {
                        if let Err(error) = self.child.wait() {
                            eprintln!("等待测试窗口进程清理失败: {error}");
                        }
                    }
                    Err(error) => eprintln!("终止测试窗口进程失败: {error}"),
                }
            }
        }
    }

    fn wait_for_window(child: &mut ChildGuard, timeout: Duration) -> HWND {
        let deadline: Instant = Instant::now() + timeout;
        loop {
            if let Some(window) = matching_windows(child.id()).into_iter().next() {
                return window;
            }
            if let Some(status) = child.child.try_wait().expect("应能读取进程状态") {
                panic!("窗口建立前进程已经退出: {status}");
            }
            assert!(
                Instant::now() < deadline,
                "等待原生窗口建立超时，进程 {} 的顶层窗口为 {:?}",
                child.id(),
                process_windows(child.id())
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn matching_windows(process_id: u32) -> Vec<HWND> {
        process_windows(process_id)
            .into_iter()
            .filter(|window| window.title == WINDOW_TITLE)
            .map(|window| window.handle)
            .collect()
    }

    fn process_windows(process_id: u32) -> Vec<WindowInfo> {
        let mut query: WindowQuery = WindowQuery {
            process_id,
            windows: Vec::new(),
        };
        let result: i32 = unsafe {
            EnumWindows(
                Some(collect_matching_window),
                (&mut query as *mut WindowQuery) as isize,
            )
        };
        assert_ne!(result, 0, "枚举 Windows 顶层窗口失败");
        query.windows
    }

    struct WindowQuery {
        process_id: u32,
        windows: Vec<WindowInfo>,
    }

    #[derive(Debug)]
    struct WindowInfo {
        handle: HWND,
        title: String,
    }

    unsafe extern "system" fn collect_matching_window(window: HWND, lparam: isize) -> i32 {
        let query: &mut WindowQuery = unsafe { &mut *(lparam as *mut WindowQuery) };
        let mut owner_process_id: u32 = 0;
        unsafe {
            GetWindowThreadProcessId(window, &mut owner_process_id);
        }
        if owner_process_id == query.process_id {
            query.windows.push(WindowInfo {
                handle: window,
                title: window_title(window).unwrap_or_default(),
            });
        }
        1
    }

    fn window_title(window: HWND) -> Option<String> {
        let mut buffer: [u16; 256] = [0; 256];
        let length: i32 =
            unsafe { GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32) };
        (length > 0).then(|| String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

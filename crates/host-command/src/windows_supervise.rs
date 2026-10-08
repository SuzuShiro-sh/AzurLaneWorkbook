//! 把本次命令放进独立作业，并在统一截止时间内排空它的输出管道。
//!
//! 作业只包含这次启动的进程。子进程可以显式脱离作业，因此需要在命令返回后继续运行的服务不会被收尾终止。
//! 关闭作业句柄本身不结束进程；只有到截止时间后才终止仍留在作业里的进程。

use std::io::{Error, Read};
use std::os::windows::io::AsRawHandle;
use std::process::{Child, ChildStderr, ChildStdout, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_MORE_FILES, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::PeekNamedPipe;
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, THREAD_SUSPEND_RESUME, WaitForSingleObject,
};

use crate::{
    CollectedOutput, HOST_COMMAND_CLEANUP_BUDGET, MAX_OUTPUT_BYTES, NativeCommandError,
    format_duration_budget, timeout_failure_message,
};

const DRAIN_SLICE: Duration = Duration::from_millis(50);

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE, action: &str) -> Result<Self, Error> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(Error::other(format!(
                "{action}: {}",
                Error::last_os_error()
            )));
        }
        Ok(Self(handle))
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

struct CommandJob(OwnedHandle);

impl CommandJob {
    fn create(stage: &'static str) -> Result<Self, NativeCommandError> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        let job = OwnedHandle::new(handle, "创建进程作业失败")
            .map_err(|error| failed(stage, error.to_string()))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        let configured = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(failed(
                stage,
                format!("设置进程作业失败: {}", Error::last_os_error()),
            ));
        }
        Ok(Self(job))
    }

    fn assign_and_resume(
        &self,
        child: &Child,
        stage: &'static str,
    ) -> Result<(), NativeCommandError> {
        let assigned = unsafe { AssignProcessToJobObject(self.0.0, child.as_raw_handle()) };
        if assigned == 0 {
            return Err(failed(
                stage,
                format!("把本次命令放入进程作业失败: {}", Error::last_os_error()),
            ));
        }
        resume_primary_thread(child.id())
            .map_err(|error| failed(stage, format!("恢复本次命令主线程失败: {error}")))
    }

    fn terminate(&self) -> Result<(), Error> {
        let terminated = unsafe { TerminateJobObject(self.0.0, 1) };
        if terminated == 0 {
            return Err(Error::last_os_error());
        }
        Ok(())
    }
}

fn resume_primary_thread(process_id: u32) -> Result<(), Error> {
    let started = Instant::now();
    loop {
        if let Some(thread_id) = find_primary_thread(process_id)? {
            return resume_thread(thread_id);
        }
        if started.elapsed() >= Duration::from_millis(200) {
            return Err(Error::other("未找到挂起进程的主线程"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn find_primary_thread(process_id: u32) -> Result<Option<u32>, Error> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    let snapshot = OwnedHandle::new(snapshot, "枚举线程失败")?;
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..THREADENTRY32::default()
    };
    let mut found = None;
    let mut has_entry = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
    while has_entry {
        if entry.th32OwnerProcessID == process_id {
            found = Some(entry.th32ThreadID);
            break;
        }
        has_entry = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
    }
    if found.is_none() {
        let error = unsafe { GetLastError() };
        if error != 0 && error != ERROR_NO_MORE_FILES {
            return Err(Error::from_raw_os_error(error as i32));
        }
    }
    Ok(found)
}

fn resume_thread(thread_id: u32) -> Result<(), Error> {
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    let thread = OwnedHandle::new(thread, "打开主线程失败")?;
    loop {
        let previous = unsafe { ResumeThread(thread.0) };
        if previous == u32::MAX {
            return Err(Error::last_os_error());
        }
        if previous <= 1 {
            return Ok(());
        }
    }
}

struct PipeCapture<R> {
    reader: R,
    raw: HANDLE,
    buffer: Vec<u8>,
    exceeded: bool,
    eof: bool,
    scratch: Vec<u8>,
}

impl<R: Read> PipeCapture<R> {
    fn new(reader: R, raw: HANDLE) -> Self {
        Self {
            reader,
            raw,
            buffer: Vec::new(),
            exceeded: false,
            eof: false,
            scratch: vec![0; 64 * 1024],
        }
    }

    fn pull(&mut self) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        match pipe_available(self.raw)? {
            PipeReady::Eof => {
                self.eof = true;
                Ok(false)
            }
            PipeReady::Empty => Ok(false),
            PipeReady::Ready(available) => {
                let want = usize::try_from(available)
                    .unwrap_or(self.scratch.len())
                    .min(self.scratch.len());
                let count = self.reader.read(&mut self.scratch[..want])?;
                if count == 0 {
                    self.eof = true;
                    return Ok(false);
                }
                self.append_read(count);
                Ok(true)
            }
        }
    }

    fn append_read(&mut self, count: usize) {
        if self.buffer.len() >= MAX_OUTPUT_BYTES {
            self.exceeded = true;
            return;
        }
        let remaining = MAX_OUTPUT_BYTES - self.buffer.len();
        let copied = remaining.min(count);
        {
            let PipeCapture {
                buffer, scratch, ..
            } = self;
            buffer.extend_from_slice(&scratch[..copied]);
        }
        if copied < count {
            self.exceeded = true;
        }
    }
}

enum PipeReady {
    Eof,
    Empty,
    Ready(u32),
}

fn pipe_available(handle: HANDLE) -> Result<PipeReady, Error> {
    let mut available = 0u32;
    let visible = unsafe {
        PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if visible == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_BROKEN_PIPE {
            return Ok(PipeReady::Eof);
        }
        return Err(Error::from_raw_os_error(error as i32));
    }
    if available == 0 {
        Ok(PipeReady::Empty)
    } else {
        Ok(PipeReady::Ready(available))
    }
}

pub(crate) fn collect_output(
    child: &mut Child,
    stdout_pipe: ChildStdout,
    stderr_pipe: ChildStderr,
    stage: &'static str,
    timeout: Duration,
) -> Result<CollectedOutput, NativeCommandError> {
    let job = match CommandJob::create(stage) {
        Ok(job) => job,
        Err(error) => {
            let note = stop_unsupervised_child(child);
            return Err(append_failure(error, note));
        }
    };
    if let Err(error) = job.assign_and_resume(child, stage) {
        let note = stop_unsupervised_child(child);
        return Err(append_failure(error, note));
    }

    let stdout_raw = stdout_pipe.as_raw_handle();
    let stderr_raw = stderr_pipe.as_raw_handle();
    let mut stdout = PipeCapture::new(stdout_pipe, stdout_raw);
    let mut stderr = PipeCapture::new(stderr_pipe, stderr_raw);
    let deadline = Instant::now() + timeout;
    let mut kill_on_drop = KillOnDrop {
        job: &job,
        armed: true,
    };

    loop {
        let stdout_progress = pull_pipe(&mut stdout, "stdout", stage)?;
        let stderr_progress = pull_pipe(&mut stderr, "stderr", stage)?;
        let status = wait_status(child, stage)?;
        if stdout.eof
            && stderr.eof
            && let Some(status) = status
        {
            kill_on_drop.armed = false;
            return Ok(finish(status, stdout, stderr));
        }
        if Instant::now() >= deadline {
            break;
        }
        if stdout_progress || stderr_progress {
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let slice = remaining.min(DRAIN_SLICE);
        if status.is_some() {
            thread::sleep(slice);
        } else {
            wait_for_process(child, slice);
        }
    }

    let process_exited = wait_status(child, stage)?.is_some();
    kill_on_drop.armed = false;
    let kill_error = job.terminate().err();
    let cleanup_deadline = Instant::now() + HOST_COMMAND_CLEANUP_BUDGET;
    while Instant::now() < cleanup_deadline {
        pull_pipe(&mut stdout, "stdout", stage)?;
        pull_pipe(&mut stderr, "stderr", stage)?;
        let status = wait_status(child, stage)?;
        if stdout.eof
            && stderr.eof
            && let Some(status) = status
        {
            if process_exited {
                return Ok(finish(status, stdout, stderr));
            }
            return Err(cleanup_timeout(stage, timeout, kill_error.as_ref(), false));
        }
        thread::sleep(Duration::from_millis(10));
    }

    let status = wait_status(child, stage)?;
    let Some(status) = status else {
        return Err(cleanup_timeout(stage, timeout, kill_error.as_ref(), true));
    };
    if !process_exited {
        return Err(cleanup_timeout(stage, timeout, kill_error.as_ref(), false));
    }
    if !stdout.eof || !stderr.eof {
        return Err(failed(
            stage,
            pipe_still_open_message(timeout, kill_error.as_ref()),
        ));
    }
    Ok(finish(status, stdout, stderr))
}

fn pull_pipe<R: Read>(
    pipe: &mut PipeCapture<R>,
    stream: &str,
    stage: &'static str,
) -> Result<bool, NativeCommandError> {
    pipe.pull()
        .map_err(|error| failed(stage, format!("读取 {stream} 失败: {error}")))
}

fn wait_status(
    child: &mut Child,
    stage: &'static str,
) -> Result<Option<ExitStatus>, NativeCommandError> {
    child
        .try_wait()
        .map_err(|error| failed(stage, format!("等待失败: {error}")))
}

fn wait_for_process(child: &Child, slice: Duration) {
    let millis = u32::try_from(slice.as_millis()).unwrap_or(u32::MAX);
    unsafe {
        WaitForSingleObject(child.as_raw_handle(), millis);
    }
}

fn finish<R, S>(
    status: ExitStatus,
    stdout: PipeCapture<R>,
    stderr: PipeCapture<S>,
) -> CollectedOutput {
    CollectedOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout: stdout.buffer,
        stderr: stderr.buffer,
        stdout_exceeded: stdout.exceeded,
        stderr_exceeded: stderr.exceeded,
    }
}

fn stop_unsupervised_child(child: &mut Child) -> Option<String> {
    if let Err(error) = child.kill() {
        return Some(format!("终止尚未纳入作业的进程失败: {error}"));
    }
    if let Err(error) = child.wait() {
        return Some(format!("等待尚未纳入作业的进程失败: {error}"));
    }
    None
}

fn append_failure(error: NativeCommandError, note: Option<String>) -> NativeCommandError {
    let Some(note) = note else {
        return error;
    };
    let NativeCommandError::Failed { stage, message } = error;
    failed(stage, format!("{message}；{note}"))
}

fn cleanup_timeout(
    stage: &'static str,
    timeout: Duration,
    kill_error: Option<&Error>,
    still_running: bool,
) -> NativeCommandError {
    let mut detail = String::new();
    if let Some(error) = kill_error {
        detail.push_str(&format!("终止本次命令进程作业失败: {error}"));
    }
    if still_running {
        if !detail.is_empty() {
            detail.push('；');
        }
        detail.push_str("收尾预算内进程仍未退出");
    }
    failed(stage, timeout_failure_message(timeout, &detail))
}

fn pipe_still_open_message(timeout: Duration, kill_error: Option<&Error>) -> String {
    let mut message = format!(
        "进程已退出，但输出管道在 {}截止前未关闭；收尾预算 {}已用尽",
        format_duration_budget(timeout),
        format_duration_budget(HOST_COMMAND_CLEANUP_BUDGET),
    );
    if let Some(error) = kill_error {
        message.push_str(&format!("；终止本次命令进程作业失败: {error}"));
    }
    message
}

fn failed(stage: &'static str, message: String) -> NativeCommandError {
    NativeCommandError::Failed { stage, message }
}

struct KillOnDrop<'a> {
    job: &'a CommandJob,
    armed: bool,
}

impl Drop for KillOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.job.terminate();
        }
    }
}

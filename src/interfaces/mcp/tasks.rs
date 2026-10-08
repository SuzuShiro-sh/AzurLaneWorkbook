//! MCP 长任务的提交去重、持久化结果和协作式取消；业务仍由统一分派器执行。

use super::dispatch;
use crate::{application::ExecutionCancellation, bootstrap::OperationSignals};
use rmcp::model::CallToolResult;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use suzushiro_content_digest::{is_canonical_sha256, sha256_bytes};
use suzushiro_controlled_root::{ControlledRoot, ControlledRootError};
use suzushiro_task_runtime::{BackgroundTask, TaskContext, TaskEvent, TaskSignalError};

type Result<T> = std::result::Result<T, TaskError>;
#[derive(Debug, thiserror::Error)]
pub(super) enum TaskError {
    #[error("{0}")]
    Invalid(String),
    #[error("MCP 服务正在关闭，不能提交新任务")]
    Closing,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Root(#[from] ControlledRootError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
impl From<&'static str> for TaskError {
    fn from(message: &'static str) -> Self {
        Self::Invalid(message.to_owned())
    }
}
impl TaskError {
    pub(super) fn into_mcp(self) -> rmcp::ErrorData {
        match self {
            Self::Invalid(message) => rmcp::ErrorData::invalid_params(message, None),
            Self::Closing => rmcp::ErrorData::invalid_request(self.to_string(), None),
            other => {
                eprintln!("MCP 任务存储失败: {other}");
                rmcp::ErrorData::internal_error(other.to_string(), None)
            }
        }
    }
}
const DIRECTORY: &str = "data/mcp-tasks";

pub(super) fn is_long(name: &str) -> bool {
    matches!(name, "workbook_generate" | "acquisition_update")
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    task_id: String,
    request_id: String,
    tool: String,
    arguments: Map<String, Value>,
    state: String,
    updated_at_ms: u128,
    progress: Value,
    result: Option<CallToolResult>,
    diagnostic: Option<String>,
    persistence_error: Option<String>,
}
impl Record {
    fn active(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running" | "cancelling")
    }
    fn view(&self) -> Value {
        json!({"task_id":self.task_id,"request_id":self.request_id,"tool":self.tool,
            "state":self.state,"updated_at_ms":self.updated_at_ms,"progress":self.progress,
            "result":self.result.as_ref().and_then(|r|r.structured_content.as_ref()),"diagnostic":self.diagnostic,"persistence_error":self.persistence_error})
    }
}
struct Entry {
    record: Mutex<Record>,
    cancelled: Arc<AtomicBool>,
}

pub(super) struct Tasks {
    root: PathBuf,
    active: Mutex<HashMap<String, Arc<Entry>>>,
    monitors: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    stopping: AtomicBool,
}
impl Tasks {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: Mutex::new(HashMap::new()),
            monitors: Mutex::new(Vec::new()),
            stopping: AtomicBool::new(false),
        }
    }

    pub(super) fn submit(
        self: &Arc<Self>,
        name: String,
        mut args: Map<String, Value>,
        gate: Arc<tokio::sync::Mutex<()>>,
        runtime: tokio::runtime::Handle,
    ) -> Result<Value> {
        let request_id = args
            .remove("request_id")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or("缺少 request_id")?;
        if !valid_request_id(&request_id) {
            return Err("request_id 必须为 1 至 128 个字符的非空文本".into());
        }
        let id = sha256_bytes(request_id.as_bytes());
        let root = ControlledRoot::open(&self.root)?;
        root.ensure_directory(Path::new(DIRECTORY))?;
        // 本进程注册和跨进程提交都在短锁内完成；业务期间仅持有该任务自己的租约。
        let mut active = self.active.lock().unwrap();
        if self.stopping.load(Ordering::Acquire) {
            return Err(TaskError::Closing);
        }
        let _submission = root.lock_file(
            Path::new("data/mcp-tasks/submit.lock"),
            Duration::from_secs(2),
        )?;
        if let Some(entry) = active.get(&id) {
            return same_request(&entry.record.lock().unwrap(), &name, &args);
        }
        if root.as_path().join(relative(&id, "json")).exists() {
            let record = self.read(&id)?;
            return same_request(&record, &name, &args);
        }
        let lease = root.lock_file(&relative(&id, "lock"), Duration::ZERO)?;
        let record = Record {
            task_id: id.clone(),
            request_id,
            tool: name.clone(),
            arguments: args.clone(),
            state: "queued".into(),
            updated_at_ms: now(),
            progress: json!({"message":"等待业务队列"}),
            result: None,
            diagnostic: None,
            persistence_error: None,
        };
        save(&root, &record)?;
        let submitted = record.view();
        let entry = Arc::new(Entry {
            record: Mutex::new(record),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        active.insert(id.clone(), entry.clone());
        let notify = Arc::new(tokio::sync::Notify::new());
        let worker_notify = notify.clone();
        let worker_entry = entry.clone();
        let business_root = self.root.clone();
        let worker_runtime = runtime.clone();
        let task = BackgroundTask::new(move |context| {
            let signals = TaskSignals {
                context,
                cancelled: worker_entry.cancelled.clone(),
            };
            let guard = worker_runtime.block_on(async {
                let pending = gate.clone().lock_owned();
                tokio::pin!(pending);
                loop {
                    if signals.cancelled() {
                        return None;
                    }
                    tokio::select! {
                        guard = &mut pending => return Some(guard),
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {},
                    }
                }
            });
            let _guard = guard;
            Ok(dispatch::run(&business_root, &name, args.clone(), &signals))
        })
        .spawn(move || worker_notify.notify_one());
        let manager = self.clone();
        let monitor = runtime.spawn(async move {
            let _lease = lease;
            let mut task = Some(task);
            loop {
                tokio::select! {
                    _ = notify.notified() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
                if root.as_path().join(relative(&id, "cancel")).exists() {
                    entry.cancelled.store(true, Ordering::Release);
                }
                if entry.cancelled.load(Ordering::Acquire) { task.as_ref().unwrap().cancel(); }
                let mut terminal = false;
                let mut changed = false;
                {
                    let mut record = entry.record.lock().unwrap();
                    if entry.cancelled.load(Ordering::Acquire) && record.active() && record.state != "cancelling" {
                        record.state = "cancelling".into(); changed = true;
                    }
                    while let Ok(event) = task.as_ref().unwrap().try_recv() {
                        changed = true;
                        match event {
                            TaskEvent::Progress(progress) => {
                                if record.state == "queued" { record.state = "running".into(); }
                                record.progress = json!({"message":progress.message(),"percent":progress.percent(),"units":progress.units()});
                            },
                            TaskEvent::Succeeded { output } => {
                                record.state = match output.structured_content.as_ref().and_then(|v|v["status"].as_str()) {
                                    Some("ok") => "succeeded", Some("cancelled") => "cancelled", Some("incomplete") => "incomplete", Some("unknown") => "unknown", _ => "failed",
                                }.into();
                                record.result = Some(output); terminal = true;
                            },
                            TaskEvent::Failed { failure } => {
                                record.state = "unknown".into();
                                record.diagnostic = Some(format!("{}: {}", failure.user_message(), failure.detail())); terminal = true;
                            },
                            TaskEvent::Cancelled => { record.state = "cancelled".into(); terminal = true; },
                        }
                    }
                    if changed {
                        record.updated_at_ms = now();
                        persist(&root, &mut record);
                    }
                }
                if terminal {
                    if let Err(error) = task.take().unwrap().finish() { eprintln!("回收 MCP 任务 {id} 失败: {error}"); }
                    // 仅重试本地终态落盘，不重新执行业务；长期存储故障仍可通过查询重试保存。
                    for _ in 0..2 {
                        if entry.record.lock().unwrap().persistence_error.is_none() { break; }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        persist(&root, &mut entry.record.lock().unwrap());
                    }
                    // 持久化成功后释放完整结果的内存；失败时保留可查询结果。
                    let mut active = manager.active.lock().unwrap();
                    if entry.record.lock().unwrap().persistence_error.is_none() { active.remove(&id); }
                    break;
                }
            }
        });
        let mut monitors = self.monitors.lock().unwrap();
        monitors.retain(|task| !task.is_finished());
        monitors.push(monitor);
        Ok(submitted)
    }

    pub(super) fn query(&self, args: &Map<String, Value>, cancel: bool) -> Result<Value> {
        let id = match (
            args.get("task_id").and_then(Value::as_str),
            args.get("request_id").and_then(Value::as_str),
        ) {
            (Some(id), None) if is_canonical_sha256(id) => id.to_owned(),
            (None, Some(request)) if valid_request_id(request) => sha256_bytes(request.as_bytes()),
            _ => {
                return Err(
                    "task_id 与 request_id 必须恰好提供一个：task_id 复制提交返回的完整 64 位小写十六进制编号；request_id 使用原提交编号，不能临时生成".into(),
                );
            }
        };
        let mut active = self.active.lock().unwrap();
        if let Some(entry) = active.get(&id).cloned() {
            let mut record = entry.record.lock().unwrap();
            if cancel && record.active() {
                entry.cancelled.store(true, Ordering::Release);
                record.state = "cancelling".into();
                record.updated_at_ms = now();
            }
            if !record.active() && record.persistence_error.is_some() {
                let root = ControlledRoot::open(&self.root)?;
                match root.lock_file(&relative(&id, "lock"), Duration::ZERO) {
                    Ok(_lease) => {
                        if persist(&root, &mut record) {
                            active.remove(&id);
                        }
                    }
                    Err(error) if lock_busy(&error) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            return Ok(record.view());
        }
        drop(active);
        let record = self.read(&id)?;
        if cancel && record.active() {
            let root = ControlledRoot::open(&self.root)?;
            let path = relative(&id, "cancel");
            if !root.as_path().join(&path).exists() {
                let path = root.prepare_new_file(&path)?;
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                {
                    Ok(file) => file.sync_all()?,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(record.view())
    }

    fn read(&self, id: &str) -> Result<Record> {
        let root = ControlledRoot::open(&self.root)?;
        if !root.as_path().join(relative(id, "json")).try_exists()? {
            return Err(TaskError::Invalid(format!(
                "任务不存在：task_id={id}；请使用原提交返回的 task_id 或原 request_id，并确认连接的是同一安装目录的 MCP 服务。记录位置：{}",
                root.as_path().join(relative(id, "json")).display()
            )));
        }
        let path = root.existing_file(&relative(id, "json"))?;
        let mut record: Record = serde_json::from_slice(&fs::read(&path)?)?;
        if record.active() {
            match root.lock_file(&relative(id, "lock"), Duration::ZERO) {
                Ok(_lease) => {
                    // 取得租约说明执行进程已经退出。重新读取，避免把刚保存的结果判成中断。
                    record = serde_json::from_slice(&fs::read(&path)?)?;
                    if record.active() {
                        record.state = "interrupted".into();
                        record.updated_at_ms = now();
                        record.diagnostic = Some("执行进程已结束，最终结果未记录；请核对已发布文件、缓存及操作日志，不会自动重跑".into());
                        save(&root, &record)?;
                    }
                }
                Err(e) if lock_busy(&e) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(record)
    }

    pub(super) async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        for entry in self.active.lock().unwrap().values() {
            entry.cancelled.store(true, Ordering::Release);
        }
        let monitors = std::mem::take(&mut *self.monitors.lock().unwrap());
        for task in monitors {
            if let Err(error) = task.await {
                eprintln!("结束 MCP 任务监视失败: {error}");
            }
        }
        let pending: Vec<_> = self.active.lock().unwrap().keys().cloned().collect();
        for id in pending {
            if let Err(error) = self.query(json!({"task_id":id}).as_object().unwrap(), false) {
                eprintln!("关闭时保存 MCP 任务 {id} 失败: {error}");
            }
        }
    }
}

fn same_request(record: &Record, name: &str, args: &Map<String, Value>) -> Result<Value> {
    if record.tool != name || record.arguments != *args {
        return Err(TaskError::Invalid(format!(
            "request_id={} 已绑定工具 {}、task_id={}，本次工具为 {name} 或参数与原提交不同；先用 task_get 查询原任务。同一次操作重发需保持全部参数不变，独立新操作才使用新的 request_id",
            record.request_id, record.tool, record.task_id
        )));
    }
    Ok(record.view())
}
fn relative(id: &str, extension: &str) -> PathBuf {
    Path::new(DIRECTORY).join(format!("{id}.{extension}"))
}
fn valid_request_id(value: &str) -> bool {
    !value.trim().is_empty() && value.chars().count() <= 128
}
fn persist(root: &ControlledRoot, record: &mut Record) -> bool {
    record.persistence_error = None;
    if let Err(error) = save(root, record) {
        let detail = format!(
            "保存 MCP 任务 {} 状态失败: {error}；最新状态暂仅保留在当前进程",
            record.task_id
        );
        eprintln!("{detail}");
        record.persistence_error = Some(detail);
        false
    } else {
        true
    }
}
fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn lock_busy(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32)
    }
    #[cfg(not(windows))]
    {
        error.kind() == io::ErrorKind::WouldBlock
    }
}
fn save(root: &ControlledRoot, record: &Record) -> Result<()> {
    let destination = relative(&record.task_id, "json");
    let target = if root.as_path().join(&destination).exists() {
        root.existing_file(&destination)?
    } else {
        root.prepare_new_file(&destination)?
    };
    let temporary_relative = relative(&record.task_id, "tmp");
    // 同一任务租约串行写入；清理由上次进程中断留下的临时文件后再原子替换。
    root.remove_file_if_exists(&temporary_relative, None)?;
    let temporary = root.prepare_new_file(&temporary_relative)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(record)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, &target)?;
    Ok(())
}

struct TaskSignals {
    context: TaskContext<CallToolResult>,
    cancelled: Arc<AtomicBool>,
}
impl OperationSignals for TaskSignals {
    fn notify_activity(
        &self,
        units: Option<(usize, usize)>,
        message: String,
    ) -> std::result::Result<(), TaskSignalError> {
        self.context.report_activity(units, message)
    }
    fn notify_progress(
        &self,
        percent: u8,
        message: String,
    ) -> std::result::Result<(), TaskSignalError> {
        self.context.report_progress(percent, message)
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.context.is_cancelled()
    }
    fn share_cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
}
impl ExecutionCancellation for TaskSignals {
    fn is_cancelled(&self) -> bool {
        self.cancelled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap();
            let path = PathBuf::from(home)
                .join("suzushiro/scratch/azlw-mcp-task-tests")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    fn arguments(id: &str) -> Map<String, Value> {
        json!({"request_id":id,"workbook":"missing.xlsx"})
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn queued_task_returns_immediately_deduplicates_and_can_be_cancelled() {
        let directory = Directory::new();
        runtime().block_on(async {
            let manager = Arc::new(Tasks::new(directory.0.clone()));
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            let guard = gate.clone().lock_owned().await;
            let runtime = tokio::runtime::Handle::current();
            let submitted = manager
                .submit(
                    "acquisition_update".into(),
                    arguments("queued"),
                    gate.clone(),
                    runtime.clone(),
                )
                .unwrap();
            assert_eq!(submitted["state"], "queued");
            let repeated = manager
                .submit(
                    "acquisition_update".into(),
                    arguments("queued"),
                    gate.clone(),
                    runtime.clone(),
                )
                .unwrap();
            assert_eq!(repeated["task_id"], submitted["task_id"]);
            assert_eq!(manager.monitors.lock().unwrap().len(), 1);
            let lookup = json!({"request_id":"queued"}).as_object().unwrap().clone();
            assert_eq!(manager.query(&lookup, false).unwrap()["state"], "queued");
            // 第二个服务进程也只返回现有任务；租约阻止它重新执行或误判中断。
            let other = Arc::new(Tasks::new(directory.0.clone()));
            assert_eq!(
                other
                    .submit(
                        "acquisition_update".into(),
                        arguments("queued"),
                        gate.clone(),
                        runtime.clone()
                    )
                    .unwrap()["task_id"],
                submitted["task_id"]
            );
            assert!(other.monitors.lock().unwrap().is_empty());
            other.query(&lookup, true).unwrap();
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if manager.query(&lookup, false).unwrap()["state"] == "cancelled" {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            drop(guard);
            manager.shutdown().await;
            let result = other.query(&lookup, false).unwrap();
            assert_eq!(result["result"]["status"], "cancelled");
            let mut conflicting = arguments("queued");
            conflicting.insert("workbook".into(), json!("different.xlsx"));
            assert!(
                manager
                    .submit("acquisition_update".into(), conflicting, gate, runtime)
                    .is_err()
            );
        });
    }

    #[test]
    fn completed_failure_survives_manager_restart_without_reexecution() {
        let directory = Directory::new();
        runtime().block_on(async {
            let request_id = "任务".repeat(64);
            let manager = Arc::new(Tasks::new(directory.0.clone()));
            let gate = Arc::new(tokio::sync::Mutex::new(()));
            manager
                .submit(
                    "acquisition_update".into(),
                    arguments(&request_id),
                    gate.clone(),
                    tokio::runtime::Handle::current(),
                )
                .unwrap();
            let lookup = json!({"request_id":request_id})
                .as_object()
                .unwrap()
                .clone();
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if manager.query(&lookup, false).unwrap()["state"] == "failed" {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            manager.shutdown().await;
            let original = manager.query(&lookup, false).unwrap();
            assert_eq!(original["result"]["status"], "failed");
            let reopened = Arc::new(Tasks::new(directory.0.clone()));
            assert_eq!(reopened.query(&lookup, false).unwrap(), original);
            assert_eq!(
                reopened
                    .submit(
                        "acquisition_update".into(),
                        arguments(&request_id),
                        gate,
                        tokio::runtime::Handle::current()
                    )
                    .unwrap(),
                original
            );
            assert!(reopened.monitors.lock().unwrap().is_empty());
        });
    }

    #[test]
    fn abandoned_task_is_interrupted_and_invalid_identifiers_are_rejected() {
        let directory = Directory::new();
        let root = ControlledRoot::open(&directory.0).unwrap();
        root.ensure_directory(Path::new(DIRECTORY)).unwrap();
        let record = Record {
            task_id: sha256_bytes(b"lost"),
            request_id: "lost".into(),
            tool: "acquisition_update".into(),
            arguments: Map::new(),
            state: "running".into(),
            updated_at_ms: now(),
            progress: Value::Null,
            result: None,
            diagnostic: None,
            persistence_error: None,
        };
        save(&root, &record).unwrap();
        let manager = Tasks::new(directory.0.clone());
        let lookup = json!({"request_id":"lost"}).as_object().unwrap().clone();
        assert_eq!(
            manager.query(&lookup, false).unwrap()["state"],
            "interrupted"
        );
        assert_eq!(
            manager.query(&lookup, true).unwrap()["state"],
            "interrupted"
        );
        for invalid in [
            json!({}),
            json!({"task_id":"../settings.json"}),
            json!({"task_id":record.task_id,"request_id":"lost"}),
        ] {
            assert!(manager.query(invalid.as_object().unwrap(), false).is_err());
        }
    }

    #[test]
    fn query_recovers_a_terminal_save_failure_without_losing_business_diagnostics() {
        let directory = Directory::new();
        let root = ControlledRoot::open(&directory.0).unwrap();
        root.ensure_directory(Path::new(DIRECTORY)).unwrap();
        let id = sha256_bytes(b"persistence");
        let mut record = Record {
            task_id: id.clone(),
            request_id: "persistence".into(),
            tool: "acquisition_update".into(),
            arguments: Map::new(),
            state: "running".into(),
            updated_at_ms: now(),
            progress: Value::Null,
            result: None,
            diagnostic: None,
            persistence_error: None,
        };
        save(&root, &record).unwrap();
        record.state = "unknown".into();
        record.diagnostic = Some("原业务异常".into());
        record.result = Some(CallToolResult::structured_error(
            json!({"status":"unknown","error":"原业务异常"}),
        ));
        let blocked_temporary = root.as_path().join(relative(&id, "tmp"));
        fs::create_dir(&blocked_temporary).unwrap();
        assert!(!persist(&root, &mut record));
        assert!(record.persistence_error.is_some());
        fs::remove_dir(&blocked_temporary).unwrap();
        let manager = Tasks::new(directory.0.clone());
        manager.active.lock().unwrap().insert(
            id.clone(),
            Arc::new(Entry {
                record: Mutex::new(record),
                cancelled: Arc::new(AtomicBool::new(false)),
            }),
        );
        let lookup = json!({"task_id":id}).as_object().unwrap().clone();
        let recovered = manager.query(&lookup, false).unwrap();
        assert_eq!(recovered["diagnostic"], "原业务异常");
        assert_eq!(recovered["result"]["status"], "unknown");
        assert!(recovered["persistence_error"].is_null());
        assert!(manager.active.lock().unwrap().is_empty());
        assert_eq!(
            Tasks::new(directory.0.clone())
                .query(&lookup, false)
                .unwrap(),
            recovered
        );
        assert_eq!(
            serde_json::to_value(TaskError::Io(io::Error::other("磁盘错误")).into_mcp()).unwrap()["code"],
            -32603
        );
        assert_eq!(
            serde_json::to_value(TaskError::Invalid("参数错误".into()).into_mcp()).unwrap()["code"],
            -32602
        );
    }
}

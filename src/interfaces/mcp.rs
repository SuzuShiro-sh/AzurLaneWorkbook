//! 标准输入输出 MCP 接口；协议通道独占 stdout，业务沿用应用服务。

mod dispatch;
mod registry;
mod results;
mod tasks;
#[cfg(test)]
mod tests;

use crate::{application::ExecutionCancellation, bootstrap::OperationSignals};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ProgressNotificationParam, ProgressToken, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, service::RequestContext};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use suzushiro_task_runtime::TaskSignalError;

pub fn serve(root: PathBuf) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let tasks = Arc::new(tasks::Tasks::new(root.clone()));
        let server = McpServer {
            root,
            gate: Arc::new(tokio::sync::Mutex::new(())),
            tools: registry::tools(),
            tasks: tasks.clone(),
        };
        let service = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| e.to_string())?;
        let result = service.waiting().await.map_err(|e| e.to_string());
        tasks.shutdown().await;
        result?;
        Ok(())
    })
}

struct McpServer {
    root: PathBuf,
    gate: Arc<tokio::sync::Mutex<()>>,
    tools: Vec<Tool>,
    tasks: Arc<tasks::Tasks>,
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "AzurLaneWorkbook",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(registry::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.and_then(|r| r.cursor).is_some() {
            return Err(ErrorData::invalid_params("此工具目录不使用分页游标", None));
        }
        Ok(ListToolsResult {
            tools: self.tools.clone(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name == request.name)
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    format!(
                        "未知工具 {}；请调用 tools/list 获取本服务的工具名，使用 name 原值",
                        request.name
                    ),
                    None,
                )
            })?;
        let arguments = request.arguments.unwrap_or_default();
        dispatch::validate_arguments(tool, &arguments)
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        if request.name == "result_get" {
            let root = self.root.clone();
            return tokio::task::spawn_blocking(move || {
                results::read(&root, &arguments).map(Into::into)
            })
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }
        if tasks::is_long(&request.name)
            || matches!(request.name.as_ref(), "task_get" | "task_cancel")
        {
            let tasks = self.tasks.clone();
            let gate = self.gate.clone();
            let runtime = tokio::runtime::Handle::current();
            let name = request.name.into_owned();
            let error_tool = name.clone();
            // 提交和查询不等待业务锁，也不受提交请求结束后的取消通知影响。
            let value = tokio::task::spawn_blocking(move || {
                if tasks::is_long(&name) {
                    tasks.submit(name, arguments, gate, runtime)
                } else {
                    tasks.query(&arguments, name == "task_cancel")
                }
            })
            .await
            .map_err(|e| ErrorData::internal_error(format!("MCP 任务管理异常: {e}"), None))?
            .map_err(|error| {
                let mut error = error.into_mcp();
                error.message = format!("工具 {error_tool}：{}", error.message).into();
                error
            })?;
            return Ok(rmcp::model::CallToolResult::structured(value).into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        // 请求结束或连接中断时请求协作式取消；阻塞业务持有锁直到收尾完成。
        let _cancel_on_drop = CancelOnDrop(cancelled.clone());
        let guard = tokio::select! {
            guard = self.gate.clone().lock_owned() => guard,
            _ = context.ct.cancelled() => return Err(ErrorData::invalid_request("排队请求已取消", None)),
        };
        if context.ct.is_cancelled() {
            return Err(ErrorData::invalid_request("请求已取消", None));
        }
        let signals = Signals {
            cancelled: cancelled.clone(),
            peer: context.peer,
            token: context.meta.get_progress_token(),
            sequence: AtomicU64::new(0),
            runtime: tokio::runtime::Handle::current(),
        };
        let root = self.root.clone();
        let name = request.name.into_owned();
        let mut task = tokio::task::spawn_blocking(move || {
            let _guard = guard;
            dispatch::run(&root, &name, arguments, &signals)
        });
        let result = tokio::select! {
            result = &mut task => result,
            _ = context.ct.cancelled() => {
                cancelled.store(true, Ordering::Release);
                task.await
            }
        }
        .map_err(|e| ErrorData::internal_error(format!("MCP 业务任务异常: {e}"), None))?;
        Ok(result.into())
    }
}

struct Signals {
    cancelled: Arc<AtomicBool>,
    peer: rmcp::Peer<RoleServer>,
    token: Option<ProgressToken>,
    sequence: AtomicU64,
    runtime: tokio::runtime::Handle,
}
impl Signals {
    fn notify(&self, message: String) -> Result<(), TaskSignalError> {
        if let Some(token) = &self.token {
            let progress = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
            let notification = ProgressNotificationParam::new(token.clone(), progress as f64)
                .with_message(message);
            // 通知失败不改变已发生的业务结果，原始错误写入 stderr。
            if let Err(error) = self
                .runtime
                .block_on(self.peer.notify_progress(notification))
            {
                eprintln!("MCP 进度通知失败: {error}");
            }
        }
        Ok(())
    }
}
impl OperationSignals for Signals {
    fn notify_activity(
        &self,
        units: Option<(usize, usize)>,
        message: String,
    ) -> Result<(), TaskSignalError> {
        self.notify(match units {
            Some((done, total)) => format!("{message} ({done}/{total})"),
            None => message,
        })
    }
    fn notify_progress(&self, percent: u8, message: String) -> Result<(), TaskSignalError> {
        self.notify(format!("{message} ({percent}%)"))
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    fn share_cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
}
impl ExecutionCancellation for Signals {
    fn is_cancelled(&self) -> bool {
        self.cancelled()
    }
}

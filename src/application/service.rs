//! 提供 CLI 和 GUI 共用的应用服务门面、端口组合及游戏会话收尾边界。

use std::fmt::{Display, Formatter};

use super::execution::ExecutionPort;
use super::{
    AppError, CheckHistoryPort, ExecutionHistoryPort, ExecutionResultsPort, GamePort,
    WorkbookBackupPort, WorkbookGenerationPort, WorkbookPort,
};

#[path = "direct_actions.rs"]
mod direct_actions;
mod execution;
mod layout;
mod planning;
mod synchronization;
mod workbook;
mod workspace;

pub use direct_actions::{
    DirectAction, DirectActionBatch, DirectActionOutcome, DirectActionService,
    DirectEquipmentSource, DirectShipSlot,
};
pub use execution::WorkbookExecuteOutcome;
pub use layout::LayoutCheckReport;
pub use layout::LayoutService;
pub use planning::CheckAndSaveOutcome;
pub use workspace::{WorkspaceService, WorkspaceStartupReport};

/// 服务内部唯一的游戏连接所有者；执行连接同时承担全部只读用例。
enum GameConnection {
    ReadOnly(Box<dyn GamePort>),
    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    Execution(Box<dyn ExecutionPort>),
}

impl GamePort for GameConnection {
    fn prepare_synchronization(&mut self) {
        match self {
            Self::ReadOnly(port) => port.prepare_synchronization(),
            Self::Execution(port) => port.prepare_synchronization(),
        }
    }

    fn read_full_state_with_progress(
        &mut self,
        progress: &mut dyn FnMut(super::OperationProgress),
    ) -> Result<super::GameObservation, AppError> {
        match self {
            Self::ReadOnly(port) => port.read_full_state_with_progress(progress),
            Self::Execution(port) => port.read_full_state_with_progress(progress),
        }
    }

    fn read_state_with_scope(
        &mut self,
        scope: crate::domain::GameReadScope,
        progress: &mut dyn FnMut(super::OperationProgress),
    ) -> Result<super::GameObservation, AppError> {
        match self {
            Self::ReadOnly(port) => port.read_state_with_scope(scope, progress),
            Self::Execution(port) => port.read_state_with_scope(scope, progress),
        }
    }

    fn read_full_state(&mut self) -> Result<super::GameObservation, AppError> {
        match self {
            Self::ReadOnly(port) => port.read_full_state(),
            Self::Execution(port) => port.read_full_state(),
        }
    }

    fn shutdown_session(&mut self) -> Result<(), AppError> {
        match self {
            Self::ReadOnly(port) => port.shutdown_session(),
            Self::Execution(port) => port.shutdown_session(),
        }
    }
}

#[derive(Clone, Copy)]
enum GameOperationAccess {
    ReadOnly,
    /// 表示写入工作流的权限和风险范围；命令是否已发送仍以执行报告与回执为准。
    Write,
}

impl GameOperationAccess {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Write => "write",
        }
    }
}

impl GameConnection {
    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    fn execution_mut(&mut self) -> Option<&mut dyn ExecutionPort> {
        match self {
            Self::Execution(port) => Some(port.as_mut()),
            Self::ReadOnly(_) => None,
        }
    }
}

/// 一次用例持有的游戏连接。没有连接时由用例直接返回未就绪错误。
struct GameSession {
    game: Option<GameConnection>,
}

impl GameSession {
    fn read_only(game: Option<Box<dyn GamePort>>) -> Self {
        Self {
            game: game.map(GameConnection::ReadOnly),
        }
    }

    fn execution(execution: Option<Box<dyn ExecutionPort>>) -> Self {
        Self {
            game: execution.map(GameConnection::Execution),
        }
    }

    /// 返回这次操作已经组合的游戏读取连接。
    fn game_mut(&mut self) -> Option<&mut dyn GamePort> {
        self.game
            .as_mut()
            .map(|connection| connection as &mut dyn GamePort)
    }

    /// 返回这次操作已经组合的执行连接。只读连接不能在这里升级。
    fn execution_mut(&mut self) -> Option<&mut dyn ExecutionPort> {
        self.game.as_mut().and_then(GameConnection::execution_mut)
    }

    /// 关闭当前会话并保留业务结果与清理结果，不在这里解释具体用例。
    fn finish_operation<T>(
        &mut self,
        access: GameOperationAccess,
        operation: Result<T, AppError>,
    ) -> GameOperationFinish<T> {
        let operation = operation.map_err(|error| error.with_context("access", access.as_str()));
        let cleanup: Result<(), AppError> = self
            .game
            .as_mut()
            .map_or(Ok(()), GamePort::shutdown_session)
            .map_err(|error| error.with_context("access", access.as_str()));
        match (operation, cleanup) {
            (Ok(value), Ok(())) => GameOperationFinish::Ready(value),
            (Ok(value), Err(cleanup)) => GameOperationFinish::ValueWithCleanup { value, cleanup },
            (Err(operation), Ok(())) => {
                GameOperationFinish::Failed(mark_recovered_game_cleanup(operation))
            }
            (Err(operation), Err(cleanup)) => {
                GameOperationFinish::Failed(game_operation_and_cleanup_error(operation, cleanup))
            }
        }
    }
}

/// 读取游戏状态并生成数据工作簿。不持有执行备份、历史或结果写回。
pub struct WorkbookSyncService {
    pub(super) workbook: Box<dyn WorkbookPort>,
    session: GameSession,
    pub(super) generation: Box<dyn WorkbookGenerationPort>,
}

impl WorkbookSyncService {
    pub(crate) fn new(
        workbook: Box<dyn WorkbookPort>,
        game: Option<Box<dyn GamePort>>,
        generation: Box<dyn WorkbookGenerationPort>,
    ) -> Self {
        Self {
            workbook,
            session: GameSession::read_only(game),
            generation,
        }
    }

    /// 用同时具备读取能力的执行连接承担本次生成。
    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    pub(crate) fn with_execution_port(mut self, execution: Box<dyn ExecutionPort>) -> Self {
        self.session = GameSession::execution(Some(execution));
        self
    }

    #[cfg(test)]
    pub(crate) fn ship_acquisition_enabled_for_test(&self) -> Result<bool, String> {
        self.generation.ship_acquisition_enabled_for_test()
    }
}

/// 检查工作簿计划并保存历史和检查结果。不持有执行备份或执行历史。
pub struct WorkbookCheckService {
    pub(super) workbook: Box<dyn WorkbookPort>,
    session: GameSession,
    pub(super) check_history: Box<dyn CheckHistoryPort>,
    pub(super) execution_results: Box<dyn ExecutionResultsPort>,
}

impl WorkbookCheckService {
    pub(crate) fn new(
        workbook: Box<dyn WorkbookPort>,
        game: Option<Box<dyn GamePort>>,
        check_history: Box<dyn CheckHistoryPort>,
        execution_results: Box<dyn ExecutionResultsPort>,
    ) -> Self {
        Self {
            workbook,
            session: GameSession::read_only(game),
            check_history,
            execution_results,
        }
    }

    #[cfg_attr(not(any(target_os = "windows", test)), allow(dead_code))]
    pub(crate) fn with_execution_port(mut self, execution: Box<dyn ExecutionPort>) -> Self {
        self.session = GameSession::execution(Some(execution));
        self
    }
}

/// 备份、执行并保存执行历史和结果。不持有工作簿生成器。
pub struct WorkbookExecuteService {
    pub(super) workbook: Box<dyn WorkbookPort>,
    session: GameSession,
    pub(super) workbook_backup: Box<dyn WorkbookBackupPort>,
    pub(super) execution_history: Box<dyn ExecutionHistoryPort>,
    pub(super) execution_results: Box<dyn ExecutionResultsPort>,
}

impl WorkbookExecuteService {
    pub(crate) fn new(
        workbook: Box<dyn WorkbookPort>,
        execution: Option<Box<dyn ExecutionPort>>,
        workbook_backup: Box<dyn WorkbookBackupPort>,
        execution_history: Box<dyn ExecutionHistoryPort>,
        execution_results: Box<dyn ExecutionResultsPort>,
    ) -> Self {
        Self {
            workbook,
            session: GameSession::execution(execution),
            workbook_backup,
            execution_history,
            execution_results,
        }
    }
}

/// 一次业务操作和它的会话关闭结果。
enum GameOperationFinish<T> {
    Ready(T),
    ValueWithCleanup { value: T, cleanup: AppError },
    Failed(AppError),
}

/// 同时保留最先发生的业务错误和随后发生的游戏会话清理错误。
#[derive(Debug)]
struct GameOperationAndCleanupFailure {
    operation: AppError,
    cleanup: AppError,
}

impl Display for GameOperationAndCleanupFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "应用操作失败: {}; 游戏会话清理同时失败: {}",
            self.operation, self.cleanup
        )?;
        let mut source = std::error::Error::source(&self.cleanup);
        while let Some(cause) = source {
            write!(formatter, "；清理原因：{cause}")?;
            source = cause.source();
        }
        Ok(())
    }
}

impl std::error::Error for GameOperationAndCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.operation)
    }
}

fn game_operation_and_cleanup_error(operation: AppError, cleanup: AppError) -> AppError {
    let stage = operation.stage();
    let code = operation.code();
    let message = operation.message().to_owned();
    let mut context: Vec<(String, String)> = operation
        .context()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    context.push(("cleanup".to_owned(), "failed".to_owned()));
    context.push(("game_cleanup_stage".to_owned(), cleanup.stage().to_owned()));
    context.push((
        "game_cleanup_code".to_owned(),
        cleanup.code().as_str().to_owned(),
    ));
    for (key, value) in cleanup.context() {
        context.push((format!("game_cleanup_{key}"), value.clone()));
    }
    let source = GameOperationAndCleanupFailure { operation, cleanup };
    let mut combined = AppError::from_source(stage, code, message, source);
    for (key, value) in context {
        combined = combined.with_context(key, value);
    }
    combined
}

/// 首次清理失败但服务边界重试成功时，保留事件证据并发布已恢复的最终状态。
fn mark_recovered_game_cleanup(error: AppError) -> AppError {
    let owner_retained = matches!(
        error.cleanup_fact(),
        Some(super::CleanupFact::Failed {
            owner_retained: true
        })
    );
    if owner_retained {
        let initial_stage = error.context().get("cleanup_stage").cloned();
        let initial_code = error.context().get("cleanup_code").cloned();
        let mut recovered = error
            .with_cleanup_fact(super::CleanupFact::Recovered)
            .with_context("cleanup_initial_attempt", "failed")
            .with_context("cleanup_initial_owner_retained", "true");
        if let Some(stage) = initial_stage {
            recovered = recovered.with_context("cleanup_initial_stage", stage);
        }
        if let Some(code) = initial_code {
            recovered = recovered.with_context("cleanup_initial_code", code);
        }
        return recovered;
    }
    error
}

/// 生产组合没有提供游戏会话时的稳定错误来源。
#[derive(Debug, thiserror::Error)]
#[error("应用服务未配置 game 端口")]
struct GamePortNotConfigured {
    stage: &'static str,
    message: &'static str,
}

/// 构造供 CLI 和 GUI 共同识别的缺失游戏端口诊断。
pub(crate) fn missing_game_port(stage: &'static str, message: &'static str) -> AppError {
    AppError::from_source(
        stage,
        super::AppErrorCode::GameNotReady,
        message,
        GamePortNotConfigured { stage, message },
    )
    .with_context("missing_port", "game")
}

#[cfg(test)]
mod tests;

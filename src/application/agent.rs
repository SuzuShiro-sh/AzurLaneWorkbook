//! 显式管理设备内代理。状态查询不会被改成注入。

use super::AppError;

/// 调用方明确选择的代理操作。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentManagementAction {
    Status,
    Inject,
    Unload,
}

/// 一次代理操作的稳定结果。字段名与命令输出一致。
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct AgentManagementReport {
    pub status: String,
    pub available: bool,
    pub pid: Option<u32>,
    pub instance: Option<String>,
    pub message: String,
    pub agent_version: Option<String>,
    pub catalog_generation: Option<u64>,
}

impl AgentManagementReport {
    /// 注入和卸载遇到忙或不可用时需要调用方停下来核对。状态查询只返回结果。
    pub fn needs_attention(&self, action: AgentManagementAction) -> bool {
        !matches!(action, AgentManagementAction::Status)
            && matches!(self.status.as_str(), "busy" | "unavailable")
    }
}

/// 代理操作完成后的应用结论。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AgentManagementOutcome {
    Ready(AgentManagementReport),
    NeedsAttention(AgentManagementReport),
}

impl AgentManagementOutcome {
    pub fn from_report(action: AgentManagementAction, report: AgentManagementReport) -> Self {
        if report.needs_attention(action) {
            Self::NeedsAttention(report)
        } else {
            Self::Ready(report)
        }
    }

    pub fn report(&self) -> &AgentManagementReport {
        match self {
            Self::Ready(report) | Self::NeedsAttention(report) => report,
        }
    }
}

/// 设备适配器执行一次已经选定的代理操作。
pub(crate) trait AgentManagementPort {
    fn manage_agent(
        &self,
        instance: Option<&str>,
        action: AgentManagementAction,
    ) -> Result<AgentManagementReport, AppError>;
}

/// 代理管理用例。调用方只看到操作结论，不选择设备探测实现。
pub struct AgentManagementService {
    port: Box<dyn AgentManagementPort>,
}

impl AgentManagementService {
    pub(crate) fn new(port: Box<dyn AgentManagementPort>) -> Self {
        Self { port }
    }

    pub fn manage(
        &self,
        instance: Option<&str>,
        action: AgentManagementAction,
    ) -> Result<AgentManagementOutcome, AppError> {
        let report = self.port.manage_agent(instance, action)?;
        Ok(AgentManagementOutcome::from_report(action, report))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentManagementAction, AgentManagementOutcome, AgentManagementPort, AgentManagementReport,
        AgentManagementService,
    };
    use crate::application::{AppError, AppErrorCode};

    struct ScriptedAgent {
        report: AgentManagementReport,
    }

    impl AgentManagementPort for ScriptedAgent {
        fn manage_agent(
            &self,
            _instance: Option<&str>,
            _action: AgentManagementAction,
        ) -> Result<AgentManagementReport, AppError> {
            Ok(self.report.clone())
        }
    }

    fn report(status: &str) -> AgentManagementReport {
        AgentManagementReport {
            status: status.to_owned(),
            available: status == "ready",
            pid: None,
            instance: Some("0".to_owned()),
            message: format!("代理{status}"),
            agent_version: None,
            catalog_generation: None,
        }
    }

    #[test]
    fn status_query_stays_ready_when_the_agent_is_busy() {
        let service = AgentManagementService::new(Box::new(ScriptedAgent {
            report: report("busy"),
        }));
        let outcome = service
            .manage(Some("0"), AgentManagementAction::Status)
            .expect("状态查询应返回报告");
        assert!(matches!(outcome, AgentManagementOutcome::Ready(_)));
        assert_eq!(outcome.report().status, "busy");
    }

    #[test]
    fn inject_and_unload_stop_when_the_agent_is_not_ready() {
        for action in [AgentManagementAction::Inject, AgentManagementAction::Unload] {
            for status in ["busy", "unavailable"] {
                let service = AgentManagementService::new(Box::new(ScriptedAgent {
                    report: report(status),
                }));
                let outcome = service.manage(Some("0"), action).expect("操作应返回报告");
                assert!(
                    matches!(outcome, AgentManagementOutcome::NeedsAttention(_)),
                    "{action:?} {status}"
                );
            }
        }
        let service = AgentManagementService::new(Box::new(ScriptedAgent {
            report: report("ready"),
        }));
        let outcome = service
            .manage(None, AgentManagementAction::Inject)
            .expect("就绪注入应完成");
        assert!(matches!(outcome, AgentManagementOutcome::Ready(_)));
    }

    struct FailingAgent;

    impl AgentManagementPort for FailingAgent {
        fn manage_agent(
            &self,
            _instance: Option<&str>,
            _action: AgentManagementAction,
        ) -> Result<AgentManagementReport, AppError> {
            Err(AppError::from_source(
                "agent.manage",
                AppErrorCode::CapabilityMissing,
                "未配置代理管理",
                std::io::Error::other("missing"),
            ))
        }
    }

    #[test]
    fn missing_agent_port_is_returned_without_a_report() {
        let service = AgentManagementService::new(Box::new(FailingAgent));
        let error = service
            .manage(None, AgentManagementAction::Unload)
            .expect_err("缺少端口应失败");
        assert_eq!(error.code(), AppErrorCode::CapabilityMissing);
    }
}

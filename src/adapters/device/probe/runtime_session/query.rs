//! 在认证连接上查询对象，不建立工作簿或完整领域状态。

use serde_json::json;

use super::{RuntimeProbeError, RuntimeSession, RuntimeSessionState};
mod data;

use crate::application::{GameQuery, GameQueryReport};

fn query_error(error: impl std::fmt::Display) -> RuntimeProbeError {
    RuntimeProbeError::InvalidOutput {
        stage: "session.query",
        message: error.to_string(),
    }
}

impl RuntimeSession {
    pub(super) fn refresh_catalog_generation(&mut self) -> Result<(), RuntimeProbeError> {
        let RuntimeSessionState::Active(connection) = &mut self.state else {
            return Err(query_error("读取会话已关闭"));
        };
        let health = super::wait_for_main_thread(&mut connection.client)?;
        if health.catalog_generation != self.catalog_generation {
            self.catalog_generation = health.catalog_generation;
            self.ship_catalog = None;
            self.equipment_catalog = None;
            self.readiness_confirmed = false;
            self.last_capabilities = None;
            self.last_full_state_capture = None;
            connection
                .client
                .enable_catalog_cache(self.resources.tool_root.clone(), health.catalog_generation);
        }
        Ok(())
    }
    pub(crate) fn query(
        &mut self,
        query: &GameQuery,
    ) -> Result<GameQueryReport, RuntimeProbeError> {
        self.refresh_catalog_generation()?;
        self.resources.journal.record("session.query.start", "ok", json!({"kind": query.kind(), "requested_count": query.ids().len(), "fields": query.fields()}))?;
        let result = self.query_objects(query).and_then(|report| {
            let RuntimeSessionState::Active(connection) = &mut self.state else {
                return Err(query_error("读取会话已关闭"));
            };
            let health = connection
                .client
                .health(self.resources.options.timeout_ms)?;
            if health.catalog_generation != self.catalog_generation {
                return Err(query_error("查询期间 Lua 运行态发生变化，需要重新读取"));
            }
            Ok(report)
        });
        match &result {
            Ok(report) => self.resources.journal.record(
                "session.query.complete",
                "ok",
                json!({"count": report.entries.len(), "missing_count": report.missing_ids.len()}),
            )?,
            Err(error) => {
                if let Err(log_error) = self.resources.journal.record(
                    "session.query.failure",
                    "error",
                    json!({"message": error.to_string()}),
                ) {
                    eprintln!("记录对象查询失败信息失败: {log_error}");
                }
            }
        }
        result
    }
}

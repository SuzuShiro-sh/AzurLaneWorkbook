//! 定义 GUI 只读展示模拟器实例候选时使用的稳定脱敏模型。

use serde::Serialize;
use suzushiro_target_core::{TargetId, TargetIdError, TargetState};

///模拟器实例目录报告版本。
pub const EMULATOR_INSTANCE_CATALOG_SCHEMA_VERSION: u32 = 2;

/// 由管理器只读状态归一化得到的实例状态。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmulatorInstanceState {
    /// 管理器进程与 Android 均未进入运行状态。
    Stopped,
    /// 实例进程存在，但 Android 或管理器状态尚未完整就绪。
    Starting,
    /// 管理器确认实例、Android 与 ADB 信息全部就绪。
    Ready,
    /// 管理器记录存在，但目标版本或 ADB 端点不满足运行时边界。
    Unavailable,
}

impl EmulatorInstanceState {
    /// 返回面向普通用户的稳定状态文本。
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Stopped => "已停止",
            Self::Starting => "启动中",
            Self::Ready => "已启动",
            Self::Unavailable => "不可用",
        }
    }
}

impl From<EmulatorInstanceState> for TargetState {
    fn from(state: EmulatorInstanceState) -> Self {
        match state {
            EmulatorInstanceState::Stopped => Self::Stopped,
            EmulatorInstanceState::Starting => Self::Starting,
            EmulatorInstanceState::Ready => Self::Ready,
            EmulatorInstanceState::Unavailable => Self::Unavailable,
        }
    }
}

/// 一条不包含 ADB 地址、端口、PID 或管理器路径的实例候选。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EmulatorInstanceCandidate {
    instance_id: TargetId,
    display_name: String,
    state: TargetState,
    android_version: String,
    selected: bool,
}

impl EmulatorInstanceCandidate {
    /// 从已经通过管理器输出校验的字段建立候选。
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn new(
        instance_id: String,
        display_name: String,
        state: EmulatorInstanceState,
        android_version: String,
        selected: bool,
    ) -> Result<Self, TargetIdError> {
        Ok(Self {
            instance_id: TargetId::new(instance_id)?,
            display_name,
            state: state.into(),
            android_version,
            selected,
        })
    }

    /// 返回带提供方命名空间的实例标识。
    pub fn instance_id(&self) -> &str {
        self.instance_id.as_str()
    }

    /// 返回管理器提供的实例名称。
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// 返回归一化运行状态。
    pub const fn state(&self) -> EmulatorInstanceState {
        match self.state {
            TargetState::Stopped => EmulatorInstanceState::Stopped,
            TargetState::Starting => EmulatorInstanceState::Starting,
            TargetState::Ready => EmulatorInstanceState::Ready,
            TargetState::Unavailable => EmulatorInstanceState::Unavailable,
        }
    }

    /// 返回管理器报告的 Android 版本。
    pub fn android_version(&self) -> &str {
        &self.android_version
    }

    /// 返回该候选是否与当前 settings.json 提示一致。
    pub const fn selected(&self) -> bool {
        self.selected
    }
}

/// 一次无启动副作用的模拟器实例目录读取结果。
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct EmulatorInstanceCatalogReport {
    message: &'static str,
    schema_version: u32,
    selected_instance: Option<String>,
    candidates: Vec<EmulatorInstanceCandidate>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
}

impl EmulatorInstanceCatalogReport {
    /// 从按实例索引稳定排序的候选建立报告。
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub(crate) fn new(
        selected_instance: Option<String>,
        candidates: Vec<EmulatorInstanceCandidate>,
    ) -> Self {
        Self {
            message: "模拟器 实例目录读取完成",
            schema_version: EMULATOR_INSTANCE_CATALOG_SCHEMA_VERSION,
            selected_instance,
            candidates,
            warnings: Vec::new(),
        }
    }

    /// 保留部分安装查询失败或降级提示，可用候选仍供选择。
    pub(crate) fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }

    /// 返回本次刷新尚未完全验证的安装信息。
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// 返回当前配置的实例提示；空值表示自动选择。
    pub fn selected_instance(&self) -> Option<&str> {
        self.selected_instance.as_deref()
    }

    /// 返回按索引稳定排序的实例候选。
    pub fn candidates(&self) -> &[EmulatorInstanceCandidate] {
        &self.candidates
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EmulatorInstanceCandidate, EmulatorInstanceCatalogReport, EmulatorInstanceState};

    #[test]
    fn partial_discovery_warnings_preserve_candidates_and_serialize_additively() {
        let report = EmulatorInstanceCatalogReport::new(None, vec![])
            .with_warnings(vec!["MuMu 查询超时".into()]);
        assert_eq!(report.warnings(), ["MuMu 查询超时"]);
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["warnings"], json!(["MuMu 查询超时"]));
        assert!(value["candidates"].is_array());
        assert!(
            serde_json::to_value(EmulatorInstanceCatalogReport::new(None, vec![]))
                .unwrap()
                .get("warnings")
                .is_none()
        );
    }

    #[test]
    fn serializes_only_user_facing_instance_fields() {
        let report = EmulatorInstanceCatalogReport::new(
            Some("0".to_owned()),
            vec![
                EmulatorInstanceCandidate::new(
                    "0".to_owned(),
                    "碧蓝航线".to_owned(),
                    EmulatorInstanceState::Ready,
                    "12.0".to_owned(),
                    true,
                )
                .unwrap(),
            ],
        );

        assert_eq!(
            serde_json::to_value(report).unwrap(),
            json!({
                "message": "模拟器 实例目录读取完成",
                "schema_version": 2,
                "selected_instance": "0",
                "candidates": [{
                    "instance_id": "0",
                    "display_name": "碧蓝航线",
                    "state": "ready",
                    "android_version": "12.0",
                    "selected": true,
                }],
            })
        );
    }

    #[test]
    fn state_wire_values_remain_stable() {
        assert_eq!(
            serde_json::to_value(EmulatorInstanceState::Stopped).unwrap(),
            "stopped"
        );
        assert_eq!(
            serde_json::to_value(EmulatorInstanceState::Starting).unwrap(),
            "starting"
        );
        assert_eq!(
            serde_json::to_value(EmulatorInstanceState::Ready).unwrap(),
            "ready"
        );
        assert_eq!(
            serde_json::to_value(EmulatorInstanceState::Unavailable).unwrap(),
            "unavailable"
        );
    }

    #[test]
    fn stopped_candidates_remain_visible() {
        let report = EmulatorInstanceCatalogReport::new(
            None,
            vec![
                EmulatorInstanceCandidate::new(
                    "3".to_owned(),
                    "已停止实例".to_owned(),
                    EmulatorInstanceState::Stopped,
                    "12.0".to_owned(),
                    false,
                )
                .unwrap(),
            ],
        );

        assert_eq!(report.candidates().len(), 1);
    }

    #[test]
    fn starting_candidates_preserve_their_state() {
        let report = EmulatorInstanceCatalogReport::new(
            None,
            vec![
                EmulatorInstanceCandidate::new(
                    "4".to_owned(),
                    "启动中实例".to_owned(),
                    EmulatorInstanceState::Starting,
                    "12.0".to_owned(),
                    false,
                )
                .unwrap(),
            ],
        );

        assert_eq!(
            report.candidates()[0].state(),
            EmulatorInstanceState::Starting
        );
    }

    #[test]
    fn unavailable_candidates_remain_visible() {
        let report = EmulatorInstanceCatalogReport::new(
            Some("5".to_owned()),
            vec![
                EmulatorInstanceCandidate::new(
                    "5".to_owned(),
                    "不兼容实例".to_owned(),
                    EmulatorInstanceState::Unavailable,
                    "11.0".to_owned(),
                    true,
                )
                .unwrap(),
            ],
        );

        assert_eq!(report.candidates()[0].state().display_name(), "不可用");
    }
}

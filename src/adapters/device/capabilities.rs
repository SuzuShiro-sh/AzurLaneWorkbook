//! 复核完整状态读取实际使用的运行态能力集合。

use thiserror::Error;

use super::runtime::CapabilitiesResult;
#[cfg(test)]
use super::runtime::CapabilityStatus;

const REQUIRED_FULL_STATE_READ_CAPABILITIES: [&str; 6] = [
    "read.bag",
    "read.owned_state",
    "read.ship_details",
    "read.equipment_configs",
    "read.compose_recipes",
    "read.equipment_reference_names",
];

/// 确认刚执行的完整读取与 agent 报告的可用能力一致。
pub(crate) fn validate_full_state_read_capabilities(
    capabilities: &CapabilitiesResult,
    weapon_count: usize,
    skill_count: usize,
) -> Result<(), FullStateCapabilityError> {
    for name in REQUIRED_FULL_STATE_READ_CAPABILITIES {
        require_ready(capabilities, name)?;
    }
    if weapon_count > 0 {
        require_ready(capabilities, "read.equipment_weapons")?;
    }
    if skill_count > 0 {
        require_ready(capabilities, "read.skill_effects")?;
    }
    Ok(())
}

fn require_ready(
    capabilities: &CapabilitiesResult,
    name: &'static str,
) -> Result<(), FullStateCapabilityError> {
    let status = capabilities
        .capabilities
        .get(name)
        .ok_or(FullStateCapabilityError::Missing { capability: name })?;
    if !status.available || status.reason_code != "ready" {
        return Err(FullStateCapabilityError::NotReady {
            capability: name,
            available: status.available,
            reason_code: status.reason_code.clone(),
        });
    }
    Ok(())
}

/// 完整读取使用的能力缺失或未处于 ready 状态。
#[derive(Debug, Error)]
pub(crate) enum FullStateCapabilityError {
    /// 能力响应缺少完整读取必需的稳定键。
    #[error("capabilities 缺少完整读取必需能力 {capability}")]
    Missing { capability: &'static str },
    /// 能力键存在，但当前运行态没有声明可用且就绪。
    #[error("完整读取能力 {capability} 未就绪: available={available}, reason_code={reason_code}")]
    NotReady {
        capability: &'static str,
        available: bool,
        reason_code: String,
    },
}

impl FullStateCapabilityError {
    /// 返回失败的稳定能力键。
    pub(crate) const fn capability(&self) -> &'static str {
        match self {
            Self::Missing { capability } | Self::NotReady { capability, .. } => capability,
        }
    }

    /// 返回能力可用状态；缺少能力键时没有该状态。
    pub(crate) const fn available(&self) -> Option<bool> {
        match self {
            Self::Missing { .. } => None,
            Self::NotReady { available, .. } => Some(*available),
        }
    }

    /// 返回 agent 报告的原因码；缺少能力键时没有该字段。
    pub(crate) fn reason_code(&self) -> Option<&str> {
        match self {
            Self::Missing { .. } => None,
            Self::NotReady { reason_code, .. } => Some(reason_code),
        }
    }
}

#[cfg(test)]
pub(crate) fn ready_capabilities() -> CapabilitiesResult {
    use std::collections::BTreeMap;

    let read_capabilities = [
        "read.bag",
        "read.owned_state",
        "read.ship_details",
        "read.equipment_configs",
        "read.compose_recipes",
        "read.equipment_reference_names",
        "read.equipment_weapons",
        "read.skill_effects",
    ];
    let mut capabilities = ["runtime.health", "runtime.main_thread_queue"]
        .into_iter()
        .chain(read_capabilities)
        .map(|name| {
            (
                name.to_owned(),
                CapabilityStatus {
                    available: true,
                    reason_code: "ready".to_owned(),
                    evidence: vec!["fixture".to_owned()],
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    capabilities.extend(
        [
            "write.equip",
            "write.unequip",
            "write.compose",
            "write.enhance",
            "write.destroy",
        ]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                CapabilityStatus {
                    available: false,
                    reason_code: "mvp_read_only".to_owned(),
                    evidence: vec!["fixture".to_owned()],
                },
            )
        }),
    );
    CapabilitiesResult { capabilities }
}

#[cfg(test)]
mod tests {
    use super::{ready_capabilities, validate_full_state_read_capabilities};

    #[test]
    fn requires_detail_capabilities_only_when_details_were_read() {
        let mut capabilities = ready_capabilities();
        capabilities.capabilities.remove("read.equipment_weapons");
        capabilities.capabilities.remove("read.skill_effects");

        validate_full_state_read_capabilities(&capabilities, 0, 0).unwrap();
        assert!(validate_full_state_read_capabilities(&capabilities, 1, 0).is_err());
        assert!(validate_full_state_read_capabilities(&capabilities, 0, 1).is_err());
    }

    #[test]
    fn rejects_required_capability_that_is_not_ready() {
        let mut capabilities = ready_capabilities();
        let status = capabilities
            .capabilities
            .get_mut("read.owned_state")
            .unwrap();
        status.available = false;
        status.reason_code = "owned_state_not_ready".to_owned();

        let error = validate_full_state_read_capabilities(&capabilities, 1, 1).unwrap_err();

        assert_eq!(error.capability(), "read.owned_state");
        assert_eq!(error.available(), Some(false));
        assert_eq!(error.reason_code(), Some("owned_state_not_ready"));
    }
}

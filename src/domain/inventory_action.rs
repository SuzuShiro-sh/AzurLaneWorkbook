//! 表示装备库存行上的处理意图及其输入不变量。

use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

use thiserror::Error;

use super::{EnhanceLevel, EquipmentSourceRef};

/// 一条装备库存来源允许选择的处理方式。
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum EquipmentInventoryActionKind {
    /// 保留来源；同时填写目标等级和数量时表示请求后续强化。
    Keep,
    /// 拆解来源中的指定数量。
    Dismantle,
}

impl EquipmentInventoryActionKind {
    /// 返回工作簿和计划摘要使用的稳定值。
    pub const fn stable_key(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Dismantle => "dismantle",
        }
    }
}

impl Display for EquipmentInventoryActionKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.stable_key())
    }
}

/// 一条装备库存来源的结构化处理要求。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EquipmentInventoryAction {
    source: EquipmentSourceRef,
    kind: EquipmentInventoryActionKind,
    dismantle_quantity: Option<u64>,
    target_enhance_level: Option<EnhanceLevel>,
    enhance_quantity: Option<u64>,
}

impl EquipmentInventoryAction {
    /// 建立处理要求，并拒绝互相矛盾的编辑字段。
    pub fn new(
        source: EquipmentSourceRef,
        kind: EquipmentInventoryActionKind,
        dismantle_quantity: Option<u64>,
        target_enhance_level: Option<EnhanceLevel>,
        enhance_quantity: Option<u64>,
    ) -> Result<Self, EquipmentInventoryActionError> {
        match kind {
            EquipmentInventoryActionKind::Keep => {
                if dismantle_quantity.is_some() {
                    return Err(EquipmentInventoryActionError::DismantleQuantityNotAllowed);
                }
                match (target_enhance_level, enhance_quantity) {
                    (Some(_), None) => {
                        return Err(EquipmentInventoryActionError::EnhanceQuantityRequired);
                    }
                    (None, Some(_)) => {
                        return Err(EquipmentInventoryActionError::TargetEnhanceLevelRequired);
                    }
                    (Some(_), Some(0)) => {
                        return Err(EquipmentInventoryActionError::EnhanceQuantityMustBePositive);
                    }
                    (None, None) | (Some(_), Some(_)) => {}
                }
            }
            EquipmentInventoryActionKind::Dismantle => {
                let Some(quantity) = dismantle_quantity else {
                    return Err(EquipmentInventoryActionError::DismantleQuantityRequired);
                };
                if quantity == 0 {
                    return Err(EquipmentInventoryActionError::DismantleQuantityMustBePositive);
                }
                if target_enhance_level.is_some() {
                    return Err(EquipmentInventoryActionError::TargetEnhanceNotAllowed);
                }
                if enhance_quantity.is_some() {
                    return Err(EquipmentInventoryActionError::EnhanceQuantityNotAllowed);
                }
            }
        }
        Ok(Self {
            source,
            kind,
            dismantle_quantity,
            target_enhance_level,
            enhance_quantity,
        })
    }

    /// 返回处理来源。
    pub const fn source(self) -> EquipmentSourceRef {
        self.source
    }

    /// 返回处理方式。
    pub const fn kind(self) -> EquipmentInventoryActionKind {
        self.kind
    }

    /// 返回拆解数量；保留或强化意图为空。
    pub const fn dismantle_quantity(self) -> Option<u64> {
        self.dismantle_quantity
    }

    /// 返回目标强化等级；拆解意图为空。
    pub const fn target_enhance_level(self) -> Option<EnhanceLevel> {
        self.target_enhance_level
    }

    /// 返回需要强化的来源装备数量；未请求强化时为空。
    pub const fn enhance_quantity(self) -> Option<u64> {
        self.enhance_quantity
    }

    /// 判断该行是否没有任何实际处理要求。
    pub const fn is_noop(self) -> bool {
        matches!(
            (
                self.kind,
                self.dismantle_quantity,
                self.target_enhance_level,
                self.enhance_quantity
            ),
            (EquipmentInventoryActionKind::Keep, None, None, None)
        )
    }
}

/// 已通过来源唯一性校验的装备库存处理列表。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EquipmentInventoryPlan {
    actions: Vec<EquipmentInventoryAction>,
}

impl EquipmentInventoryPlan {
    /// 校验同一仓库配置或舰船槽位不会被重复处理，并建立稳定顺序。
    pub fn new(
        mut actions: Vec<EquipmentInventoryAction>,
    ) -> Result<Self, EquipmentInventoryActionError> {
        let mut seen_sources = BTreeSet::new();
        for action in &actions {
            if !seen_sources.insert(action.source()) {
                return Err(EquipmentInventoryActionError::DuplicateSource {
                    source_ref: action.source(),
                });
            }
        }
        actions.sort_by_key(|action| action.source());
        Ok(Self { actions })
    }

    /// 返回按来源稳定排序的处理要求。
    pub fn actions(&self) -> &[EquipmentInventoryAction] {
        &self.actions
    }

    /// 返回是否没有需要处理的库存来源。
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// 返回处理要求数量。
    pub fn len(&self) -> usize {
        self.actions.len()
    }
}

/// 装备库存处理输入违反了结构性不变量。
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum EquipmentInventoryActionError {
    /// 保留或强化输入不能同时带拆解数量。
    #[error("保留装备不能填写拆解数量")]
    DismantleQuantityNotAllowed,
    /// 拆解输入必须明确数量。
    #[error("拆解装备必须填写拆解数量")]
    DismantleQuantityRequired,
    /// 拆解数量不能为零。
    #[error("拆解数量必须大于 0")]
    DismantleQuantityMustBePositive,
    /// 拆解输入不能同时要求强化。
    #[error("拆解装备不能填写目标强化等级")]
    TargetEnhanceNotAllowed,
    /// 强化输入必须明确数量。
    #[error("填写目标强化等级时必须填写强化数量")]
    EnhanceQuantityRequired,
    /// 强化数量不能脱离目标等级单独填写。
    #[error("填写强化数量时必须填写目标强化等级")]
    TargetEnhanceLevelRequired,
    /// 强化数量不能为零。
    #[error("强化数量必须大于 0")]
    EnhanceQuantityMustBePositive,
    /// 拆解输入不能同时要求强化数量。
    #[error("拆解装备不能填写强化数量")]
    EnhanceQuantityNotAllowed,
    /// 同一来源不能出现多个处理动作。
    #[error("装备来源 {source_ref} 出现重复处理要求")]
    DuplicateSource { source_ref: EquipmentSourceRef },
}

#[cfg(test)]
mod tests {
    use super::{
        EquipmentInventoryAction, EquipmentInventoryActionError, EquipmentInventoryActionKind,
        EquipmentInventoryPlan,
    };
    use crate::domain::{EnhanceLevel, EquipmentConfigId, EquipmentSourceRef};

    fn warehouse_source(value: u64) -> EquipmentSourceRef {
        EquipmentSourceRef::Warehouse(EquipmentConfigId::new(value).unwrap())
    }

    #[test]
    fn keeps_require_a_positive_quantity_with_the_target_level() {
        let action = EquipmentInventoryAction::new(
            warehouse_source(1000),
            EquipmentInventoryActionKind::Keep,
            None,
            Some(EnhanceLevel::new(3)),
            Some(2),
        )
        .unwrap();
        assert_eq!(action.target_enhance_level().unwrap().get(), 3);
        assert_eq!(action.enhance_quantity(), Some(2));
        assert!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Keep,
                Some(1),
                None,
                None,
            )
            .is_err()
        );
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Keep,
                None,
                Some(EnhanceLevel::new(3)),
                None,
            ),
            Err(EquipmentInventoryActionError::EnhanceQuantityRequired)
        );
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Keep,
                None,
                None,
                Some(1),
            ),
            Err(EquipmentInventoryActionError::TargetEnhanceLevelRequired)
        );
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Keep,
                None,
                Some(EnhanceLevel::new(3)),
                Some(0),
            ),
            Err(EquipmentInventoryActionError::EnhanceQuantityMustBePositive)
        );
    }

    #[test]
    fn dismantle_requires_a_positive_quantity_and_excludes_enhancement() {
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Dismantle,
                None,
                None,
                None,
            ),
            Err(EquipmentInventoryActionError::DismantleQuantityRequired)
        );
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Dismantle,
                Some(0),
                None,
                None,
            ),
            Err(EquipmentInventoryActionError::DismantleQuantityMustBePositive)
        );
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Dismantle,
                Some(1),
                Some(EnhanceLevel::new(1)),
                None,
            ),
            Err(EquipmentInventoryActionError::TargetEnhanceNotAllowed)
        );
        assert_eq!(
            EquipmentInventoryAction::new(
                warehouse_source(1000),
                EquipmentInventoryActionKind::Dismantle,
                Some(1),
                None,
                Some(1),
            ),
            Err(EquipmentInventoryActionError::EnhanceQuantityNotAllowed)
        );
    }

    #[test]
    fn plan_rejects_duplicate_sources_and_sorts_the_remaining_sources() {
        let first = EquipmentInventoryAction::new(
            warehouse_source(1002),
            EquipmentInventoryActionKind::Dismantle,
            Some(1),
            None,
            None,
        )
        .unwrap();
        let second = EquipmentInventoryAction::new(
            warehouse_source(1001),
            EquipmentInventoryActionKind::Keep,
            None,
            Some(EnhanceLevel::new(1)),
            Some(1),
        )
        .unwrap();
        let plan = EquipmentInventoryPlan::new(vec![first, second]).unwrap();
        assert_eq!(plan.actions()[0].source(), warehouse_source(1001));
        assert_eq!(plan.actions()[1].source(), warehouse_source(1002));

        let duplicate = EquipmentInventoryAction::new(
            warehouse_source(1001),
            EquipmentInventoryActionKind::Dismantle,
            Some(1),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            EquipmentInventoryPlan::new(vec![second, duplicate]),
            Err(EquipmentInventoryActionError::DuplicateSource { .. })
        ));
    }
}

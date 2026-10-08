//! 构造配装目标、库存动作与完整计划的稳定摘要输入。

use serde::Serialize;
use suzushiro_content_digest::sha256_compact_json as stable_json_sha256;

use crate::domain::{
    DesiredSlotState, DesiredState, EnhanceLevel, EquipmentInventoryPlan, SlotTarget, SourcePolicy,
};

use super::{PlanCheckError, PlanSource, PlanStep, ResourceConstraint, ResourceDelta};

#[derive(Serialize)]
struct DesiredStateDigest {
    slots: Vec<DesiredSlotDigest>,
}

#[derive(Serialize)]
struct DesiredSlotDigest {
    ship_instance_id: u64,
    slot_index: u8,
    allocation_priority: i32,
    target: DesiredTargetDigest,
}

#[derive(Serialize)]
enum DesiredTargetDigest {
    Keep,
    Empty,
    Equipment {
        family_id: u64,
        source_policy: &'static str,
        exact_source: Option<PlanSource>,
        target_enhance_level: Option<u8>,
    },
}

#[derive(Serialize)]
struct PlanDigestInput<'a> {
    schema_version: u32,
    game_state_content_sha256: &'a str,
    desired_state_content_sha256: &'a str,
    inventory_plan_content_sha256: &'a str,
    steps: &'a [PlanStep],
    resource_constraints: &'a [ResourceConstraint],
    resource_delta: &'a ResourceDelta,
}

#[derive(Serialize)]
struct InventoryPlanDigest {
    actions: Vec<InventoryActionDigest>,
}

#[derive(Serialize)]
struct InventoryActionDigest {
    source: PlanSource,
    kind: &'static str,
    dismantle_quantity: Option<u64>,
    target_enhance_level: Option<u8>,
    enhance_quantity: Option<u64>,
}

pub(super) fn desired_state_digest(desired: &DesiredState) -> Result<String, PlanCheckError> {
    let digest = DesiredStateDigest {
        slots: desired.slots().iter().map(desired_slot_digest).collect(),
    };
    stable_digest(&digest)
}

pub(super) fn inventory_plan_digest(
    inventory_plan: &EquipmentInventoryPlan,
) -> Result<String, PlanCheckError> {
    let digest = InventoryPlanDigest {
        actions: inventory_plan
            .actions()
            .iter()
            .filter(|action| !action.is_noop())
            .map(|action| InventoryActionDigest {
                source: PlanSource::from_domain(action.source()),
                kind: action.kind().stable_key(),
                dismantle_quantity: action.dismantle_quantity(),
                target_enhance_level: action.target_enhance_level().map(EnhanceLevel::get),
                enhance_quantity: action.enhance_quantity(),
            })
            .collect(),
    };
    stable_digest(&digest)
}

pub(super) fn absent_modifications_game_state_digest() -> Result<String, PlanCheckError> {
    #[derive(Serialize)]
    struct Marker {
        kind: &'static str,
    }
    stable_digest(&Marker {
        kind: "workbook-modifications-absent",
    })
}

pub(super) fn plan_digest(
    schema_version: u32,
    game_state_content_sha256: &str,
    desired_state_content_sha256: &str,
    inventory_plan_content_sha256: &str,
    steps: &[PlanStep],
    resource_constraints: &[ResourceConstraint],
    resource_delta: &ResourceDelta,
) -> Result<String, PlanCheckError> {
    stable_digest(&PlanDigestInput {
        schema_version,
        game_state_content_sha256,
        desired_state_content_sha256,
        inventory_plan_content_sha256,
        steps,
        resource_constraints,
        resource_delta,
    })
}

fn desired_slot_digest(desired: &DesiredSlotState) -> DesiredSlotDigest {
    let target = match desired.target() {
        SlotTarget::Keep => DesiredTargetDigest::Keep,
        SlotTarget::Empty => DesiredTargetDigest::Empty,
        SlotTarget::Equipment(equipment) => DesiredTargetDigest::Equipment {
            family_id: equipment.family_id().get(),
            source_policy: source_policy_key(equipment.source_policy()),
            exact_source: equipment.exact_source().map(PlanSource::from_domain),
            target_enhance_level: equipment.target_enhance_level().map(EnhanceLevel::get),
        },
    };
    DesiredSlotDigest {
        ship_instance_id: desired.slot().ship_instance_id().get(),
        slot_index: desired.slot().slot_index().get(),
        allocation_priority: desired.allocation_priority(),
        target,
    }
}

fn source_policy_key(policy: SourcePolicy) -> &'static str {
    match policy {
        SourcePolicy::CurrentThenWarehouseThenComposeThenShip => {
            "current_then_warehouse_then_compose_then_ship"
        }
        SourcePolicy::WarehouseThenCompose => "warehouse_then_compose",
        SourcePolicy::WarehouseThenComposeThenShip => "warehouse_then_compose_then_ship",
        SourcePolicy::WarehouseThenShipThenCompose => "warehouse_then_ship_then_compose",
        SourcePolicy::ComposeThenWarehouseThenShip => "compose_then_warehouse_then_ship",
        SourcePolicy::WarehouseOnly => "warehouse_only",
        SourcePolicy::ComposeOnly => "compose_only",
        SourcePolicy::ShipOnly => "ship_only",
        SourcePolicy::ExactSource => "exact_source",
    }
}

fn stable_digest<T: Serialize>(value: &T) -> Result<String, PlanCheckError> {
    stable_json_sha256(value).map_err(|source| PlanCheckError::DigestEncoding {
        message: source.to_string(),
    })
}

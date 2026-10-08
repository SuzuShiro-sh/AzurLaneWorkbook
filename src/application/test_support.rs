//! 为应用层端口契约测试提供可复用的最小假适配器和空模型。

use std::sync::{Arc, Mutex};

use super::{
    AppError, CompiledPlan, ExecutionCommand, ExecutionCommandReceipt, ExecutionPort,
    ExecutionPreflight, ExecutionReport, ExecutionSendResult, ExecutionTargetIdentity, GamePort,
    LAYOUT_SCHEMA_VERSION, NoExecutionCancellation, WorkbookGenerationPort,
    WorkbookGenerationReport, WorkbookLayout, WorkbookPort, WorkbookProjectionV4, compile_plan,
};
use crate::domain::{
    AccountResources, BagComposeAvailability, BagInventory, BagItem, DesiredState, EnhanceLevel,
    EquipmentAttribute, EquipmentCatalog, EquipmentCatalogSource, EquipmentClassification,
    EquipmentCompatibility, EquipmentComposeRecipe, EquipmentDefinition, EquipmentDetailCatalog,
    EquipmentEnhancement, EquipmentFamily, EquipmentIdentity, EquipmentInventory,
    EquipmentItemQuantity, EquipmentResources, GameState, GameStateSource, NamedEquipmentNation,
    NamedEquipmentShipType, NamedEquipmentType, RawRecordSet, ShipAttributeBreakdown,
    ShipAttributeValues, ShipCatalog, ShipCatalogGroup, ShipCatalogSource, ShipClassification,
    ShipEquipment, ShipEquipmentSlot, ShipGrowth, ShipIdentity, ShipIntimacy, ShipOilCost,
    ShipPerformance, ShipProfile, ShipRoster, ShipRosterSource, ShipStars,
    SkillEffectEvidenceCatalog, SlotIndex, WarehouseEquipmentStack,
};

/// 测试用的调用顺序记录。
pub(crate) type Events = Arc<Mutex<Vec<&'static str>>>;

/// 只返回固定布局的工作簿端口假实现。
pub(crate) struct FakeWorkbookPort {
    layout: WorkbookLayout,
    events: Events,
}

impl FakeWorkbookPort {
    pub(crate) fn new(layout: WorkbookLayout, events: Events) -> Self {
        Self { layout, events }
    }
}

impl WorkbookPort for FakeWorkbookPort {
    fn load_layout(&self) -> Result<WorkbookLayout, AppError> {
        self.events.lock().unwrap().push("layout");
        Ok(self.layout.clone())
    }
}

/// 返回固定完整状态或一次性错误的游戏端口假实现。
pub(crate) struct FakeGamePort {
    state: Option<GameState>,
    error: Option<AppError>,
    events: Events,
}

impl FakeGamePort {
    pub(crate) fn success(state: GameState, events: Events) -> Self {
        Self {
            state: Some(state),
            error: None,
            events,
        }
    }

    pub(crate) fn failure(error: AppError, events: Events) -> Self {
        Self {
            state: None,
            error: Some(error),
            events,
        }
    }
}

impl GamePort for FakeGamePort {
    fn read_full_state(&mut self) -> Result<super::GameObservation, AppError> {
        self.events.lock().unwrap().push("game");
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        Ok(super::GameObservation::from_state(
            self.state.take().expect("假游戏端口只能被读取一次"),
        ))
    }
}

/// 返回固定报告的工作簿生成端口假实现。
pub(crate) struct FakeWorkbookGenerationPort {
    report: WorkbookGenerationReport,
    events: Events,
}

impl FakeWorkbookGenerationPort {
    pub(crate) fn new(report: WorkbookGenerationReport, events: Events) -> Self {
        Self { report, events }
    }
}

impl WorkbookGenerationPort for FakeWorkbookGenerationPort {
    fn generate_workbook(
        &self,
        _requested_name: Option<&str>,
        _layout: &WorkbookLayout,
        _projection: WorkbookProjectionV4,
    ) -> Result<WorkbookGenerationReport, AppError> {
        self.events.lock().unwrap().push("generation");
        Ok(self.report.clone())
    }
}

/// 构造不含舰船、装备和资源行的最小合法布局。
pub(crate) fn empty_layout() -> WorkbookLayout {
    WorkbookLayout::new(
        LAYOUT_SCHEMA_VERSION,
        "测试布局".to_owned(),
        "验证应用服务".to_owned(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "0".repeat(64),
    )
    .expect("空测试布局应满足模型不变量")
}

/// 构造可通过工作簿投影校验的最小完整游戏状态。
pub(crate) fn empty_game_state() -> GameState {
    let digest = "1".repeat(64);
    GameState::new(
        GameStateSource::new(
            "2".repeat(64),
            1,
            1,
            1,
            2,
            1,
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
        ),
        ShipRoster::new(
            ShipRosterSource::new("2".repeat(64), digest.clone()),
            Vec::new(),
        ),
        test_ship_catalog("2".repeat(64), digest.clone(), &[]),
        EquipmentCatalog::new(
            EquipmentCatalogSource::new("2".repeat(64), digest.clone()),
            Vec::new(),
            Vec::new(),
            0,
        ),
        EquipmentDetailCatalog::new(Vec::new(), Vec::new()),
        EquipmentInventory::new(Vec::new()),
        BagInventory::new(Vec::new()),
        AccountResources::new(0, 0, 0),
        RawRecordSet::new(1, digest, Vec::new()),
    )
}

/// 通过生产执行状态机生成不含写步骤的成功报告。
pub(crate) fn empty_execution_report() -> ExecutionReport {
    let state = empty_game_state();
    let check_report = compile_plan(&state, &DesiredState::new(Vec::new()).unwrap())
        .expect("空目标应能编译为合法计划");
    let mut port = EmptyExecutionPort {
        state,
        target_identity: ExecutionTargetIdentity::new("a".repeat(64))
            .expect("测试目标身份应为规范 SHA-256"),
    };
    super::execution::execute_plan(&mut port, check_report.plan(), &NoExecutionCancellation)
        .expect("空计划应通过生产执行状态机")
}

/// 让适配器测试通过生产状态机执行一份已经编译的计划。
pub(crate) fn execute_compiled_plan(
    port: &mut dyn ExecutionPort,
    plan: &CompiledPlan,
) -> Result<ExecutionReport, AppError> {
    super::execution::execute_plan(port, plan, &NoExecutionCancellation)
}

/// 构造覆盖实际回读值、报告聚合值和未执行空值的映射测试报告。
pub(crate) fn execution_workbook_report_fixture() -> ExecutionReport {
    super::execution::execution_workbook_report_fixture()
}

/// 为执行报告夹具提供可重复读取且不会接收写命令的端口。
struct EmptyExecutionPort {
    state: GameState,
    target_identity: ExecutionTargetIdentity,
}

impl GamePort for EmptyExecutionPort {
    fn read_full_state(&mut self) -> Result<super::GameObservation, AppError> {
        Ok(super::GameObservation::from_state(self.state.clone()))
    }
}

impl ExecutionPort for EmptyExecutionPort {
    fn bind_current_session(&mut self) -> Result<(), AppError> {
        Ok(())
    }

    fn target_identity(&mut self, _state: &GameState) -> Result<ExecutionTargetIdentity, AppError> {
        Ok(self.target_identity.clone())
    }

    fn preflight_plan(&mut self, preflight: &ExecutionPreflight) -> Result<(), AppError> {
        assert_eq!(preflight.target_identity(), &self.target_identity);
        assert!(preflight.steps().is_empty());
        Ok(())
    }

    fn send_command(&mut self, _command: &ExecutionCommand) -> ExecutionSendResult {
        panic!("空计划不得发送命令")
    }

    fn query_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("空计划不得查询命令")
    }

    fn cancel_command(
        &mut self,
        _command_id: &str,
        _budget: std::time::Duration,
    ) -> Result<ExecutionCommandReceipt, AppError> {
        panic!("空计划不得取消命令")
    }
}

/// 构造覆盖单个装备族、仓库堆叠和固定五槽的计划器测试状态。
pub(crate) fn plan_game_state() -> GameState {
    plan_game_state_with_capacity(3, 300)
}

/// 构造可指定仓库容量边界的计划器测试状态。
/// 构造多艘同型舰船，供长计划按真实槽位编译。
pub(crate) fn plan_game_state_with_ship_count(ship_count: usize) -> GameState {
    let count = ship_count.max(1) as u64;
    plan_game_state_fixture(
        count,
        count.saturating_add(10),
        Some(PlanComposeFixture {
            gold: 1_000_000,
            material_quantity: count.saturating_mul(20),
            max_count: Some(count),
            live_matches_catalog: true,
        }),
        Some(PlanEnhanceFixture {
            gold: 1_000_000,
            material_quantity: count.saturating_mul(20),
        }),
        PlanCompatibilityFixture::default(),
        ship_count.max(1),
    )
}

pub(crate) fn plan_game_state_with_capacity(
    equipment_capacity: u64,
    equipment_limit: u64,
) -> GameState {
    plan_game_state_fixture(
        equipment_capacity,
        equipment_limit,
        None,
        None,
        PlanCompatibilityFixture::default(),
        1,
    )
}

/// 构造带一条可用装备合成配方的计划、执行和设备适配器测试状态。
pub(crate) fn plan_game_state_with_compose(
    gold: u64,
    material_quantity: u64,
    equipment_capacity: u64,
    equipment_limit: u64,
    max_count: Option<u64>,
) -> GameState {
    plan_game_state_fixture(
        equipment_capacity,
        equipment_limit,
        Some(PlanComposeFixture {
            gold,
            material_quantity,
            max_count,
            live_matches_catalog: true,
        }),
        None,
        PlanCompatibilityFixture::default(),
        1,
    )
}

/// 构造带两段非零强化成本的计划、执行和设备适配器测试状态。
pub(crate) fn plan_game_state_with_enhance(
    gold: u64,
    material_quantity: u64,
    equipment_capacity: u64,
    equipment_limit: u64,
) -> GameState {
    plan_game_state_fixture(
        equipment_capacity,
        equipment_limit,
        None,
        Some(PlanEnhanceFixture {
            gold,
            material_quantity,
        }),
        PlanCompatibilityFixture::default(),
        1,
    )
}

/// 构造同时带合成配方和两段非零强化成本的测试状态。
pub(crate) fn plan_game_state_with_compose_and_enhance(
    gold: u64,
    compose_material_quantity: u64,
    enhance_material_quantity: u64,
    equipment_capacity: u64,
    equipment_limit: u64,
    max_count: Option<u64>,
) -> GameState {
    plan_game_state_fixture(
        equipment_capacity,
        equipment_limit,
        Some(PlanComposeFixture {
            gold,
            material_quantity: compose_material_quantity,
            max_count,
            live_matches_catalog: true,
        }),
        Some(PlanEnhanceFixture {
            gold,
            material_quantity: enhance_material_quantity,
        }),
        PlanCompatibilityFixture::default(),
        1,
    )
}

/// 构造可指定槽位允许类型和装备禁用舰种的计划器测试状态。
pub(crate) fn plan_game_state_with_equipment_compatibility(
    allowed_equipment_type_ids: Vec<u64>,
    forbidden_ship_type_ids: Vec<u64>,
    equipment_type_ids: [u64; 3],
    include_compose: bool,
) -> GameState {
    plan_game_state_fixture(
        3,
        300,
        include_compose.then_some(PlanComposeFixture {
            gold: 1_000,
            material_quantity: 20,
            max_count: Some(4),
            live_matches_catalog: true,
        }),
        None,
        PlanCompatibilityFixture {
            allowed_equipment_type_ids,
            forbidden_ship_type_ids,
            equipment_type_ids,
            include_external_sources: true,
        },
        1,
    )
}

/// 构造仅保留目标槽位当前装备、没有外部装备来源的兼容性测试状态。
pub(crate) fn plan_game_state_with_current_equipment_only(
    allowed_equipment_type_ids: Vec<u64>,
    equipment_type_ids: [u64; 3],
) -> GameState {
    plan_game_state_fixture(
        3,
        300,
        None,
        None,
        PlanCompatibilityFixture {
            allowed_equipment_type_ids,
            forbidden_ship_type_ids: Vec::new(),
            equipment_type_ids,
            include_external_sources: false,
        },
        1,
    )
}

/// 构造静态配方与实时配方输出不一致且目标配置不兼容的测试状态。
pub(crate) fn plan_game_state_with_incompatible_compose_mismatch() -> GameState {
    plan_game_state_fixture(
        3,
        300,
        Some(PlanComposeFixture {
            gold: 1_000,
            material_quantity: 20,
            max_count: Some(4),
            live_matches_catalog: false,
        }),
        None,
        PlanCompatibilityFixture {
            allowed_equipment_type_ids: vec![1],
            forbidden_ship_type_ids: Vec::new(),
            equipment_type_ids: [2, 1, 1],
            include_external_sources: true,
        },
        1,
    )
}

#[derive(Clone, Copy)]
struct PlanComposeFixture {
    gold: u64,
    material_quantity: u64,
    max_count: Option<u64>,
    live_matches_catalog: bool,
}

#[derive(Clone, Copy)]
struct PlanEnhanceFixture {
    gold: u64,
    material_quantity: u64,
}

#[derive(Clone)]
struct PlanCompatibilityFixture {
    allowed_equipment_type_ids: Vec<u64>,
    forbidden_ship_type_ids: Vec<u64>,
    equipment_type_ids: [u64; 3],
    include_external_sources: bool,
}

impl Default for PlanCompatibilityFixture {
    fn default() -> Self {
        Self {
            allowed_equipment_type_ids: vec![1],
            forbidden_ship_type_ids: Vec::new(),
            equipment_type_ids: [1, 1, 1],
            include_external_sources: true,
        }
    }
}

fn plan_game_state_fixture(
    equipment_capacity: u64,
    equipment_limit: u64,
    compose: Option<PlanComposeFixture>,
    enhance: Option<PlanEnhanceFixture>,
    compatibility: PlanCompatibilityFixture,
    ship_count: usize,
) -> GameState {
    let digest: String = "1".repeat(64);
    let family_id = crate::domain::EquipmentFamilyId::new(1000).unwrap();
    let config_zero = crate::domain::EquipmentConfigId::new(1000).unwrap();
    let config_one = crate::domain::EquipmentConfigId::new(1001).unwrap();
    let config_two = crate::domain::EquipmentConfigId::new(1002).unwrap();
    let zero_resources = || EquipmentResources::new(0, Vec::new());
    let first_enhance_cost = || {
        enhance.map_or_else(zero_resources, |_| {
            EquipmentResources::new(10, vec![EquipmentItemQuantity::new(3001, 2)])
        })
    };
    let second_enhance_cost = || {
        enhance.map_or_else(zero_resources, |_| {
            EquipmentResources::new(20, vec![EquipmentItemQuantity::new(3001, 3)])
        })
    };
    let dismantle_resources =
        || EquipmentResources::new(10, vec![EquipmentItemQuantity::new(2001, 2)]);
    let equipment_compatibility = || {
        EquipmentCompatibility::new(
            Vec::new(),
            Vec::new(),
            compatibility
                .forbidden_ship_type_ids
                .iter()
                .map(|ship_type_id| {
                    NamedEquipmentShipType::new(*ship_type_id, format!("舰种 {ship_type_id}"))
                })
                .collect(),
        )
    };
    let definition_zero = EquipmentDefinition::new(
        EquipmentIdentity::new(
            config_zero,
            family_id,
            "测试装备".to_owned(),
            "icon".to_owned(),
        ),
        EquipmentClassification::new(
            NamedEquipmentType::new(compatibility.equipment_type_ids[0], "主炮".to_owned()),
            NamedEquipmentNation::new(1, "测试阵营".to_owned()),
            1,
            1,
            "".to_owned(),
            0,
            0,
            false,
            false,
        ),
        EquipmentEnhancement::new(
            EnhanceLevel::new(0),
            None,
            None,
            Some(config_one),
            Vec::new(),
            first_enhance_cost(),
            zero_resources(),
            dismantle_resources(),
        ),
        Vec::<EquipmentAttribute>::new(),
        equipment_compatibility(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "".to_owned(),
        0,
        None,
        0,
        0,
    );
    let definition_one = EquipmentDefinition::new(
        EquipmentIdentity::new(
            config_one,
            family_id,
            "测试装备".to_owned(),
            "icon".to_owned(),
        ),
        EquipmentClassification::new(
            NamedEquipmentType::new(compatibility.equipment_type_ids[1], "主炮".to_owned()),
            NamedEquipmentNation::new(1, "测试阵营".to_owned()),
            1,
            1,
            "".to_owned(),
            0,
            0,
            false,
            false,
        ),
        EquipmentEnhancement::new(
            EnhanceLevel::new(1),
            Some(config_zero),
            Some(config_zero),
            enhance.map(|_| config_two),
            Vec::new(),
            second_enhance_cost(),
            zero_resources(),
            zero_resources(),
        ),
        Vec::<EquipmentAttribute>::new(),
        equipment_compatibility(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "".to_owned(),
        0,
        None,
        0,
        0,
    );
    let definition_two = EquipmentDefinition::new(
        EquipmentIdentity::new(
            config_two,
            family_id,
            "测试装备".to_owned(),
            "icon".to_owned(),
        ),
        EquipmentClassification::new(
            NamedEquipmentType::new(compatibility.equipment_type_ids[2], "主炮".to_owned()),
            NamedEquipmentNation::new(1, "测试阵营".to_owned()),
            1,
            1,
            "".to_owned(),
            0,
            0,
            false,
            false,
        ),
        EquipmentEnhancement::new(
            EnhanceLevel::new(2),
            Some(config_zero),
            Some(config_one),
            None,
            Vec::new(),
            zero_resources(),
            zero_resources(),
            zero_resources(),
        ),
        Vec::<EquipmentAttribute>::new(),
        equipment_compatibility(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        "".to_owned(),
        0,
        None,
        0,
        0,
    );
    let mut definitions = vec![definition_zero, definition_one];
    if enhance.is_some() {
        definitions.push(definition_two);
    }
    let config_count = definitions.len();
    let attributes =
        ShipAttributeValues::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    let ships = (0..ship_count.max(1))
        .map(|index| {
            let slots = [
                ShipEquipmentSlot::new(
                    SlotIndex::new(1).unwrap(),
                    Some(ShipEquipment::new(1, config_zero, EnhanceLevel::new(0))),
                    compatibility.allowed_equipment_type_ids.clone(),
                ),
                ShipEquipmentSlot::new(
                    SlotIndex::new(2).unwrap(),
                    None,
                    compatibility.allowed_equipment_type_ids.clone(),
                ),
                ShipEquipmentSlot::new(
                    SlotIndex::new(3).unwrap(),
                    None,
                    compatibility.allowed_equipment_type_ids.clone(),
                ),
                ShipEquipmentSlot::new(
                    SlotIndex::new(4).unwrap(),
                    compatibility
                        .include_external_sources
                        .then_some(ShipEquipment::new(1, config_one, EnhanceLevel::new(1))),
                    compatibility.allowed_equipment_type_ids.clone(),
                ),
                ShipEquipmentSlot::new(
                    SlotIndex::new(5).unwrap(),
                    None,
                    compatibility.allowed_equipment_type_ids.clone(),
                ),
            ];
            ShipProfile::new(
                ShipIdentity::new(
                    crate::domain::ShipInstanceId::new(9001 + index as u64).unwrap(),
                    1,
                    "测试舰船".to_owned(),
                    0,
                ),
                ShipGrowth::new(1, 1, 0, 0, 0, 0, 0),
                ShipIntimacy::new(0, 100, 1, "陌生".to_owned(), false, 0),
                Vec::new(),
                ShipClassification::new(
                    1,
                    crate::domain::NamedShipClass::new(1, "驱逐".to_owned()),
                    crate::domain::NamedShipClass::new(1, "轻甲".to_owned()),
                    crate::domain::NamedShipClass::new(1, "测试阵营".to_owned()),
                    1,
                    ShipStars::new(1, 1),
                    0,
                ),
                ShipPerformance::new(
                    0,
                    false,
                    ShipOilCost::new(0, 0, 0),
                    ShipAttributeBreakdown::new(attributes, attributes, attributes),
                ),
                Vec::new(),
                slots,
            )
        })
        .collect::<Vec<_>>();
    GameState::new(
        GameStateSource::new(
            "2".repeat(64),
            1,
            1,
            1,
            2,
            1,
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
            digest.clone(),
        ),
        ShipRoster::new(ShipRosterSource::new("2".repeat(64), digest.clone()), ships),
        test_ship_catalog("2".repeat(64), digest.clone(), &[1]),
        EquipmentCatalog::new(
            EquipmentCatalogSource::new("2".repeat(64), digest.clone()),
            vec![EquipmentFamily::new(family_id, definitions)],
            compose
                .map(|_| {
                    vec![EquipmentComposeRecipe::new(
                        5001,
                        EquipmentItemQuantity::new(2001, 5),
                        100,
                        config_zero,
                    )]
                })
                .unwrap_or_default(),
            config_count,
        ),
        EquipmentDetailCatalog::new(Vec::new(), Vec::new()),
        EquipmentInventory::new(if compatibility.include_external_sources {
            vec![
                WarehouseEquipmentStack::new(
                    1,
                    config_zero,
                    family_id,
                    EnhanceLevel::new(0),
                    if ship_count <= 1 {
                        1
                    } else {
                        ship_count as u64
                    },
                ),
                WarehouseEquipmentStack::new(
                    2,
                    config_one,
                    family_id,
                    EnhanceLevel::new(1),
                    if ship_count <= 1 {
                        2
                    } else {
                        ship_count as u64
                    },
                ),
            ]
        } else {
            Vec::new()
        }),
        BagInventory::new({
            let mut items = Vec::new();
            if let Some(fixture) = compose {
                items.push(BagItem::new(
                    2001,
                    fixture.material_quantity,
                    "测试合成材料".to_owned(),
                    None,
                ));
                items.push(BagItem::new(
                    5001,
                    0,
                    "测试合成配方".to_owned(),
                    Some(BagComposeAvailability::new(
                        5001,
                        2001,
                        5,
                        100,
                        Some(if fixture.live_matches_catalog {
                            config_zero
                        } else {
                            config_one
                        }),
                        fixture.max_count,
                    )),
                ));
            }
            if let Some(fixture) = enhance {
                items.push(BagItem::new(
                    3001,
                    fixture.material_quantity,
                    "测试强化材料".to_owned(),
                    None,
                ));
            }
            items.sort_unstable_by_key(BagItem::item_id);
            items
        }),
        AccountResources::new(
            enhance
                .map(|fixture| fixture.gold)
                .or_else(|| compose.map(|fixture| fixture.gold))
                .unwrap_or(0),
            equipment_capacity,
            equipment_limit,
        ),
        RawRecordSet::new(1, digest, Vec::new()),
    )
}

pub(crate) fn test_ship_catalog(
    module_sha256: String,
    digest: String,
    group_ids: &[u64],
) -> ShipCatalog {
    ShipCatalog::new(
        ShipCatalogSource::new(module_sha256, digest),
        group_ids
            .iter()
            .map(|group_id| {
                ShipCatalogGroup::new(
                    *group_id,
                    group_id * 10 + 1,
                    "测试舰船".to_owned(),
                    "Test Ship".to_owned(),
                    1,
                    1,
                    1,
                    1,
                    125,
                    6,
                    [vec![1], vec![1], vec![1], vec![1], vec![1]],
                    vec![group_id * 10 + 1],
                    vec![format!("ship_data_group:{group_id}")],
                    Vec::new(),
                )
            })
            .collect(),
        Vec::new(),
        SkillEffectEvidenceCatalog::default(),
    )
}

/// 构造供服务接缝测试透传的稳定报告。
pub(crate) fn generation_report(
    layout: &WorkbookLayout,
    projection: &WorkbookProjectionV4,
) -> WorkbookGenerationReport {
    WorkbookGenerationReport::new(
        "data/workbooks/fake.xlsx".to_owned(),
        false,
        1_700_000_000_123,
        layout.schema_version(),
        projection.schema_version(),
        9,
        5,
        0,
        388,
        0,
        0,
        0,
        56,
        388,
        layout.content_sha256().to_owned(),
        projection.content_sha256().to_owned(),
        projection.source().game_state_content_sha256().to_owned(),
        "3".repeat(64),
        "4".repeat(64),
    )
}

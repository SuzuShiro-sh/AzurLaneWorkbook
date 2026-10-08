//! 领域 RPC：快照、装备命令和关闭。认证与帧传输留在客户端模块。

use std::io::{ErrorKind, Read};
use std::time::Duration;

use super::super::protocol::{
    AccountBeforeResult, AgentIdentity, CapabilitiesResult, ComposeRecipePageResult, EmptyPayload,
    EquipmentCatalogPagePayload, EquipmentCommandLookupPayload, EquipmentCommandReceipt,
    EquipmentConfigBatchPayload, EquipmentConfigBatchResult, EquipmentConfigPageResult,
    EquipmentReferenceNameBatchPayload, EquipmentReferenceNameBatchResult,
    EquipmentWeaponBatchPayload, EquipmentWeaponBatchResult, HealthResult, OwnedQuery,
    OwnedQueryResult, PlayerResources, RpcOperation, RuntimeEquipmentCommand, RuntimeProtocolError,
    ShipCatalogPagePayload, ShipCatalogPageResult, ShipCatalogTableKey, ShutdownPreparedResult,
    SkillEffectBatchPayload, SkillEffectBatchResult, SkillEffectQuery, SnapshotBagPayload,
    SnapshotBagResult, SnapshotOwnedStatePayload, SnapshotOwnedStateResult,
    SnapshotShipDetailsPayload, SnapshotShipDetailsResult, validate_capabilities_result,
};
use super::{AgentClient, ClientStage, RuntimeClientError};

impl AgentClient {
    /// 按对象身份与字段读取持有状态，不采集背包和玩家资源。
    pub fn query_owned(
        &mut self,
        timeout_ms: u32,
        query: &OwnedQuery,
    ) -> Result<OwnedQueryResult, RuntimeClientError> {
        query.validate()?;
        let expected = query.clone();
        self.request(
            RpcOperation::QueryOwned,
            timeout_ms,
            query.clone(),
            move |result: &OwnedQueryResult| result.validate(&expected),
        )
    }

    /// 仅实时读取物资和装备容量，不读取船坞、背包或装备条目。
    pub fn snapshot_resources(
        &mut self,
        timeout_ms: u32,
    ) -> Result<PlayerResources, RuntimeClientError> {
        self.request(
            RpcOperation::SnapshotResources,
            timeout_ms,
            EmptyPayload::default(),
            PlayerResources::validate,
        )
    }

    /// 按严格升序的显式 ID 读取完整配置，并独立报告不存在的标识。
    pub fn snapshot_equipment_configs_by_ids(
        &mut self,
        timeout_ms: u32,
        ids: &[u64],
        expected_module_sha256: &str,
    ) -> Result<EquipmentConfigBatchResult, RuntimeClientError> {
        let payload = EquipmentConfigBatchPayload::new(ids)?;
        let requested = payload.ids.clone();
        let expected = expected_module_sha256.to_owned();
        self.catalog_request(
            RpcOperation::SnapshotEquipmentConfigs,
            timeout_ms,
            payload,
            move |result: &EquipmentConfigBatchResult| result.validate(&requested, &expected),
        )
    }

    /// 查询 agent、目标进程和主线程队列状态。
    pub fn health(&mut self, timeout_ms: u32) -> Result<HealthResult, RuntimeClientError> {
        let identity: AgentIdentity = self.identity.clone();
        self.request(
            RpcOperation::Health,
            timeout_ms,
            EmptyPayload::default(),
            move |result: &HealthResult| result.validate(&identity),
        )
    }

    /// 查询完整的只读能力集合。
    pub fn capabilities(
        &mut self,
        timeout_ms: u32,
    ) -> Result<CapabilitiesResult, RuntimeClientError> {
        self.request(
            RpcOperation::Capabilities,
            timeout_ms,
            EmptyPayload::default(),
            validate_capabilities_result,
        )
    }

    /// 在明确条目上限内读取背包快照。
    pub fn snapshot_bag(
        &mut self,
        timeout_ms: u32,
        max_items: u32,
    ) -> Result<SnapshotBagResult, RuntimeClientError> {
        let payload: SnapshotBagPayload = SnapshotBagPayload::new(max_items)?;
        self.request(
            RpcOperation::SnapshotBag,
            timeout_ms,
            payload,
            move |result: &SnapshotBagResult| result.validate(max_items),
        )
    }

    /// 在同一次游戏主线程停顿内读取舰船养成、自身技能、装备、背包和玩家资源。
    pub fn snapshot_owned_state(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
    ) -> Result<SnapshotOwnedStateResult, RuntimeClientError> {
        let payload: SnapshotOwnedStatePayload =
            SnapshotOwnedStatePayload::new(max_ships, max_equipments, max_items)?;
        self.request(
            RpcOperation::SnapshotOwnedState,
            timeout_ms,
            payload,
            move |result: &SnapshotOwnedStateResult| {
                result.validate(max_ships, max_equipments, max_items)
            },
        )
    }

    /// 在同一次船坞遍历里读取账号前窗口的养成和舰船详情。
    pub fn snapshot_account_before(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        max_equipments: u32,
        max_items: u32,
        expected_module_sha256: &str,
    ) -> Result<AccountBeforeResult, RuntimeClientError> {
        let payload: SnapshotOwnedStatePayload =
            SnapshotOwnedStatePayload::new(max_ships, max_equipments, max_items)?;
        let expected_module_sha256 = expected_module_sha256.to_owned();
        self.request(
            RpcOperation::SnapshotAccountBefore,
            timeout_ms,
            payload,
            move |result: &AccountBeforeResult| {
                result.validate(
                    max_ships,
                    max_equipments,
                    max_items,
                    &expected_module_sha256,
                )
            },
        )
    }

    /// 读取当前客户端解析出的舰船分类、属性阶段和自身技能展示详情。
    pub fn snapshot_ship_details(
        &mut self,
        timeout_ms: u32,
        max_ships: u32,
        expected_module_sha256: &str,
    ) -> Result<SnapshotShipDetailsResult, RuntimeClientError> {
        let payload: SnapshotShipDetailsPayload = SnapshotShipDetailsPayload::new(max_ships)?;
        let expected_module_sha256: String = expected_module_sha256.to_owned();
        self.request(
            RpcOperation::SnapshotShipDetails,
            timeout_ms,
            payload,
            move |result: &SnapshotShipDetailsResult| {
                result.validate(max_ships, &expected_module_sha256)
            },
        )
    }

    /// 读取固定白名单中一张舰船静态配置表的一页完整物化记录。
    pub fn snapshot_ship_catalog(
        &mut self,
        timeout_ms: u32,
        table_key: ShipCatalogTableKey,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<ShipCatalogPageResult, RuntimeClientError> {
        let payload = ShipCatalogPagePayload::new(table_key, start_index, page_size)?;
        let expected_module_sha256 = expected_module_sha256.to_owned();
        let request = if ShipCatalogTableKey::ALL.contains(&table_key) {
            Self::catalog_request
        } else {
            Self::request
        };
        request(
            self,
            RpcOperation::SnapshotShipCatalog,
            timeout_ms,
            payload,
            move |result: &ShipCatalogPageResult| {
                result.validate(table_key, start_index, page_size, &expected_module_sha256)
            },
        )
    }

    /// 读取 `equip_data_template.all` 中一页完整装备配置。
    pub fn snapshot_equipment_configs(
        &mut self,
        timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<EquipmentConfigPageResult, RuntimeClientError> {
        let payload = EquipmentCatalogPagePayload::new(start_index, page_size)?;
        let expected_module_sha256 = expected_module_sha256.to_owned();
        self.catalog_request(
            RpcOperation::SnapshotEquipmentConfigs,
            timeout_ms,
            payload,
            move |result: &EquipmentConfigPageResult| {
                result.validate(start_index, page_size, &expected_module_sha256)
            },
        )
    }

    /// 读取 `compose_data_template.all` 中一页静态装备合成配方。
    pub fn snapshot_compose_recipes(
        &mut self,
        timeout_ms: u32,
        start_index: u32,
        page_size: u32,
        expected_module_sha256: &str,
    ) -> Result<ComposeRecipePageResult, RuntimeClientError> {
        let payload = EquipmentCatalogPagePayload::new(start_index, page_size)?;
        let expected_module_sha256 = expected_module_sha256.to_owned();
        self.catalog_request(
            RpcOperation::SnapshotComposeRecipes,
            timeout_ms,
            payload,
            move |result: &ComposeRecipePageResult| {
                result.validate(start_index, page_size, &expected_module_sha256)
            },
        )
    }

    /// 按严格升序的显式 ID 批量读取装备武器原始参数。
    pub fn snapshot_equipment_weapons(
        &mut self,
        timeout_ms: u32,
        weapon_ids: &[u64],
        expected_module_sha256: &str,
    ) -> Result<EquipmentWeaponBatchResult, RuntimeClientError> {
        let payload = EquipmentWeaponBatchPayload::new(weapon_ids)?;
        let requested_weapon_ids = payload.weapon_ids.clone();
        let expected_module_sha256 = expected_module_sha256.to_owned();
        self.catalog_request(
            RpcOperation::SnapshotEquipmentWeapons,
            timeout_ms,
            payload,
            move |result: &EquipmentWeaponBatchResult| {
                result.validate(&requested_weapon_ids, &expected_module_sha256)
            },
        )
    }

    /// 按严格升序的 `(skill_id, level)` 批量读取三路技能效果证据。
    pub fn snapshot_skill_effects(
        &mut self,
        timeout_ms: u32,
        skills: &[SkillEffectQuery],
        expected_module_sha256: &str,
    ) -> Result<SkillEffectBatchResult, RuntimeClientError> {
        let payload = SkillEffectBatchPayload::new(skills)?;
        let requested_skills = payload.skills.clone();
        let expected_module_sha256 = expected_module_sha256.to_owned();
        self.catalog_request(
            RpcOperation::SnapshotSkillEffects,
            timeout_ms,
            payload,
            move |result: &SkillEffectBatchResult| {
                result.validate(&requested_skills, &expected_module_sha256)
            },
        )
    }

    /// 按四个已去重命名空间批量解析当前客户端显示名称。
    pub fn snapshot_equipment_reference_names(
        &mut self,
        timeout_ms: u32,
        equipment_type_ids: &[u64],
        nation_ids: &[u64],
        ship_type_ids: &[u64],
        attribute_keys: &[String],
        expected_module_sha256: &str,
    ) -> Result<EquipmentReferenceNameBatchResult, RuntimeClientError> {
        let payload = EquipmentReferenceNameBatchPayload::new(
            equipment_type_ids,
            nation_ids,
            ship_type_ids,
            attribute_keys,
        )?;
        let requested = payload.clone();
        let expected_module_sha256 = expected_module_sha256.to_owned();
        self.catalog_request(
            RpcOperation::SnapshotEquipmentReferenceNames,
            timeout_ms,
            payload,
            move |result: &EquipmentReferenceNameBatchResult| {
                result.validate(&requested, &expected_module_sha256)
            },
        )
    }

    /// 最多派发一次装备命令；重复的相同 command_id 由 Agent 幂等账本收敛。
    pub fn execute_equipment_command(
        &mut self,
        timeout_ms: u32,
        command: &RuntimeEquipmentCommand,
    ) -> Result<EquipmentCommandReceipt, RuntimeClientError> {
        command.validate()?;
        let expected_command_id = command.command_id().to_owned();
        self.request(
            RpcOperation::ExecuteEquipmentCommand,
            timeout_ms,
            command.clone(),
            move |result: &EquipmentCommandReceipt| result.validate(&expected_command_id),
        )
    }

    /// 查询原装备命令当前能够确认的状态，不重新派发写入。
    pub fn query_equipment_command(
        &mut self,
        timeout_ms: u32,
        command_id: &str,
        socket_budget: Duration,
    ) -> Result<EquipmentCommandReceipt, RuntimeClientError> {
        let payload = EquipmentCommandLookupPayload::new(command_id)?;
        let expected_command_id = payload.command_id.clone();
        self.request_with_socket_budget(
            RpcOperation::QueryEquipmentCommand,
            timeout_ms,
            payload,
            move |result: &EquipmentCommandReceipt| result.validate(&expected_command_id),
            Some(socket_budget),
        )
    }

    /// 请求停止 Agent 对原命令的等待，并返回当前原命令状态。
    pub fn cancel_equipment_command(
        &mut self,
        timeout_ms: u32,
        command_id: &str,
        socket_budget: Duration,
    ) -> Result<EquipmentCommandReceipt, RuntimeClientError> {
        let payload = EquipmentCommandLookupPayload::new(command_id)?;
        let expected_command_id = payload.command_id.clone();
        self.request_with_socket_budget(
            RpcOperation::CancelEquipmentCommand,
            timeout_ms,
            payload,
            move |result: &EquipmentCommandReceipt| result.validate(&expected_command_id),
            Some(socket_budget),
        )
    }

    /// 排空 Agent 队列并确认服务端在收据后关闭连接；调用后客户端永久封闭。
    pub fn shutdown(
        &mut self,
        timeout_ms: u32,
    ) -> Result<ShutdownPreparedResult, RuntimeClientError> {
        let identity: AgentIdentity = self.identity.clone();
        let session_id = self.session_id;
        let result: Result<ShutdownPreparedResult, RuntimeClientError> = self.request(
            RpcOperation::Shutdown,
            timeout_ms,
            EmptyPayload::default(),
            move |result: &ShutdownPreparedResult| result.validate(session_id, &identity),
        );
        self.usable = false;
        let prepared: ShutdownPreparedResult = result?;

        let mut unexpected: [u8; 1] = [0; 1];
        match self.stream.read(&mut unexpected) {
            Ok(0) => Ok(prepared),
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                        | ErrorKind::BrokenPipe
                        | ErrorKind::UnexpectedEof
                ) =>
            {
                Ok(prepared)
            }
            Ok(_) => Err(RuntimeClientError::Protocol(RuntimeProtocolError::new(
                "shutdown_connection_not_closed",
                "Agent 在 shutdown 收据后仍发送了额外数据",
            ))),
            Err(source) => Err(RuntimeClientError::Io {
                stage: ClientStage::ReadShutdownEof,
                source,
            }),
        }
    }
}

//! 验证宿主客户端与假 agent 之间的严格 RPC 线上契约和失败边界。

#![recursion_limit = "256"]

use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use azur_lane_workbook::adapters::device::runtime::{
    AgentClient, ClientStage, EquipmentCommandAction, EquipmentCommandEquipment,
    EquipmentCommandMaterialCost, EquipmentCommandPhase, EquipmentCommandStatus, ExpectedAgent,
    RetryDirective, RuntimeAbi, RuntimeClientError, RuntimeEquipmentCommand, RuntimeProtocolError,
    SessionEffect, ShipCatalogTableKey,
};
use azur_lane_workbook::adapters::device::session::{SessionId, SessionSecret};
use serde_json::{Value, json};
use suzushiro_secure_channel::{
    HANDSHAKE_NONCE_BYTES, PROTECTED_OVERHEAD_BYTES, SECRET_BYTES, SESSION_ID_BYTES, SecureChannel,
    create_server_challenge, verify_client_proof,
};

// 测试固定同一组预期身份，便于只改变单个协议变量验证失败分类。
const SESSION_ID: &str = "00112233445566778899aabbccddeeff";
const SESSION_SECRET: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const PACKAGE_NAME: &str = "com.bilibili.azurlane";
const PROCESS_ID: u32 = 13_882;
const MODULE_SHA256: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const COMMAND_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TARGET_SHA256: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PLAN_SHA256: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const PRE_STATE_SHA256: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
// 延迟必须跨过基础 I/O 时限但留在协议时限内，同时给 Windows 握手保留稳定余量。
const FIXTURE_MAX_PLAINTEXT_BYTES: usize = 32 * 1024 * 1024;

thread_local! {
    /// 每个假 Agent 线程只服务一条连接，方向状态随线程结束自动擦除。
    static FIXTURE_SECURE_CHANNEL: RefCell<Option<SecureChannel>> = const { RefCell::new(None) };
}

#[test]
/// 拆解命令在任何网络 I/O 前拒绝强化过的来源装备。
fn equipment_command_rejects_enhanced_dismantle_locally() {
    let source = EquipmentCommandEquipment::new(700, 700, 1).expect("来源装备应有效");
    let error = EquipmentCommandAction::dismantle(source, 3, 1, 10, 300)
        .expect_err("强化过的装备不得进入拆解协议");

    assert_eq!(error.code, "equipment_command_precondition_invalid");
    assert!(error.message.contains("未强化"));
}

#[test]
/// 验证握手、健康、能力和背包快照共用一个认证连接及递增请求号。
fn readonly_sequence_uses_one_authenticated_connection() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);

        let health_request: Value = read_json_frame(&mut stream);
        assert_eq!(health_request["request_id"], "0000000000000001");
        assert_eq!(health_request["operation"], "health");
        assert_eq!(health_request["payload"], json!({}));
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": health_result()
            }),
        );

        let capabilities_request: Value = read_json_frame(&mut stream);
        assert_eq!(capabilities_request["request_id"], "0000000000000002");
        assert_eq!(capabilities_request["operation"], "capabilities");
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000002",
                "status": "ok",
                "result": capabilities_result(false)
            }),
        );

        let snapshot_request: Value = read_json_frame(&mut stream);
        assert_eq!(snapshot_request["request_id"], "0000000000000003");
        assert_eq!(snapshot_request["operation"], "snapshot_bag");
        assert_eq!(snapshot_request["payload"], json!({"max_items": 2000}));
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000003",
                "status": "ok",
                "result": {
                    "schema_version": 1,
                    "complete": true,
                    "count": 1,
                    "truncated": false,
                    "items": [{
                        "item_id": 20001,
                        "quantity": 37,
                        "kind": "bag",
                        "resolved_name": "物资样本",
                        "compose_recipe": null
                    }],
                    "read_errors": []
                }
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    assert_eq!(client.handshake_attempts(), 1);
    let health = client.health(5_000).expect("health 应成功");
    let capabilities = client.capabilities(5_000).expect("capabilities 应成功");
    let snapshot = client
        .snapshot_bag(5_000, 2_000)
        .expect("snapshot_bag 应成功");

    assert!(health.main_thread_queue_ready);
    assert!(capabilities.capabilities["read.bag"].available);
    assert!(snapshot.complete);
    assert_eq!(snapshot.items[0].item_id, 20_001);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// execute、query 与 cancel 必须沿用同一认证连接并始终描述原命令状态。
fn equipment_command_lifecycle_uses_one_authenticated_connection() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);

        let execute_request: Value = read_json_frame(&mut stream);
        assert_eq!(execute_request["request_id"], "0000000000000001");
        assert_eq!(execute_request["operation"], "execute_equipment_command");
        assert_eq!(
            execute_request["payload"],
            json!({
                "schema_version": 2,
                "command_id": COMMAND_ID,
                "target_fingerprint_sha256": TARGET_SHA256,
                "plan_hash": PLAN_SHA256,
                "sequence": 1,
                "pre_state_content_sha256": PRE_STATE_SHA256,
                "action": {
                    "kind": "unequip",
                    "ship_id": 9001,
                    "slot_index": 1,
                    "target_before": {
                        "equipment_id": 500,
                        "config_id": 500,
                        "enhance_level": 10
                    },
                    "target_warehouse_quantity_before": 2,
                    "equipment_capacity_before": 10,
                    "equipment_limit_before": 300
                }
            })
        );
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000001",
            "unknown",
            "observing",
            false,
        );

        let query_request: Value = read_json_frame(&mut stream);
        assert_eq!(query_request["request_id"], "0000000000000002");
        assert_eq!(query_request["operation"], "query_equipment_command");
        assert_eq!(query_request["payload"], json!({"command_id": COMMAND_ID}));
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000002",
            "success",
            "succeeded",
            false,
        );

        let cancel_request: Value = read_json_frame(&mut stream);
        assert_eq!(cancel_request["request_id"], "0000000000000003");
        assert_eq!(cancel_request["operation"], "cancel_equipment_command");
        assert_eq!(cancel_request["payload"], json!({"command_id": COMMAND_ID}));
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000003",
            "success",
            "succeeded",
            true,
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let command = unequip_command().expect("合法卸装命令应构造成功");
    let initial = client
        .execute_equipment_command(5_000, &command)
        .expect("命令派发后应返回 unknown 收据");
    assert_eq!(initial.status, EquipmentCommandStatus::Unknown);
    assert_eq!(initial.phase, EquipmentCommandPhase::Observing);

    let queried = client
        .query_equipment_command(5_000, COMMAND_ID, Duration::from_millis(5_000))
        .expect("查询应收敛为成功");
    assert_eq!(queried.status, EquipmentCommandStatus::Success);
    assert_eq!(queried.phase, EquipmentCommandPhase::Succeeded);

    let cancelled = client
        .cancel_equipment_command(5_000, COMMAND_ID, Duration::from_millis(5_000))
        .expect("终态命令取消请求应幂等返回原状态");
    assert_eq!(cancelled.status, EquipmentCommandStatus::Success);
    assert!(cancelled.cancel_requested);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 拆解请求只发送仓库来源和数量，不允许舰船字段混入线上契约。
fn dismantle_command_serializes_a_strict_warehouse_action() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);

        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["operation"], "execute_equipment_command");
        assert_eq!(
            request["payload"],
            json!({
                "schema_version": 2,
                "command_id": COMMAND_ID,
                "target_fingerprint_sha256": TARGET_SHA256,
                "plan_hash": PLAN_SHA256,
                "sequence": 1,
                "pre_state_content_sha256": PRE_STATE_SHA256,
                "action": {
                    "kind": "dismantle",
                    "source_before": {
                        "equipment_id": 700,
                        "config_id": 700,
                        "enhance_level": 0
                    },
                    "source_quantity_before": 3,
                    "dismantle_quantity": 1,
                    "equipment_capacity_before": 10,
                    "equipment_limit_before": 300
                }
            })
        );
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000001",
            "unknown",
            "observing",
            false,
        );
    });

    let mut client = connect_client(address).expect("握手应成功");
    let receipt = client
        .execute_equipment_command(5_000, &dismantle_command().expect("拆解命令应有效"))
        .expect("严格拆解请求应通过传输");

    assert_eq!(receipt.status, EquipmentCommandStatus::Unknown);
    assert_eq!(receipt.phase, EquipmentCommandPhase::Observing);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 合成请求只发送配方及同一状态中的产物、材料、物资和容量前态。
fn compose_command_serializes_a_strict_resource_action() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);

        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["operation"], "execute_equipment_command");
        assert_eq!(
            request["payload"],
            json!({
                "schema_version": 2,
                "command_id": COMMAND_ID,
                "target_fingerprint_sha256": TARGET_SHA256,
                "plan_hash": PLAN_SHA256,
                "sequence": 1,
                "pre_state_content_sha256": PRE_STATE_SHA256,
                "action": {
                    "kind": "compose",
                    "recipe_id": 2,
                    "compose_quantity": 2,
                    "output_config_id": 1000,
                    "output_before": {
                        "equipment_id": 800,
                        "config_id": 1000,
                        "enhance_level": 0
                    },
                    "output_quantity_before": 3,
                    "material_id": 20001,
                    "material_quantity_before": 20,
                    "material_quantity_per_unit": 5,
                    "gold_before": 1000,
                    "gold_per_unit": 100,
                    "equipment_capacity_before": 10,
                    "equipment_limit_before": 300
                }
            })
        );
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000001",
            "unknown",
            "observing",
            false,
        );
    });

    let mut client = connect_client(address).expect("握手应成功");
    let receipt = client
        .execute_equipment_command(5_000, &compose_command().expect("合成命令应有效"))
        .expect("严格合成请求应通过传输");

    assert_eq!(receipt.status, EquipmentCommandStatus::Unknown);
    assert_eq!(receipt.phase, EquipmentCommandPhase::Observing);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 仓库和舰上强化在线路上只发送各自位置所需字段，并绑定同一前态资源。
fn enhance_commands_serialize_strict_location_actions() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);

        let warehouse_request: Value = read_json_frame(&mut stream);
        assert_eq!(warehouse_request["operation"], "execute_equipment_command");
        assert_eq!(
            warehouse_request["payload"]["action"],
            json!({
                "kind": "enhance_warehouse",
                "source_before": {
                    "equipment_id": 900,
                    "config_id": 1000,
                    "enhance_level": 0
                },
                "source_quantity_before": 3,
                "target_config_id": 1001,
                "target_enhance_level": 1,
                "target_before": {
                    "equipment_id": 901,
                    "config_id": 1001,
                    "enhance_level": 1
                },
                "target_warehouse_quantity_before": 2,
                "materials": [
                    {"item_id": 17001, "quantity_before": 10, "cost": 2},
                    {"item_id": 17002, "quantity_before": 4, "cost": 1}
                ],
                "gold_before": 1000,
                "gold_cost": 20,
                "equipment_capacity_before": 5,
                "equipment_limit_before": 300
            })
        );
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000001",
            "unknown",
            "observing",
            false,
        );

        let ship_request: Value = read_json_frame(&mut stream);
        assert_eq!(ship_request["operation"], "execute_equipment_command");
        assert_eq!(
            ship_request["payload"]["action"],
            json!({
                "kind": "enhance_ship",
                "ship_id": 9001,
                "slot_index": 1,
                "source_before": {
                    "equipment_id": 900,
                    "config_id": 1000,
                    "enhance_level": 0
                },
                "target_config_id": 1001,
                "target_enhance_level": 1,
                "materials": [
                    {"item_id": 17001, "quantity_before": 10, "cost": 2},
                    {"item_id": 17002, "quantity_before": 4, "cost": 1}
                ],
                "gold_before": 1000,
                "gold_cost": 20,
                "equipment_capacity_before": 5,
                "equipment_limit_before": 300
            })
        );
        write_equipment_command_receipt(
            &mut stream,
            "0000000000000002",
            "unknown",
            "observing",
            false,
        );
    });

    let mut client = connect_client(address).expect("握手应成功");
    client
        .execute_equipment_command(
            5_000,
            &warehouse_enhance_command().expect("仓库强化命令应有效"),
        )
        .expect("严格仓库强化请求应通过传输");
    client
        .execute_equipment_command(5_000, &ship_enhance_command().expect("舰上强化命令应有效"))
        .expect("严格舰上强化请求应通过传输");
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 装备命令在任何网络 I/O 前拒绝超出普通五槽范围的参数。
fn equipment_command_rejects_invalid_slot_locally() {
    let target = EquipmentCommandEquipment::new(500, 500, 10).expect("装备前态应有效");
    let error = EquipmentCommandAction::unequip(9_001, 6, target, 2, 10, 300).unwrap_err();

    assert_eq!(error.code, "equipment_command_slot_invalid");
}

#[test]
/// 回执 command_id 不匹配时必须封闭会话，避免把其他写入结果关联到当前命令。
fn equipment_command_rejects_mismatched_receipt_id() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": {
                    "schema_version": 1,
                    "command_id": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                    "status": "unknown",
                    "phase": "observing",
                    "write_dispatched": true,
                    "cancel_requested": false,
                    "observation_count": 1,
                    "error_code": null,
                    "message": null
                }
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let error = client
        .execute_equipment_command(5_000, &unequip_command().expect("命令应有效"))
        .unwrap_err();

    assert_eq!(error.code(), "agent_identity_mismatch");
    assert_eq!(
        client.health(5_000).unwrap_err().code(),
        "runtime_session_unusable"
    );
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 验证完整运行态请求使用三个显式上限，并返回同一响应中的全部子快照。
fn owned_state_snapshot_uses_strict_typed_contract() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["request_id"], "0000000000000001");
        assert_eq!(request["operation"], "snapshot_owned_state");
        assert_eq!(
            request["payload"],
            json!({
                "max_ships": 500,
                "max_equipments": 1000,
                "max_items": 2000
            })
        );
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": owned_state_result()
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let snapshot = client
        .snapshot_owned_state(5_000, 500, 1_000, 2_000)
        .expect("snapshot_owned_state 应成功");

    assert!(snapshot.complete);
    assert_eq!(snapshot.dock.ships[0].ship_id, 9_001);
    assert_eq!(snapshot.dock.ships[0].config_id, 101_174);
    assert_eq!(snapshot.dock.ships[0].level, 100);
    assert_eq!(snapshot.dock.ships[0].experience_in_level, 3_000_000);
    assert_eq!(snapshot.dock.ships[0].intimacy_raw, 10_000);
    assert_eq!(snapshot.dock.ships[0].energy, 150);
    assert_eq!(snapshot.dock.ships[0].proficiency, 0);
    assert_eq!(snapshot.dock.ships[0].fleet_memberships.len(), 1);
    assert_eq!(snapshot.dock.ships[0].fleet_memberships[0].fleet_id, 1);
    assert_eq!(
        snapshot.dock.ships[0].fleet_memberships[0]
            .display_name
            .as_deref(),
        Some("第一舰队")
    );
    assert_eq!(snapshot.dock.ships[0].skills[0].skill_id, 10_410);
    assert_eq!(
        snapshot.dock.ships[0].slots[0]
            .equipment
            .as_ref()
            .expect("一号槽应有装备")
            .config_id,
        500
    );
    assert_eq!(snapshot.warehouse.items[0].quantity, 2);
    assert_eq!(snapshot.player.gold, 123_456);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 验证宿主明确拒绝旧版完整运行态，避免把缺失养成字段误解为零。
fn owned_state_rejects_legacy_schema() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
        let mut result: Value = owned_state_result();
        result["schema_version"] = json!(1);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": result
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let error: RuntimeClientError = client
        .snapshot_owned_state(5_000, 500, 1_000, 2_000)
        .unwrap_err();

    assert_eq!(error.code(), "owned_state_schema_unsupported");
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 验证舰船详情请求只携带显式上限，并解析分类、属性阶段和自身技能详情。
fn ship_details_snapshot_uses_strict_typed_contract() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["request_id"], "0000000000000001");
        assert_eq!(request["operation"], "snapshot_ship_details");
        assert_eq!(request["payload"], json!({"max_ships": 500}));
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": ship_details_result()
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let snapshot = client
        .snapshot_ship_details(5_000, 500, MODULE_SHA256)
        .expect("snapshot_ship_details 应成功");

    assert!(snapshot.complete);
    assert_eq!(snapshot.source.module_sha256, MODULE_SHA256);
    assert_eq!(snapshot.ships[0].name, "测试舰船");
    assert_eq!(snapshot.ships[0].intimacy_stage_id, 5);
    assert_eq!(snapshot.ships[0].intimacy_stage_description, "爱");
    assert_eq!(snapshot.ships[0].classification.ship_type_name, "驱逐舰");
    assert_eq!(snapshot.ships[0].base_attributes.speed, 16.5);
    assert_eq!(snapshot.ships[0].effective_attributes.cannon, 132.0);
    assert_eq!(
        snapshot.ships[0].slot_rules[1].allowed_equipment_type_ids,
        vec![5, 10]
    );
    assert_eq!(snapshot.ships[0].skills[0].effective_skill_id, 10_411);
    assert_eq!(snapshot.ships[0].skills[0].current_effect, "当前效果");
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 舰船详情协议升级后拒绝旧 schema，避免静默把缺失规则解释为空集合。
fn ship_details_rejects_old_schema() {
    let mut result: Value = ship_details_result();
    result["schema_version"] = json!(1);

    assert_ship_details_result_rejected(result, "ship_details_schema_unsupported");
}

/// 通过真实客户端解码入口断言一份舰船详情结果以指定协议错误被拒绝。
fn assert_ship_details_result_rejected(result: Value, expected_code: &str) {
    let (address, server) = spawn_server(move |mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": result
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let error: RuntimeClientError = client
        .snapshot_ship_details(5_000, 500, MODULE_SHA256)
        .unwrap_err();

    assert_eq!(error.code(), expected_code);
    server.join().expect("假 agent 不应 panic");
}

#[test]
fn equipment_config_page_uses_strict_cursor_contract() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["request_id"], "0000000000000001");
        assert_eq!(request["operation"], "snapshot_equipment_configs");
        assert_eq!(
            request["payload"],
            json!({"start_index": 0, "page_size": 2})
        );
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": equipment_config_page_result()
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let page = client
        .snapshot_equipment_configs(5_000, 0, 2, MODULE_SHA256)
        .expect("装备配置页应通过契约校验");

    assert!(page.complete);
    assert_eq!(page.total_count, 3);
    assert_eq!(page.next_index, Some(2));
    assert_eq!(page.configs[0].config_id, 500);
    assert_eq!(page.configs[0].root_config_id, Some(500));
    assert_eq!(page.configs[0].raw_config["trans_use_gold"], 20);
    assert_eq!(page.configs[0].weapon_ids, vec![1001, 1002]);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 舰船静态目录请求固定表键和零基游标，并保留全字段物化对象。
fn ship_catalog_page_uses_strict_typed_contract() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["request_id"], "0000000000000001");
        assert_eq!(request["operation"], "snapshot_ship_catalog");
        assert_eq!(
            request["payload"],
            json!({
                "table_key": "ship_data_statistics",
                "start_index": 32,
                "page_size": 2
            })
        );
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": ship_catalog_page_result("ship_data_statistics")
            }),
        );
    });

    let mut client = connect_client(address).expect("握手应成功");
    let page = client
        .snapshot_ship_catalog(
            5_000,
            ShipCatalogTableKey::ShipDataStatistics,
            32,
            2,
            MODULE_SHA256,
        )
        .expect("舰船静态目录页应通过契约校验");

    assert!(page.complete);
    assert_eq!(page.table_key, ShipCatalogTableKey::ShipDataStatistics);
    assert_eq!(page.records.len(), 2);
    assert_eq!(page.records[0].raw["name"], "测试舰船");
    assert_eq!(page.records[1].raw["base"], 4_001);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 响应表键不能偏离请求白名单项，避免不同配置表被并入同一捕获位置。
fn ship_catalog_page_rejects_table_mismatch() {
    let result = ship_catalog_page_result("ship_data_group");
    assert_ship_catalog_result_rejected(
        result,
        ShipCatalogTableKey::ShipDataStatistics,
        "ship_catalog_table_mismatch",
    );
}

#[test]
/// shutdown 必须发送严格空载荷，核对排空收据，并等待服务端 EOF 后才算成功。
fn shutdown_requires_prepared_receipt_and_server_eof() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["request_id"], "0000000000000001");
        assert_eq!(request["operation"], "shutdown");
        assert_eq!(request["payload"], json!({}));
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": request["request_id"],
                "status": "ok",
                "result": shutdown_result()
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let prepared = client.shutdown(5_000).expect("EOF 前的排空收据应成功");

    assert_eq!(prepared.process_id, PROCESS_ID);
    assert_eq!(prepared.worker_tid, 13_900);
    assert_eq!(prepared.worker_start_time, 987_654);
    assert_eq!(
        client.health(5_000).unwrap_err().code(),
        "runtime_session_unusable"
    );
    server.join().expect("假 agent 不应 panic");
}

/// 连续短连接只允许三次，耗尽后保留最后一次握手传输错误。
#[test]
fn handshake_transport_retries_are_bounded() {
    let listener: TcpListener = TcpListener::bind(("127.0.0.1", 0)).expect("应绑定环回端口");
    let address: SocketAddr = listener.local_addr().expect("应取得环回端口");
    let server: JoinHandle<()> = thread::spawn(move || {
        for _attempt in 1_u32..=3 {
            let (stream, _peer): (TcpStream, SocketAddr) =
                listener.accept().expect("应接受有界宿主连接");
            drop(stream);
        }
    });

    let error: RuntimeClientError = connect_client(address).expect_err("三次短连接后应失败");
    assert_eq!(error.code(), "runtime_handshake_attempts_exhausted");
    assert!(matches!(
        error,
        RuntimeClientError::HandshakeAttemptsExhausted { attempts: 3, .. }
    ));
    server.join().expect("假 agent 不应 panic");
}

/// 服务端证明无效时宿主不得泄露客户端证明，也不得把认证失败当作可重试传输错误。
#[test]
fn handshake_rejects_invalid_server_proof_before_client_proof() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        let session_id: SessionId = SESSION_ID.parse().unwrap();
        let mut session_id_bytes = [0; SESSION_ID_BYTES];
        session_id.copy_bytes_to(&mut session_id_bytes);
        let wrong_secret = [0x77; SECRET_BYTES];
        let challenge = create_server_challenge(
            &session_id_bytes,
            &wrong_secret,
            &[0x33; HANDSHAKE_NONCE_BYTES],
        );
        write_raw_frame(&mut stream, challenge.as_bytes());
        let mut unexpected = [0; 1];
        match stream.read(&mut unexpected) {
            Ok(0) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                        | ErrorKind::UnexpectedEof
                ) => {}
            outcome => panic!("无效服务端证明后宿主仍发送了数据或返回异常结果: {outcome:?}"),
        }
    });

    let error: RuntimeClientError = connect_client(address).unwrap_err();
    assert_eq!(error.code(), "runtime_secure_channel_invalid");
    assert!(matches!(
        error,
        RuntimeClientError::SecureChannel {
            stage: ClientStage::ReadChallenge,
            ..
        }
    ));
    server.join().expect("假 agent 不应 panic");
}

/// 验证同一逻辑读取可在保持连接时使用递增线序请求号重试。
#[test]
fn same_request_retry_keeps_connection_and_increments_wire_request_id() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);

        let first_request: Value = read_json_frame(&mut stream);
        assert_eq!(first_request["request_id"], "0000000000000001");
        assert_eq!(first_request["operation"], "snapshot_bag");
        assert_eq!(first_request["payload"], json!({"max_items": 2000}));
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "error",
                "error": {
                    "code": "lua_bag_proxy_invalid",
                    "stage": "agent.lua",
                    "message": "BagProxy 尚未就绪",
                    "retry": "same_request",
                    "session_effect": "unchanged",
                    "details": {}
                }
            }),
        );

        let second_request: Value = read_json_frame(&mut stream);
        assert_eq!(second_request["request_id"], "0000000000000002");
        assert_eq!(second_request["operation"], first_request["operation"]);
        assert_eq!(second_request["payload"], first_request["payload"]);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000002",
                "status": "ok",
                "result": snapshot_result()
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let first_error: RuntimeClientError = client.snapshot_bag(5_000, 2_000).unwrap_err();
    match first_error {
        RuntimeClientError::Agent { error, .. } => {
            assert_eq!(error.retry, RetryDirective::SameRequest);
            assert_eq!(error.session_effect, SessionEffect::Unchanged);
        }
        other => panic!("应收到 agent 可重试错误，实际为 {other}"),
    }
    let snapshot = client
        .snapshot_bag(5_000, 2_000)
        .expect("同一逻辑读取的第二次请求应成功");
    assert!(snapshot.complete);
    assert_eq!(snapshot.count, 1);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 验证响应请求号不匹配时立即封闭当前会话。
fn response_request_id_mismatch_closes_session() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000002",
                "status": "ok",
                "result": health_result()
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let error: RuntimeClientError = client.health(5_000).unwrap_err();

    assert_eq!(error.code(), "request_id_mismatch");
    assert_eq!(
        client.health(5_000).unwrap_err().code(),
        "runtime_session_unusable"
    );
    server.join().expect("假 agent 不应 panic");
}

/// 已成功接收的业务响应密文不可在下一请求中重放，失败后当前连接永久封闭。
#[test]
fn replayed_protected_response_closes_session() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let first_request: Value = read_json_frame(&mut stream);
        let response = seal_json_frame(&json!({
            "protocol_version": 1,
            "request_id": first_request["request_id"],
            "status": "ok",
            "result": health_result()
        }));
        write_raw_frame(&mut stream, &response);
        let _: Value = read_json_frame(&mut stream);
        write_raw_frame(&mut stream, &response);
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    client.health(5_000).expect("首次业务响应应成功");
    let error = client.health(5_000).unwrap_err();
    assert_eq!(error.code(), "runtime_secure_channel_invalid");
    assert_eq!(
        client.health(5_000).unwrap_err().code(),
        "runtime_session_unusable"
    );
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// agent 要求关闭连接后，客户端保留原始错误并拒绝当前会话的后续请求。
fn must_close_agent_error_marks_session_unusable() {
    assert_terminal_session_effect_closes_session("must_close", SessionEffect::MustClose);
}

#[test]
/// 新 Agent 只可用 ready 原因开放已实现的装备写动作，旧只读状态仍保持兼容。
fn capabilities_allow_ready_equipment_write_operations() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": capabilities_result(true)
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let capabilities = client.capabilities(5_000).expect("装备写能力应通过校验");

    assert!(capabilities.capabilities["write.equip"].available);
    assert!(capabilities.capabilities["write.unequip"].available);
    assert!(capabilities.capabilities["write.destroy"].available);
    assert!(capabilities.capabilities["write.compose"].available);
    assert!(capabilities.capabilities["write.enhance"].available);
    server.join().expect("假 agent 不应 panic");
}

#[test]
/// 验证对端中断会报告读取阶段，并阻止会话继续发起请求。
fn dropped_connection_reports_read_stage_and_closes_session() {
    let (address, server) = spawn_server(|mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let error: RuntimeClientError = client.health(5_000).unwrap_err();

    assert_eq!(error.code(), "runtime_io_failed");
    assert!(matches!(
        error,
        RuntimeClientError::Io {
            stage: ClientStage::ReadResponse,
            ..
        }
    ));
    assert_eq!(
        client.health(5_000).unwrap_err().code(),
        "runtime_session_unusable"
    );
    server.join().expect("假 agent 不应 panic");
}

/// 构造会话终止类 agent 错误，并验证后续请求不会再次写入同一连接。
fn assert_terminal_session_effect_closes_session(
    session_effect: &'static str,
    expected_effect: SessionEffect,
) {
    let (address, server) = spawn_server(move |mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let request: Value = read_json_frame(&mut stream);
        assert_eq!(request["request_id"], "0000000000000001");
        assert_eq!(request["operation"], "health");
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "error",
                "error": {
                    "code": "fixture_session_terminal",
                    "stage": "agent.fixture",
                    "message": "固定会话终止错误",
                    "retry": "never",
                    "session_effect": session_effect,
                    "details": {}
                }
            }),
        );
    });

    let mut client: AgentClient = connect_client(address).expect("握手应成功");
    let first_error: RuntimeClientError = client.health(5_000).unwrap_err();
    match first_error {
        RuntimeClientError::Agent { error, .. } => {
            assert_eq!(error.code, "fixture_session_terminal");
            assert_eq!(error.session_effect, expected_effect);
        }
        other => panic!("应保留 agent 会话终止错误，实际为 {other}"),
    }
    assert_eq!(
        client.health(5_000).unwrap_err().code(),
        "runtime_session_unusable"
    );
    server.join().expect("假 agent 不应 panic");
}

/// 使用固定身份样本连接假 agent。
fn connect_client(address: SocketAddr) -> Result<AgentClient, RuntimeClientError> {
    connect_client_with_timeout(address, Duration::from_secs(2))
}

/// 使用固定身份样本和指定传输超时连接假 agent。
fn connect_client_with_timeout(
    address: SocketAddr,
    io_timeout: Duration,
) -> Result<AgentClient, RuntimeClientError> {
    let session_id: SessionId = SESSION_ID.parse().expect("会话样本应有效");
    let session_secret: SessionSecret = SESSION_SECRET.parse().expect("密钥样本应有效");
    let expected: ExpectedAgent =
        ExpectedAgent::new(session_id, PROCESS_ID, PACKAGE_NAME, RuntimeAbi::X86_64)
            .expect("目标样本应有效");
    AgentClient::connect(address, expected, session_secret, io_timeout)
}

/// 在临时环回端口启动一次性假 agent，并返回可等待的服务线程。
fn spawn_server(handler: impl FnOnce(TcpStream) + Send + 'static) -> (SocketAddr, JoinHandle<()>) {
    let listener: TcpListener = TcpListener::bind(("127.0.0.1", 0)).expect("应绑定环回端口");
    let address: SocketAddr = listener.local_addr().expect("应取得环回端口");
    let handle: JoinHandle<()> = thread::spawn(move || {
        let (stream, _peer): (TcpStream, SocketAddr) = listener.accept().expect("应接受宿主连接");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("应设置假 agent 读取超时");
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("应设置假 agent 写入超时");
        handler(stream);
    });
    (address, handle)
}

/// 核对宿主首帧并返回与预期目标一致的握手响应。
fn complete_handshake(stream: &mut TcpStream) {
    expect_handshake_proof(stream);
    write_json_frame(
        stream,
        &json!({
            "protocol_version": 1,
            "message_type": "handshake_ok",
            "session_id": SESSION_ID,
            "agent_version": "0.11.0",
            "process_id": PROCESS_ID,
            "package_name": PACKAGE_NAME,
            "abi": "x86_64"
        }),
    );
}

/// 先发送服务端证明，再核对宿主的固定长度客户端证明。
fn expect_handshake_proof(stream: &mut TcpStream) {
    let session_id: SessionId = SESSION_ID.parse().unwrap();
    let session_secret: SessionSecret = SESSION_SECRET.parse().unwrap();
    let mut session_id_bytes = [0; SESSION_ID_BYTES];
    let mut session_secret_bytes = [0; SECRET_BYTES];
    session_id.copy_bytes_to(&mut session_id_bytes);
    session_secret.copy_bytes_to(&mut session_secret_bytes);
    let server_nonce = [0x55; HANDSHAKE_NONCE_BYTES];
    let challenge =
        create_server_challenge(&session_id_bytes, &session_secret_bytes, &server_nonce);
    write_raw_frame(stream, challenge.as_bytes());
    let mut proof = read_raw_frame(stream);
    let keys = verify_client_proof(&challenge, &session_id_bytes, &session_secret_bytes, &proof)
        .expect("宿主应返回有效客户端证明");
    FIXTURE_SECURE_CHANNEL.with(|slot| {
        *slot.borrow_mut() = Some(keys.into_server_channel());
    });
    proof.fill(0);
    session_secret_bytes.fill(0);
}

/// 构造与固定握手身份一致的健康结果。
fn health_result() -> Value {
    json!({
        "agent_version": "0.11.0",
        "process_id": PROCESS_ID,
        "package_name": PACKAGE_NAME,
        "abi": "x86_64",
        "session_state": "ready",
        "main_thread_queue_ready": true,
        "catalog_generation": 1
    })
}

/// 构造与固定握手身份一致的完整 shutdown 排空收据。
fn shutdown_result() -> Value {
    json!({
        "state": "prepared",
        "session_id": SESSION_ID,
        "process_id": PROCESS_ID,
        "worker_tid": 13_900,
        "worker_start_time": 987_654,
        "hook_target": "000000007f013c80",
        "trampoline_start": "0000000080000000",
        "trampoline_size": 4096
    })
}
fn bag_item_result(item_id: u64) -> Value {
    json!({
        "item_id": item_id,
        "quantity": 37,
        "kind": "bag",
        "resolved_name": "物资样本",
        "compose_recipe": null
    })
}

/// 构造包含一条完整物品记录的成功背包快照结果。
fn snapshot_result() -> Value {
    json!({
        "schema_version": 1,
        "complete": true,
        "count": 1,
        "truncated": false,
        "items": [bag_item_result(20_001)],
        "read_errors": []
    })
}

/// 构造船坞、仓库、背包和玩家资源彼此一致的完整运行态结果。
fn owned_state_result() -> Value {
    json!({
        "schema_version": 3,
        "complete": true,
        "dock": {
            "complete": true,
            "count": 1,
            "truncated": false,
            "ships": [{
                "ship_id": 9001,
                "config_id": 101174,
                "level": 100,
                "experience_in_level": 3000000,
                "intimacy_raw": 10000,
                "energy": 150,
                "proficiency": 0,
                "fleet_memberships": [{
                    "fleet_id": 1,
                    "display_name": "第一舰队",
                    "kind": "regular",
                    "team": "vanguard",
                    "position": 1
                }],
                "skills": [{
                    "skill_id": 10410,
                    "level": 1,
                    "experience": 0
                }],
                "slots": [
                    {
                        "slot_index": 1,
                        "equipment": {
                            "equipment_id": 500,
                            "config_id": 500,
                            "enhance_level": 10
                        }
                    },
                    {"slot_index": 2, "equipment": null},
                    {"slot_index": 3, "equipment": null},
                    {"slot_index": 4, "equipment": null},
                    {"slot_index": 5, "equipment": null}
                ]
            }],
            "read_errors": []
        },
        "warehouse": {
            "complete": true,
            "count": 1,
            "truncated": false,
            "items": [{
                "equipment_id": 600,
                "config_id": 600,
                "quantity": 2,
                "enhance_level": 0
            }],
            "read_errors": []
        },
        "bag": snapshot_result(),
        "player": {
            "gold": 123456,
            "equipment_capacity": 2,
            "equipment_limit": 300
        }
    })
}

/// 构造与完整运行态中同一艘舰船严格对应的详情快照。
fn ship_details_result() -> Value {
    json!({
        "schema_version": 4,
        "complete": true,
        "count": 1,
        "truncated": false,
        "source": {"module_sha256": MODULE_SHA256},
        "ships": [{
            "ship_id": 9001,
            "config_id": 101174,
            "name": "测试舰船",
            "level": 100,
            "max_level": 125,
            "experience_in_level": 3000000,
            "total_experience": 4500000,
            "next_level_experience": 10000,
            "intimacy_raw": 10000,
            "intimacy_maximum": 200,
            "intimacy_stage_id": 5,
            "intimacy_stage_description": "爱",
            "proposed": false,
            "propose_time": 0,
            "create_time": 0,
            "combat_power": 4321,
            "locked": true,
            "oil_cost": {"start": 4, "end": 6, "total": 10},
            "classification": {
                "group_id": 10117,
                "ship_type_id": 1,
                "ship_type_name": "驱逐舰",
                "armor_type_id": 1,
                "armor_type_name": "轻型装甲",
                "nation_id": 1,
                "nation_name": "白鹰",
                "rarity": 4,
                "star": 5,
                "max_star": 6,
                "skin_id": 101170
            },
            "base_attributes": ship_attributes(100.0, 16.5),
            "equipment_applied_attributes": ship_attributes(125.0, 15.5),
            "effective_attributes": ship_attributes(132.0, 16.0),
            "slot_rules": [
                {"slot_index": 1, "allowed_equipment_type_ids": [1, 2]},
                {"slot_index": 2, "allowed_equipment_type_ids": [5, 10]},
                {"slot_index": 3, "allowed_equipment_type_ids": [6, 21]},
                {"slot_index": 4, "allowed_equipment_type_ids": [10]},
                {"slot_index": 5, "allowed_equipment_type_ids": [10]}
            ],
            "skills": [{
                "skill_id": 10410,
                "effective_skill_id": 10411,
                "name": "测试技能",
                "level": 1,
                "max_level": 10,
                "experience": 0,
                "next_level_experience": 100,
                "description_template": "技能描述模板",
                "current_effect": "当前效果"
            }]
        }],
        "read_errors": []
    })
}

/// 构造十一项字段齐全的属性阶段，只改变炮击和航速以验证小数与差额。
fn ship_attributes(cannon: f64, speed: f64) -> Value {
    json!({
        "durability": 1000.0,
        "cannon": cannon,
        "torpedo": 80.0,
        "anti_aircraft": 70.0,
        "air": 0.0,
        "reload": 120.0,
        "hit": 90.0,
        "dodge": 60.0,
        "anti_sub": 50.0,
        "luck": 45.0,
        "speed": speed
    })
}

/// 构造跨两页的装备目录首个完整页面。
fn equipment_config_page_result() -> Value {
    let config = |config_id: u64, root_config_id: u64, weapon_ids: Vec<u64>| {
        json!({
            "config_id": config_id,
            "root_config_id": root_config_id,
            "raw_config": {
                "id": config_id,
                "prev": 0,
                "trans_use_gold": 20,
                "trans_use_item": [[17001, 1]]
            },
            "attributes": [{"type": "cannon", "value": 12, "auxBoost": true}],
            "properties": {"equipmentType": 1},
            "skill": null,
            "property_rate": [],
            "weapon_ids": weapon_ids,
            "gear_score": 25,
            "anti_siren_power": null,
            "is_device": false,
            "is_aircraft": false,
            "complete": true,
            "read_errors": []
        })
    };
    json!({
        "schema_version": 1,
        "complete": true,
        "count": 2,
        "source": {"module_sha256": MODULE_SHA256},
        "start_index": 0,
        "total_count": 3,
        "next_index": 2,
        "configs": [config(500, 500, vec![1001, 1002]), config(501, 500, vec![])],
        "read_errors": []
    })
}
fn ship_catalog_page_result(table_key: &str) -> Value {
    json!({
        "table_key": table_key,
        "source": {"module_sha256": MODULE_SHA256},
        "start_index": 32,
        "total_count": 34,
        "next_index": null,
        "records": [
            {
                "id": 4001,
                "raw": {
                    "id": 4001,
                    "base": 0,
                    "name": "测试舰船",
                    "future_field": {"1": true}
                }
            },
            {
                "id": 4002,
                "raw": {
                    "id": 4002,
                    "base": 4001,
                    "name": "测试舰船改"
                }
            }
        ],
        "read_errors": [],
        "complete": true
    })
}

/// 通过真实客户端解码入口断言静态目录页以指定协议错误被拒绝。
fn assert_ship_catalog_result_rejected(
    result: Value,
    requested_table: ShipCatalogTableKey,
    expected_code: &str,
) {
    let (address, server) = spawn_server(move |mut stream: TcpStream| {
        complete_handshake(&mut stream);
        let _: Value = read_json_frame(&mut stream);
        write_json_frame(
            &mut stream,
            &json!({
                "protocol_version": 1,
                "request_id": "0000000000000001",
                "status": "ok",
                "result": result
            }),
        );
    });

    let mut client = connect_client(address).expect("握手应成功");
    let error = client
        .snapshot_ship_catalog(5_000, requested_table, 32, 2, MODULE_SHA256)
        .unwrap_err();
    assert_eq!(error.code(), expected_code);
    server.join().expect("假 agent 不应 panic");
}

/// 构造协议测试共用的合法卸装命令。
fn unequip_command() -> Result<RuntimeEquipmentCommand, RuntimeProtocolError> {
    let target = EquipmentCommandEquipment::new(500, 500, 10)?;
    let action = EquipmentCommandAction::unequip(9_001, 1, target, 2, 10, 300)?;
    RuntimeEquipmentCommand::new(
        COMMAND_ID,
        TARGET_SHA256,
        PLAN_SHA256,
        1,
        PRE_STATE_SHA256,
        action,
    )
}

/// 构造协议测试共用的合法仓库拆解命令。
fn dismantle_command() -> Result<RuntimeEquipmentCommand, RuntimeProtocolError> {
    let source = EquipmentCommandEquipment::new(700, 700, 0)?;
    let action = EquipmentCommandAction::dismantle(source, 3, 1, 10, 300)?;
    RuntimeEquipmentCommand::new(
        COMMAND_ID,
        TARGET_SHA256,
        PLAN_SHA256,
        1,
        PRE_STATE_SHA256,
        action,
    )
}

fn compose_command() -> Result<RuntimeEquipmentCommand, RuntimeProtocolError> {
    let output = EquipmentCommandEquipment::new(800, 1_000, 0)?;
    let action = EquipmentCommandAction::compose(
        2,
        2,
        1_000,
        Some(output),
        3,
        20_001,
        20,
        5,
        1_000,
        100,
        10,
        300,
    )?;
    RuntimeEquipmentCommand::new(
        COMMAND_ID,
        TARGET_SHA256,
        PLAN_SHA256,
        1,
        PRE_STATE_SHA256,
        action,
    )
}

/// 构造协议测试共用的合法仓库单级强化命令。
fn warehouse_enhance_command() -> Result<RuntimeEquipmentCommand, RuntimeProtocolError> {
    let source = EquipmentCommandEquipment::new(900, 1_000, 0)?;
    let target = EquipmentCommandEquipment::new(901, 1_001, 1)?;
    let materials = vec![
        EquipmentCommandMaterialCost::new(17_001, 10, 2)?,
        EquipmentCommandMaterialCost::new(17_002, 4, 1)?,
    ];
    let action = EquipmentCommandAction::enhance_warehouse(
        source,
        3,
        1_001,
        1,
        Some(target),
        2,
        materials,
        1_000,
        20,
        5,
        300,
    )?;
    RuntimeEquipmentCommand::new(
        COMMAND_ID,
        TARGET_SHA256,
        PLAN_SHA256,
        1,
        PRE_STATE_SHA256,
        action,
    )
}

/// 构造协议测试共用的合法舰上单级强化命令。
fn ship_enhance_command() -> Result<RuntimeEquipmentCommand, RuntimeProtocolError> {
    let source = EquipmentCommandEquipment::new(900, 1_000, 0)?;
    let materials = vec![
        EquipmentCommandMaterialCost::new(17_001, 10, 2)?,
        EquipmentCommandMaterialCost::new(17_002, 4, 1)?,
    ];
    let action = EquipmentCommandAction::enhance_ship(
        9_001, 1, source, 1_001, 1, materials, 1_000, 20, 5, 300,
    )?;
    RuntimeEquipmentCommand::new(
        COMMAND_ID,
        TARGET_SHA256,
        PLAN_SHA256,
        2,
        PRE_STATE_SHA256,
        action,
    )
}

/// 返回和请求号绑定的装备命令收据。
fn write_equipment_command_receipt(
    stream: &mut TcpStream,
    request_id: &str,
    status: &str,
    phase: &str,
    cancel_requested: bool,
) {
    write_json_frame(
        stream,
        &json!({
            "protocol_version": 1,
            "request_id": request_id,
            "status": "ok",
            "result": {
                "schema_version": 1,
                "command_id": COMMAND_ID,
                "status": status,
                "phase": phase,
                "write_dispatched": true,
                "cancel_requested": cancel_requested,
                "observation_count": 2,
                "error_code": null,
                "message": null
            }
        }),
    );
}

/// 构造完整能力集合，可定向开放已经登记的装备写能力。
fn capabilities_result(enable_equipment_write: bool) -> Value {
    json!({
        "capabilities": {
            "runtime.health": capability(true, "ready"),
            "runtime.main_thread_queue": capability(true, "ready"),
            "read.bag": capability(true, "ready"),
            "read.owned_state": capability(true, "ready"),
            "read.ship_details": capability(true, "ready"),
            "read.equipment_configs": capability(true, "ready"),
            "read.compose_recipes": capability(true, "ready"),
            "read.equipment_weapons": capability(true, "ready"),
            "read.skill_effects": capability(true, "ready"),
            "read.equipment_reference_names": capability(true, "ready"),
            "write.equip": capability(enable_equipment_write, if enable_equipment_write { "ready" } else { "mvp_read_only" }),
            "write.unequip": capability(enable_equipment_write, if enable_equipment_write { "ready" } else { "mvp_read_only" }),
            "write.compose": capability(enable_equipment_write, if enable_equipment_write { "ready" } else { "mvp_read_only" }),
            "write.enhance": capability(enable_equipment_write, if enable_equipment_write { "ready" } else { "mvp_read_only" }),
            "write.destroy": capability(enable_equipment_write, if enable_equipment_write { "ready" } else { "mvp_read_only" })
        }
    })
}

/// 构造带固定契约证据的单项能力状态。
fn capability(available: bool, reason_code: &str) -> Value {
    json!({
        "available": available,
        "reason_code": reason_code,
        "evidence": ["contract_fixture"]
    })
}

/// 读取宿主发送的长度前缀 JSON 帧。
fn read_json_frame(stream: &mut TcpStream) -> Value {
    let protected = read_raw_frame(stream);
    assert!(
        protected.len() <= FIXTURE_MAX_PLAINTEXT_BYTES + PROTECTED_OVERHEAD_BYTES,
        "宿主受保护帧超过 fixture 上限"
    );
    FIXTURE_SECURE_CHANNEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let channel = slot.as_mut().expect("读取业务帧前必须完成挑战证明");
        let plaintext = channel
            .inbound
            .open(&protected, FIXTURE_MAX_PLAINTEXT_BYTES)
            .expect("宿主应发送有效有序认证帧");
        serde_json::from_slice(&plaintext).expect("宿主应发送有效 JSON")
    })
}

/// 读取四字节大端长度前缀帧，不对二进制正文作 JSON 假设。
fn read_raw_frame(stream: &mut TcpStream) -> Vec<u8> {
    let mut header: [u8; 4] = [0; 4];
    stream.read_exact(&mut header).expect("应读取完整帧头");
    let length: usize = u32::from_be_bytes(header) as usize;
    let mut payload: Vec<u8> = vec![0; length];
    stream.read_exact(&mut payload).expect("应读取完整帧体");
    payload
}

/// 将 JSON 样本编码后发送给宿主客户端。
fn write_json_frame(stream: &mut TcpStream, value: &Value) {
    let payload: Vec<u8> = serde_json::to_vec(value).expect("fixture 应可编码");
    write_raw_json_frame(stream, &payload);
}

/// 发送原始 JSON 字节，用于保留重复字段等非常规测试样本。
fn write_raw_json_frame(stream: &mut TcpStream, payload: &[u8]) {
    let frame = seal_plaintext_frame(payload);
    write_raw_frame(stream, &frame);
}

/// 序列化并密封 fixture JSON，但将发送时机留给篡改和重放测试。
fn seal_json_frame(value: &Value) -> Vec<u8> {
    let payload = serde_json::to_vec(value).expect("fixture 应可编码");
    seal_plaintext_frame(&payload)
}

/// 使用假 Agent 当前发送序号密封任意已编码 JSON 明文。
fn seal_plaintext_frame(payload: &[u8]) -> Vec<u8> {
    FIXTURE_SECURE_CHANNEL.with(|slot| {
        let mut slot = slot.borrow_mut();
        let channel = slot.as_mut().expect("写入业务帧前必须完成挑战证明");
        channel
            .outbound
            .seal(payload, FIXTURE_MAX_PLAINTEXT_BYTES)
            .expect("fixture JSON 应可密封")
    })
}

/// 发送任意二进制帧，供挑战证明和非常规 JSON 样本共用。
fn write_raw_frame(stream: &mut TcpStream, payload: &[u8]) {
    let length: u32 = u32::try_from(payload.len()).expect("fixture 帧应小于 u32 上限");
    stream
        .write_all(&length.to_be_bytes())
        .and_then(|()| stream.write_all(payload))
        .expect("应发送完整 fixture 帧");
}

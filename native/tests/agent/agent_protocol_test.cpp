// 验证 agent 严格请求解析、稳定错误和确定性响应编码契约。

#include <algorithm>
#include <array>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>

#include "bootstrap_config.h"
#include "bootstrap_validation.h"
#include "protocol/handshake.h"
#include "protocol/json_codec.h"
#include "protocol/protocol_types.h"
#include "runtime_rpc_contract_data.h"
#include "snapshots/owned_state_snapshot.h"

namespace {

/// 以明确消息终止首个失败断言。
void require(bool condition, const char* message) {
    if (!condition) {
        std::cerr << "FAILED: " << message << std::endl;
        std::exit(1);
    }
}

/// 取出当前请求中的具体操作载荷。
template <typename Body>
const Body* rpc_body(const azlw::agent::RpcRequest& request) {
    return std::get_if<Body>(&request.body);
}

/// 执行请求成功时返回其中的装备命令。
const azlw::agent::EquipmentCommand* executed_equipment_command(
    const azlw::agent::RpcRequest& request) {
    const auto* body = rpc_body<azlw::agent::ExecuteEquipmentCommandRpcRequest>(request);
    return body == nullptr ? nullptr : &body->command;
}

/// 构造协议测试使用的完整有效启动配置。
azlw::BootstrapConfigV2 make_config() {
    azlw::BootstrapConfigV2 config{};
    std::memcpy(config.magic, azlw::kBootstrapMagic, sizeof(config.magic));
    config.schema_version = azlw::kBootstrapVersion;
    config.total_size = sizeof(config);
    config.target_pid = 13'882;
    config.timeout_ms = 5'000;
    for (std::size_t index = 0; index < azlw::kSessionIdSize; ++index) {
        config.session_id[index] = static_cast<std::uint8_t>(index);
    }
    for (std::size_t index = 0; index < azlw::kSessionSecretSize; ++index) {
        config.session_secret[index] = static_cast<std::uint8_t>(index);
    }
    for (std::size_t index = 0; index < azlw::kSessionIdSize; ++index) {
        config.channel_id[index] = static_cast<std::uint8_t>(index + 16);
        config.mapping_id[index] = static_cast<std::uint8_t>(index + 32);
    }
    std::strcpy(config.package_name, "com.bilibili.azurlane");
    std::strcpy(config.module_name, "libtolua.so");
    std::memset(config.module_sha256, 'a', 64);
    config.target_symbol_offset = 0x13c80;
    return config;
}

/// 构造同时绑定进程、映射、Hook 和独立 RPC 载体的有效卸载配置。
azlw::AgentUnloadConfigV1 make_unload_config() {
    azlw::AgentUnloadConfigV1 config{};
    std::memcpy(config.magic, azlw::kUnloadMagic, sizeof(config.magic));
    config.schema_version = azlw::kUnloadVersion;
    config.total_size = sizeof(config);
    config.target_pid = 13'882;
    config.timeout_ms = 5'000;
    config.session_id[0] = 1;
    config.process_start_time = 123'456;
    std::strcpy(config.package_name, "com.bilibili.azurlane");
    std::strcpy(config.module_name, "libtolua.so");
    std::memset(config.module_sha256, 'a', 64);
    std::memset(config.agent_sha256, 'b', 64);
    std::strcpy(config.agent_mapping_name, "/memfd:azlw-agent-test (deleted)");
    config.target_symbol_offset = 0x13c80;
    config.agent_handle = 0x1000;
    config.agent_base = 0x7f0010000000;
    config.agent_load_size = 0x20'000;
    config.finalize_address = config.agent_base + 0x1000;
    config.hook_target = 0x7f1010000000;
    config.trampoline_start = 0x7f2010000000;
    config.trampoline_size = 4096;
    config.rpc_worker_tid = 13'900;
    config.rpc_worker_start_time = 123'500;
    return config;
}

}  // namespace

/// 覆盖握手、请求边界、未知操作和成功响应的冻结格式。
void test_owned_query_protocol() {
    using namespace azlw::agent;
    auto parse = [](std::string payload, RpcRequest* request) {
        bool can_respond = false;
        AgentError error;
        const std::string frame = "{\"protocol_version\":1,\"request_id\":\"0000000000000001\",\"operation\":\"query_owned\",\"timeout_ms\":5000,\"payload\":" + payload + "}";
        return parse_rpc_request(frame, request, &can_respond, &error);
    };
    RpcRequest request;
    require(parse(R"({"kind":"ships","ids":[7,8],"fields":["name","skills"]})", &request), "query_owned should accept explicit ship selectors");
    require(std::get<QueryOwnedRpcRequest>(request.body).query.ids.size() == 2, "query_owned should retain ids");
    require(parse(R"({"kind":"equipment","ids":[],"fields":[]})", &request), "query_owned should accept all equipment");
    OwnedQueryExecution execution;
    execution.query = {.kind = "ships", .ids = {7, 8}, .fields = {"level"}};
    OwnedQueryShip entry;
    entry.ship.ship_id = 7;
    entry.ship.config_id = 100;
    entry.ship.level = 5;
    execution.ships.push_back(entry);
    execution.missing_ids.push_back(8);
    const auto encoded = encode_owned_query("0000000000000001", execution);
    require(encoded.find(R"("entries":[{"ship_id":7,"config_id":100,"level":5}],"missing_ids":[8])") != std::string::npos,
            "query_owned should encode selected fields and identity only");

    for (const char* payload : {
        R"({"kind":"bag","ids":[],"fields":[]})",
        R"({"kind":"ships","ids":[0],"fields":[]})",
        R"({"kind":"ships","ids":[9007199254740992],"fields":[]})",
        R"({"kind":"ships","ids":[7,7],"fields":[]})",
        R"({"kind":"ships","ids":[],"fields":["unknown"]})",
        R"({"kind":"equipment","ids":[],"fields":["skills"]})",
        R"({"kind":"ships","ids":[],"fields":["level","level"]})",
        R"({"kind":"ships","ids":[],"fields":[],"extra":1})",
        R"({"kind":"ships","kind":"ships","ids":[],"fields":[]})",
        R"({"kind":"ships","ids":[]})"
    }) require(!parse(payload, &request), "query_owned must reject invalid selectors");
}

int main() {
    test_owned_query_protocol();
    using namespace azlw::agent;

    static_assert(kProtocolVersion == azlw::runtime_rpc_contract::kProtocolVersion);
    static_assert(std::string_view(kAgentVersion) == azlw::runtime_rpc_contract::kAgentVersion);
    static_assert(azlw::kMinimumTimeoutMs == azlw::runtime_rpc_contract::kMinimumTimeoutMs);
    static_assert(azlw::kMaximumTimeoutMs == azlw::runtime_rpc_contract::kMaximumTimeoutMs);
    static_assert(kMaximumRequestBytes == azlw::runtime_rpc_contract::kMaximumRequestBytes);
    static_assert(kMaximumResponseBytes == azlw::runtime_rpc_contract::kMaximumResponseBytes);
    static_assert(kMaximumSnapshotItems == azlw::runtime_rpc_contract::kMaximumSnapshotItems);
    static_assert(
        kMaximumEquipmentPageSize == azlw::runtime_rpc_contract::kMaximumEquipmentPageSize);
    static_assert(
        kMaximumEquipmentCatalogItems ==
        azlw::runtime_rpc_contract::kMaximumEquipmentCatalogItems);
    static_assert(
        kMaximumShipCatalogPageSize ==
        azlw::runtime_rpc_contract::kMaximumShipCatalogPageSize);
    static_assert(
        kMaximumShipCatalogItems == azlw::runtime_rpc_contract::kMaximumShipCatalogItems);
    static_assert(
        kMaximumEquipmentWeaponBatchSize ==
        azlw::runtime_rpc_contract::kMaximumEquipmentWeaponBatchSize);
    static_assert(
        kMaximumSkillEffectBatchSize == azlw::runtime_rpc_contract::kMaximumSkillEffectBatchSize);
    static_assert(
        kMaximumEquipmentReferenceBatchSize ==
        azlw::runtime_rpc_contract::kMaximumEquipmentReferenceBatchSize);
    static_assert(
        kShipEquipmentSlotCount == azlw::runtime_rpc_contract::kShipEquipmentSlotCount);
    static_assert(
        kMaximumShipSlotEquipmentTypeCount ==
        azlw::runtime_rpc_contract::kMaximumShipSlotEquipmentTypeCount);
    static_assert(
        kMaximumShipSkillCount == azlw::runtime_rpc_contract::kMaximumShipSkillCount);
    static_assert(
        kMaximumFleetTeamShipCount == azlw::runtime_rpc_contract::kMaximumFleetTeamShipCount);
    static_assert(
        kMaximumShipFleetMembershipCount ==
        azlw::runtime_rpc_contract::kMaximumShipFleetMembershipCount);
    static_assert(
        kMaximumEnhanceMaterialCount == azlw::runtime_rpc_contract::kMaximumEnhanceMaterialCount);

    const azlw::BootstrapConfigV2 config = make_config();
    std::string config_error;
    require(
        azlw::validate_bootstrap_config(config, &config_error),
        "valid memfd bootstrap config must parse");
    auto anonymous_config = config;
    anonymous_config.agent_runtime_policy = azlw::make_agent_runtime_policy(
        azlw::AgentMappingMode::AnonymousRemap,
        azlw::AgentVisibilityMode::Normal);
    require(
        azlw::validate_bootstrap_config(anonymous_config, &config_error),
        "valid anonymous bootstrap config must parse");
    auto invalid_mapping_config = config;
    invalid_mapping_config.agent_runtime_policy.value = 0xff;
    require(
        !azlw::validate_bootstrap_config(invalid_mapping_config, &config_error),
        "unknown bootstrap mapping mode must be rejected");
    require(
        azlw::bootstrap_session_id_hex(config) == "000102030405060708090a0b0c0d0e0f",
        "session id text mismatch");
    require(
        azlw::bootstrap_channel_id_hex(config) == "101112131415161718191a1b1c1d1e1f",
        "channel id text mismatch");
    require(
        azlw::bootstrap_mapping_id_hex(config) == "202122232425262728292a2b2c2d2e2f",
        "mapping id text mismatch");
    azlw::AgentUnloadConfigV1 unload_config = make_unload_config();
    std::string unload_error;
    require(
        azlw::validate_unload_config(unload_config, &unload_error),
        "valid unload config must parse");
    auto anonymous_unload_config = unload_config;
    anonymous_unload_config.agent_runtime_policy = azlw::make_agent_runtime_policy(
        azlw::AgentMappingMode::AnonymousRemap,
        azlw::AgentVisibilityMode::Normal);
    require(
        azlw::validate_unload_config(anonymous_unload_config, &unload_error),
        "valid anonymous unload config must parse");
    auto hidden_unload_config = unload_config;
    hidden_unload_config.agent_runtime_policy = azlw::make_agent_runtime_policy(
        azlw::AgentMappingMode::AnonymousRemap,
        azlw::AgentVisibilityMode::SolistHidden);
    hidden_unload_config.agent_soinfo_address = 0x770000;
    require(
        azlw::validate_unload_config(hidden_unload_config, &unload_error),
        "valid hidden unload config must parse");
    hidden_unload_config.agent_soinfo_address = 0;
    require(
        !azlw::validate_unload_config(hidden_unload_config, &unload_error),
        "hidden unload config without soinfo must be rejected");
    auto normal_with_soinfo = unload_config;
    normal_with_soinfo.agent_soinfo_address = 0x770000;
    require(
        !azlw::validate_unload_config(normal_with_soinfo, &unload_error),
        "normal unload config with soinfo must be rejected");
    auto protected_unload_config = unload_config;
    protected_unload_config.agent_runtime_policy = azlw::make_agent_runtime_policy(
        azlw::AgentMappingMode::AnonymousRemap,
        azlw::AgentVisibilityMode::SolistAndElfHeader);
    protected_unload_config.agent_soinfo_address = 0x770000;
    for (std::size_t index = 0; index < sizeof(protected_unload_config.protected_elf_header);
         ++index) {
        protected_unload_config.protected_elf_header[index] =
            static_cast<std::uint8_t>(index + 1);
    }
    require(
        azlw::validate_unload_config(protected_unload_config, &unload_error),
        "valid protected ELF header unload config must parse");
    std::fill(
        std::begin(protected_unload_config.protected_elf_header),
        std::end(protected_unload_config.protected_elf_header),
        0);
    require(
        !azlw::validate_unload_config(protected_unload_config, &unload_error),
        "protected ELF header mode without evidence must be rejected");
    hidden_unload_config.agent_soinfo_address = 0x770000;
    hidden_unload_config.protected_elf_header[0] = 1;
    require(
        !azlw::validate_unload_config(hidden_unload_config, &unload_error),
        "solist-only mode with ELF header evidence must be rejected");
    auto invalid_mapping_unload_config = unload_config;
    invalid_mapping_unload_config.agent_runtime_policy.value = 0xff;
    require(
        !azlw::validate_unload_config(invalid_mapping_unload_config, &unload_error),
        "unknown unload mapping mode must be rejected");
    unload_config.rpc_worker_tid = unload_config.target_pid;
    require(
        !azlw::validate_unload_config(unload_config, &unload_error),
        "unload carrier must not be the process main thread");
    azlw::secure_channel::HandshakeNonce server_nonce{};
    azlw::secure_channel::HandshakeNonce client_nonce{};
    for (std::size_t index = 0; index < server_nonce.size(); ++index) {
        server_nonce[index] = static_cast<std::uint8_t>(index + 32);
        client_nonce[index] = static_cast<std::uint8_t>(index + 64);
    }
    std::string parse_error;
    azlw::secure_channel::ServerChallenge challenge;
    require(
        create_handshake_challenge(config, server_nonce, &challenge, &parse_error),
        "valid server challenge must encode");
    azlw::secure_channel::SessionId session_id{};
    azlw::secure_channel::SessionSecret session_secret{};
    std::copy_n(config.session_id, session_id.size(), session_id.begin());
    std::copy_n(config.session_secret, session_secret.size(), session_secret.begin());
    azlw::secure_channel::VerifiedServerChallenge verified;
    require(
        azlw::secure_channel::verify_server_challenge(
            session_id,
            session_secret,
            challenge.bytes(),
            &verified) == azlw::secure_channel::Error::Ok,
        "server challenge proof must verify");
    azlw::secure_channel::ClientProofBytes proof{};
    azlw::secure_channel::ChannelKeyMaterial client_keys;
    require(
        azlw::secure_channel::answer_server_challenge(
            verified,
            session_secret,
            client_nonce,
            &proof,
            &client_keys) == azlw::secure_channel::Error::Ok,
        "valid client proof must encode");
    azlw::secure_channel::ChannelKeyMaterial server_keys;
    require(
        verify_handshake_proof(challenge, proof, config, &server_keys, &parse_error),
        "valid client proof must verify");
    require(
        server_keys.host_to_agent_key() == client_keys.host_to_agent_key(),
        "both handshake sides must derive the same keys");

    auto invalid_proof = proof;
    invalid_proof.back() ^= 1;
    require(
        !verify_handshake_proof(challenge, invalid_proof, config, &server_keys, &parse_error),
        "modified client proof must fail");
    require(
        !verify_handshake_proof(
            challenge,
            std::span<const std::uint8_t>(proof).first(proof.size() - 1),
            config,
            &server_keys,
            &parse_error),
        "truncated client proof must fail");
    std::array<std::uint8_t, azlw::secure_channel_contract::kClientProofBytes + 1>
        oversized_proof{};
    std::copy(proof.begin(), proof.end(), oversized_proof.begin());
    require(
        !verify_handshake_proof(
            challenge,
            oversized_proof,
            config,
            &server_keys,
            &parse_error),
        "oversized client proof must fail");

    RpcRequest request;
    AgentError error;
    bool can_respond = false;
    const std::string snapshot_request = azlw::test_contract::kSnapshotBagRequest;
    require(
        parse_rpc_request(snapshot_request, &request, &can_respond, &error),
        "valid snapshot request must parse");
    require(can_respond && request.request_number == 1, "request id must decode as big-endian hex");
    const auto* bag = rpc_body<SnapshotBagRpcRequest>(request);
    require(
        bag != nullptr && bag->max_items == 2'000 && bag->timeout_ms == 5'000,
        "snapshot payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string owned_state_request =
        R"({"protocol_version":1,"request_id":"0000000000000002","operation":"snapshot_owned_state","timeout_ms":5000,"payload":{"max_ships":500,"max_equipments":1000,"max_items":2000}})";
    require(
        parse_rpc_request(owned_state_request, &request, &can_respond, &error),
        "valid owned-state request must parse");
    require(
        rpc_body<SnapshotOwnedStateRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotOwnedStateRpcRequest>(request)->max_ships == 500 &&
            rpc_body<SnapshotOwnedStateRpcRequest>(request)->max_equipments == 1'000 &&
            rpc_body<SnapshotOwnedStateRpcRequest>(request)->max_items == 2'000,
        "owned-state payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string ship_details_request =
        R"({"protocol_version":1,"request_id":"0000000000000003","operation":"snapshot_ship_details","timeout_ms":5000,"payload":{"max_ships":839}})";
    require(
        parse_rpc_request(ship_details_request, &request, &can_respond, &error),
        "valid ship-details request must parse");
    require(
        rpc_body<SnapshotShipDetailsRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotShipDetailsRpcRequest>(request)->max_ships == 839,
        "ship-details payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string ship_catalog_request =
        R"({"protocol_version":1,"request_id":"0000000000000004","operation":"snapshot_ship_catalog","timeout_ms":5000,"payload":{"table_key":"ship_data_statistics","start_index":32,"page_size":32}})";
    require(
        parse_rpc_request(ship_catalog_request, &request, &can_respond, &error),
        "valid ship-catalog request must parse");
    require(
        rpc_body<SnapshotShipCatalogRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotShipCatalogRpcRequest>(request)->table_key ==
                "ship_data_statistics" &&
            rpc_body<SnapshotShipCatalogRpcRequest>(request)->start_index == 32 &&
            rpc_body<SnapshotShipCatalogRpcRequest>(request)->page_size == 32,
        "ship-catalog page payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string equipment_config_request =
        R"({"protocol_version":1,"request_id":"0000000000000004","operation":"snapshot_equipment_configs","timeout_ms":5000,"payload":{"start_index":0,"page_size":250}})";
    require(
        parse_rpc_request(equipment_config_request, &request, &can_respond, &error),
        "valid equipment-config request must parse");
    require(
        rpc_body<SnapshotEquipmentConfigsRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotEquipmentConfigsRpcRequest>(request)->start_index == 0 &&
            rpc_body<SnapshotEquipmentConfigsRpcRequest>(request)->page_size == 250,
        "equipment-config page payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string compose_recipe_request =
        R"({"protocol_version":1,"request_id":"0000000000000005","operation":"snapshot_compose_recipes","timeout_ms":5000,"payload":{"start_index":250,"page_size":77}})";
    require(
        parse_rpc_request(compose_recipe_request, &request, &can_respond, &error),
        "valid compose-recipe request must parse");
    require(
        rpc_body<SnapshotComposeRecipesRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotComposeRecipesRpcRequest>(request)->start_index == 250 &&
            rpc_body<SnapshotComposeRecipesRpcRequest>(request)->page_size == 77,
        "compose-recipe page payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string equipment_weapon_request =
        R"({"protocol_version":1,"request_id":"0000000000000006","operation":"snapshot_equipment_weapons","timeout_ms":5000,"payload":{"weapon_ids":[1001,1002]}})";
    require(
        parse_rpc_request(equipment_weapon_request, &request, &can_respond, &error),
        "valid equipment-weapon request must parse");
    require(
        rpc_body<SnapshotEquipmentWeaponsRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotEquipmentWeaponsRpcRequest>(request)->weapon_ids ==
                std::vector<std::uint64_t>({1'001, 1'002}),
        "equipment-weapon batch payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string skill_effect_request =
        R"({"protocol_version":1,"request_id":"0000000000000007","operation":"snapshot_skill_effects","timeout_ms":5000,"payload":{"skills":[{"skill_id":60720,"level":1},{"skill_id":60720,"level":10},{"skill_id":60721,"level":1}]}})";
    require(
        parse_rpc_request(skill_effect_request, &request, &can_respond, &error),
        "valid skill-effect request must parse");
    require(
        rpc_body<SnapshotSkillEffectsRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotSkillEffectsRpcRequest>(request)->skills.size() == 3 &&
            rpc_body<SnapshotSkillEffectsRpcRequest>(request)->skills[1].skill_id == 60'720 &&
            rpc_body<SnapshotSkillEffectsRpcRequest>(request)->skills[1].level == 10,
        "skill-effect batch payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string equipment_reference_request =
        R"({"protocol_version":1,"request_id":"0000000000000008","operation":"snapshot_equipment_reference_names","timeout_ms":5000,"payload":{"equipment_type_ids":[4,10],"nation_ids":[0,1,3],"ship_type_ids":[4,5,10],"attribute_keys":["cannon","reload"]}})";
    require(
        parse_rpc_request(equipment_reference_request, &request, &can_respond, &error),
        "valid equipment-reference request must parse");
    require(
        rpc_body<SnapshotEquipmentReferenceNamesRpcRequest>(request) != nullptr &&
            rpc_body<SnapshotEquipmentReferenceNamesRpcRequest>(request)->equipment_type_ids ==
                std::vector<std::uint64_t>({4, 10}) &&
            rpc_body<SnapshotEquipmentReferenceNamesRpcRequest>(request)->nation_ids ==
                std::vector<std::uint64_t>({0, 1, 3}) &&
            rpc_body<SnapshotEquipmentReferenceNamesRpcRequest>(request)->ship_type_ids ==
                std::vector<std::uint64_t>({4, 5, 10}) &&
            rpc_body<SnapshotEquipmentReferenceNamesRpcRequest>(request)->attribute_keys ==
                std::vector<std::string>({"cannon", "reload"}),
        "equipment-reference batch payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string unequip_command_request =
        R"({"protocol_version":1,"request_id":"0000000000000009","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":1,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"unequip","ship_id":9001,"slot_index":1,"target_before":{"equipment_id":500,"config_id":500,"enhance_level":10},"target_warehouse_quantity_before":2,"equipment_capacity_before":10,"equipment_limit_before":300}}})";
    require(
        parse_rpc_request(unequip_command_request, &request, &can_respond, &error),
        "valid unequip command must parse");
    const auto* unequip_command = executed_equipment_command(request);
    const auto* unequip_action = unequip_command == nullptr
                                     ? nullptr
                                     : std::get_if<UnequipEquipmentCommandAction>(
                                           &unequip_command->action);
    require(
        unequip_action != nullptr && unequip_action->ship_id == 9'001 &&
            unequip_action->target_before.enhance_level == 10,
        "unequip command payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string unknown_action_field_request = unequip_command_request;
    const std::size_t action_end = unknown_action_field_request.rfind("}}}");
    require(action_end != std::string::npos, "unequip fixture action boundary is missing");
    unknown_action_field_request.insert(
        action_end,
        R"(,"unexpected":true)");
    require(
        !parse_rpc_request(unknown_action_field_request, &request, &can_respond, &error) &&
            error.code == "unknown_field",
        "equipment command action must reject unknown fields");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string uppercase_command_id_request = unequip_command_request;
    const std::size_t command_id_start =
        uppercase_command_id_request.find(std::string(64, 'a'));
    require(command_id_start != std::string::npos, "unequip fixture command_id is missing");
    uppercase_command_id_request[command_id_start] = 'A';
    require(
        !parse_rpc_request(uppercase_command_id_request, &request, &can_respond, &error) &&
            error.code == "equipment_command_metadata_invalid",
        "equipment command must reject non-canonical hashes");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string equip_command_request =
        R"({"protocol_version":1,"request_id":"000000000000000a","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":2,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"equip","ship_id":9001,"slot_index":1,"target_before":null,"source_before":{"equipment_id":500,"config_id":500,"enhance_level":10},"source_quantity_before":3,"target_warehouse_quantity_before":0,"equipment_capacity_before":11,"equipment_limit_before":300}}})";
    require(
        parse_rpc_request(equip_command_request, &request, &can_respond, &error),
        "valid equip command must parse");
    const auto* equip_command = executed_equipment_command(request);
    const auto* equip_action = equip_command == nullptr
                                   ? nullptr
                                   : std::get_if<EquipEquipmentCommandAction>(&equip_command->action);
    require(
        equip_action != nullptr && !equip_action->target_before.has_value() &&
            equip_action->source_quantity_before == 3,
        "equip command payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string dismantle_command_request =
        R"({"protocol_version":1,"request_id":"0000000000000010","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":3,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"dismantle","source_before":{"equipment_id":700,"config_id":700,"enhance_level":0},"source_quantity_before":5,"dismantle_quantity":2,"equipment_capacity_before":11,"equipment_limit_before":300}}})";
    require(
        parse_rpc_request(dismantle_command_request, &request, &can_respond, &error),
        "valid dismantle command must parse");
    const auto* dismantle_command = executed_equipment_command(request);
    const auto* dismantle_action =
        dismantle_command == nullptr
            ? nullptr
            : std::get_if<DismantleEquipmentCommandAction>(&dismantle_command->action);
    require(
        dismantle_action != nullptr && dismantle_action->source_before.equipment_id == 700 &&
            dismantle_action->source_quantity_before == 5 &&
            dismantle_action->dismantle_quantity == 2,
        "dismantle command payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string dismantle_with_ship = dismantle_command_request;
    const std::size_t dismantle_kind_end =
        dismantle_with_ship.find(R"("kind":"dismantle")");
    require(dismantle_kind_end != std::string::npos, "dismantle fixture kind is missing");
    dismantle_with_ship.insert(
        dismantle_kind_end + std::string(R"("kind":"dismantle")").size(),
        R"(,"ship_id":9001)");
    require(
        !parse_rpc_request(dismantle_with_ship, &request, &can_respond, &error) &&
            error.code == "equipment_command_action_invalid",
        "dismantle must reject ship-only action fields");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string enhanced_dismantle = dismantle_command_request;
    const std::size_t enhance_level = enhanced_dismantle.find(R"("enhance_level":0)");
    require(enhance_level != std::string::npos, "dismantle fixture enhance level is missing");
    enhanced_dismantle.replace(
        enhance_level,
        std::string(R"("enhance_level":0)").size(),
        R"("enhance_level":1)");
    require(
        !parse_rpc_request(enhanced_dismantle, &request, &can_respond, &error) &&
            error.code == "equipment_command_precondition_invalid",
        "dismantle must reject enhanced equipment before dispatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string compose_command_request =
        R"({"protocol_version":1,"request_id":"0000000000000011","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"9999999999999999999999999999999999999999999999999999999999999999","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":4,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"compose","recipe_id":2,"compose_quantity":2,"output_config_id":1000,"output_before":{"equipment_id":800,"config_id":1000,"enhance_level":0},"output_quantity_before":3,"material_id":20001,"material_quantity_before":20,"material_quantity_per_unit":5,"gold_before":1000,"gold_per_unit":100,"equipment_capacity_before":10,"equipment_limit_before":300}}})";
    require(
        parse_rpc_request(compose_command_request, &request, &can_respond, &error),
        "valid compose command must parse");
    const auto* compose_command = executed_equipment_command(request);
    const auto* compose_action = compose_command == nullptr
                                     ? nullptr
                                     : std::get_if<ComposeEquipmentCommandAction>(
                                           &compose_command->action);
    require(
        compose_action != nullptr && compose_action->recipe_id == 2 &&
            compose_action->compose_quantity == 2 &&
            compose_action->material_quantity_before == 20 &&
            compose_action->output_before.has_value() &&
            compose_action->output_before->config_id == 1000,
        "compose command payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string insufficient_compose = compose_command_request;
    const std::size_t material_before =
        insufficient_compose.find(R"("material_quantity_before":20)");
    require(material_before != std::string::npos, "compose fixture material quantity is missing");
    insufficient_compose.replace(
        material_before,
        std::string(R"("material_quantity_before":20)").size(),
        R"("material_quantity_before":9)");
    require(
        !parse_rpc_request(insufficient_compose, &request, &can_respond, &error) &&
            error.code == "equipment_command_precondition_invalid",
        "compose must reject insufficient material before dispatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string enhance_warehouse_command_request =
        R"({"protocol_version":1,"request_id":"0000000000000012","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"8888888888888888888888888888888888888888888888888888888888888888","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":5,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"enhance_warehouse","source_before":{"equipment_id":900,"config_id":1000,"enhance_level":0},"source_quantity_before":3,"target_config_id":1001,"target_enhance_level":1,"target_before":{"equipment_id":901,"config_id":1001,"enhance_level":1},"target_warehouse_quantity_before":2,"materials":[{"item_id":17001,"quantity_before":10,"cost":2},{"item_id":17002,"quantity_before":4,"cost":1}],"gold_before":1000,"gold_cost":20,"equipment_capacity_before":5,"equipment_limit_before":300}}})";
    require(
        parse_rpc_request(
            enhance_warehouse_command_request,
            &request,
            &can_respond,
            &error),
        "valid warehouse enhance command must parse");
    const auto* enhance_warehouse_command = executed_equipment_command(request);
    const auto* enhance_warehouse_action =
        enhance_warehouse_command == nullptr
            ? nullptr
            : std::get_if<EnhanceWarehouseEquipmentCommandAction>(
                  &enhance_warehouse_command->action);
    require(
        enhance_warehouse_action != nullptr &&
            enhance_warehouse_action->source_before.config_id == 1'000 &&
            enhance_warehouse_action->target_config_id == 1'001 &&
            enhance_warehouse_action->target_before.has_value() &&
            enhance_warehouse_action->materials.size() == 2 &&
            enhance_warehouse_action->materials[1].cost == 1 &&
            enhance_warehouse_action->gold_cost == 20,
        "warehouse enhance command payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string enhance_ship_command_request =
        R"({"protocol_version":1,"request_id":"0000000000000013","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"7777777777777777777777777777777777777777777777777777777777777777","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":6,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"enhance_ship","ship_id":9001,"slot_index":1,"source_before":{"equipment_id":900,"config_id":1000,"enhance_level":0},"target_config_id":1001,"target_enhance_level":1,"materials":[{"item_id":17001,"quantity_before":10,"cost":2}],"gold_before":1000,"gold_cost":20,"equipment_capacity_before":5,"equipment_limit_before":300}}})";
    require(
        parse_rpc_request(enhance_ship_command_request, &request, &can_respond, &error),
        "valid ship enhance command must parse");
    const auto* enhance_ship_command = executed_equipment_command(request);
    const auto* enhance_ship_action =
        enhance_ship_command == nullptr
            ? nullptr
            : std::get_if<EnhanceShipEquipmentCommandAction>(&enhance_ship_command->action);
    require(
        enhance_ship_action != nullptr && enhance_ship_action->ship_id == 9'001 &&
            enhance_ship_action->slot_index == 1 &&
            enhance_ship_action->target_enhance_level == 1 &&
            enhance_ship_action->materials.size() == 1,
        "ship enhance command payload mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string unordered_enhance_materials = enhance_warehouse_command_request;
    const std::size_t first_material =
        unordered_enhance_materials.find(R"("item_id":17001)");
    require(first_material != std::string::npos, "enhance fixture first material is missing");
    unordered_enhance_materials.replace(
        first_material,
        std::string(R"("item_id":17001)").size(),
        R"("item_id":17003)");
    require(
        !parse_rpc_request(
            unordered_enhance_materials,
            &request,
            &can_respond,
            &error) &&
            error.code == "equipment_command_precondition_invalid",
        "enhance materials must be strictly ordered by item id");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string maximum_enhance_materials = enhance_warehouse_command_request;
    const std::size_t materials_start = maximum_enhance_materials.find(R"("materials":[)");
    const std::size_t materials_end = maximum_enhance_materials.find(
        R"(],"gold_before")",
        materials_start);
    require(
        materials_start != std::string::npos && materials_end != std::string::npos,
        "warehouse enhance material array boundary is missing");
    std::string material_array = R"("materials":[)";
    for (std::uint64_t index = 1; index <= kMaximumEnhanceMaterialCount; ++index) {
        if (index > 1) {
            material_array.push_back(',');
        }
        material_array += R"({"item_id":)" + std::to_string(17'000 + index) +
                          R"(,"quantity_before":10,"cost":1})";
    }
    maximum_enhance_materials.replace(
        materials_start,
        materials_end + 1 - materials_start,
        material_array + ']');
    require(
        parse_rpc_request(
            maximum_enhance_materials,
            &request,
            &can_respond,
            &error),
        "warehouse enhance must accept the documented 64-material boundary");
    enhance_warehouse_command = executed_equipment_command(request);
    enhance_warehouse_action =
        enhance_warehouse_command == nullptr
            ? nullptr
            : std::get_if<EnhanceWarehouseEquipmentCommandAction>(
                  &enhance_warehouse_command->action);
    require(
        enhance_warehouse_action != nullptr &&
            enhance_warehouse_action->materials.size() == kMaximumEnhanceMaterialCount,
        "warehouse enhance maximum material batch mismatch");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string enhance_ship_with_target = enhance_ship_command_request;
    const std::size_t enhance_ship_source =
        enhance_ship_with_target.find(R"("source_before":)");
    require(enhance_ship_source != std::string::npos, "ship enhance source is missing");
    enhance_ship_with_target.insert(enhance_ship_source, R"("target_before":null,)");
    require(
        !parse_rpc_request(
            enhance_ship_with_target,
            &request,
            &can_respond,
            &error) &&
            error.code == "equipment_command_action_invalid",
        "ship enhance must reject warehouse-only target fields");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    std::string old_schema_command = dismantle_command_request;
    const std::size_t schema_version = old_schema_command.find(R"("schema_version":2)");
    require(schema_version != std::string::npos, "dismantle fixture schema is missing");
    old_schema_command.replace(
        schema_version,
        std::string(R"("schema_version":2)").size(),
        R"("schema_version":1)");
    require(
        !parse_rpc_request(old_schema_command, &request, &can_respond, &error) &&
            error.code == "equipment_command_metadata_invalid",
        "equipment command schema version one must be rejected after the wire split");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string unobservable_equip_command =
        R"({"protocol_version":1,"request_id":"000000000000000f","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":2,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"equip","ship_id":9001,"slot_index":1,"target_before":{"equipment_id":500,"config_id":500,"enhance_level":10},"source_before":{"equipment_id":500,"config_id":500,"enhance_level":10},"source_quantity_before":3,"target_warehouse_quantity_before":3,"equipment_capacity_before":11,"equipment_limit_before":300}}})";
    require(
        !parse_rpc_request(unobservable_equip_command, &request, &can_respond, &error) &&
            error.code == "equipment_command_precondition_invalid",
        "equip command must reject an unobservable same-id replacement");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string query_command_request =
        R"({"protocol_version":1,"request_id":"000000000000000b","operation":"query_equipment_command","timeout_ms":5000,"payload":{"command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}})";
    require(
        parse_rpc_request(query_command_request, &request, &can_respond, &error) &&
            rpc_body<QueryEquipmentCommandRpcRequest>(request) != nullptr &&
            rpc_body<QueryEquipmentCommandRpcRequest>(request)->command_id ==
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        "equipment command query must parse");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string cancel_command_request =
        R"({"protocol_version":1,"request_id":"000000000000000c","operation":"cancel_equipment_command","timeout_ms":5000,"payload":{"command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}})";
    require(
        parse_rpc_request(cancel_command_request, &request, &can_respond, &error) &&
            operation_of(request) == Operation::CancelEquipmentCommand,
        "equipment command cancel must parse");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string invalid_unequip_command =
        R"({"protocol_version":1,"request_id":"000000000000000d","operation":"execute_equipment_command","timeout_ms":5000,"payload":{"schema_version":2,"command_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","target_fingerprint_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","plan_hash":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","sequence":1,"pre_state_content_sha256":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","action":{"kind":"unequip","ship_id":9001,"slot_index":1,"target_before":{"equipment_id":500,"config_id":500,"enhance_level":10},"target_warehouse_quantity_before":2,"equipment_capacity_before":300,"equipment_limit_before":300}}})";
    require(
        !parse_rpc_request(invalid_unequip_command, &request, &can_respond, &error) &&
            error.code == "equipment_command_precondition_invalid",
        "unequip command must reject a full equipment warehouse");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string duplicate_lookup_command =
        R"({"protocol_version":1,"request_id":"000000000000000e","operation":"query_equipment_command","timeout_ms":5000,"payload":{"command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"}})";
    require(
        !parse_rpc_request(duplicate_lookup_command, &request, &can_respond, &error) &&
            error.code == "duplicate_field",
        "equipment command lookup must reject duplicate command_id");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string shutdown_request =
        R"({"protocol_version":1,"request_id":"0000000000000009","operation":"shutdown","timeout_ms":5000,"payload":{}})";
    require(
        parse_rpc_request(shutdown_request, &request, &can_respond, &error) &&
            rpc_body<ShutdownRpcRequest>(request) != nullptr &&
            rpc_body<ShutdownRpcRequest>(request)->timeout_ms == 5'000,
        "shutdown with empty payload must parse");
    const std::string invalid_shutdown_request =
        R"({"protocol_version":1,"request_id":"000000000000000a","operation":"shutdown","timeout_ms":5000,"payload":{"force":true}})";
    require(
        !parse_rpc_request(invalid_shutdown_request, &request, &can_respond, &error) &&
            error.code == "unknown_field",
        "shutdown payload must remain empty");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string unordered_weapon_request =
        R"({"protocol_version":1,"request_id":"0000000000000008","operation":"snapshot_equipment_weapons","timeout_ms":5000,"payload":{"weapon_ids":[1002,1001]}})";
    require(
        !parse_rpc_request(unordered_weapon_request, &request, &can_respond, &error),
        "unordered equipment-weapon batch must fail");
    require(
        can_respond && error.code == "equipment_weapon_batch_invalid",
        "unordered equipment-weapon batch must return stable error");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string unordered_attribute_request =
        R"({"protocol_version":1,"request_id":"0000000000000009","operation":"snapshot_equipment_reference_names","timeout_ms":5000,"payload":{"equipment_type_ids":[],"nation_ids":[],"ship_type_ids":[],"attribute_keys":["reload","cannon"]}})";
    require(
        !parse_rpc_request(unordered_attribute_request, &request, &can_respond, &error),
        "unordered equipment-reference attributes must fail");
    require(
        can_respond && error.code == "equipment_reference_batch_invalid",
        "unordered equipment-reference attributes must return stable error");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string zero_equipment_type_request =
        R"({"protocol_version":1,"request_id":"000000000000000a","operation":"snapshot_equipment_reference_names","timeout_ms":5000,"payload":{"equipment_type_ids":[0],"nation_ids":[],"ship_type_ids":[],"attribute_keys":[]}})";
    require(
        !parse_rpc_request(zero_equipment_type_request, &request, &can_respond, &error),
        "zero equipment-type identifier must fail");
    require(
        can_respond && error.code == "equipment_reference_batch_invalid",
        "zero equipment-type identifier must return stable error");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string oversized_equipment_page =
        R"({"protocol_version":1,"request_id":"0000000000000006","operation":"snapshot_equipment_configs","timeout_ms":5000,"payload":{"start_index":0,"page_size":1001}})";
    require(
        !parse_rpc_request(oversized_equipment_page, &request, &can_respond, &error),
        "oversized equipment-config page must fail");
    require(
        can_respond && error.code == "catalog_page_out_of_range",
        "oversized equipment-config page must return stable error");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string unknown_ship_catalog_table =
        R"({"protocol_version":1,"request_id":"0000000000000006","operation":"snapshot_ship_catalog","timeout_ms":5000,"payload":{"table_key":"lua_globals","start_index":0,"page_size":1}})";
    require(
        !parse_rpc_request(unknown_ship_catalog_table, &request, &can_respond, &error),
        "unknown ship-catalog table must fail");
    require(
        can_respond && error.code == "ship_catalog_table_unsupported",
        "unknown ship-catalog table must return stable error");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string duplicate_payload =
        R"({"protocol_version":1,"request_id":"0000000000000002","operation":"snapshot_bag","timeout_ms":5000,"payload":{"max_items":1,"max_items":2}})";
    require(
        !parse_rpc_request(duplicate_payload, &request, &can_respond, &error),
        "duplicate payload field must fail");
    require(can_respond && error.code == "duplicate_field", "duplicate field must return stable error");

    request = RpcRequest{};
    error = AgentError{};
    can_respond = false;
    const std::string unknown_operation = azlw::test_contract::kUnknownOperationRequest;
    require(
        parse_rpc_request(unknown_operation, &request, &can_respond, &error) &&
            operation_of(request) == Operation::Unsupported &&
            rpc_body<UnsupportedRpcRequest>(request) != nullptr &&
            !rpc_body<UnsupportedRpcRequest>(request)->raw_operation.empty(),
        "unknown operation must remain dispatchable for unsupported_operation");

    const AgentIdentity identity{
        .session_id = "000102030405060708090a0b0c0d0e0f",
        .process_id = 13'882,
        .package_name = "com.bilibili.azurlane",
    };
    const std::string handshake_ok = encode_handshake_ok(identity);
    require(
        handshake_ok == azlw::test_contract::kHandshakeResponse,
        "handshake encoding must expose the frozen agent version");
    const std::string health = encode_health("0000000000000001", identity, true, 1);
    require(
        health == azlw::test_contract::kHealthResponse,
        "health encoding must remain deterministic");
    const AgentError contract_error{
        .code = "fixture_failed",
        .stage = "fixture.execute",
        .message = "fixture execution failed",
        .retry = "same_request",
        .session_effect = "unchanged",
    };
    require(
        encode_error("0000000000000004", contract_error) == azlw::test_contract::kErrorResponse,
        "error encoding must retain the shared empty-details contract");
    const EquipmentCommandReceipt command_receipt{
        .schema_version = 1,
        .command_id =
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
        .status = EquipmentCommandStatus::Unknown,
        .phase = EquipmentCommandPhase::Observing,
        .write_dispatched = true,
        .cancel_requested = false,
        .observation_count = 2,
        .error_code = std::nullopt,
        .message = std::nullopt,
    };
    require(
        encode_equipment_command_receipt("0000000000000002", command_receipt) ==
            R"({"protocol_version":1,"request_id":"0000000000000002","status":"ok","result":{"schema_version":1,"command_id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","status":"unknown","phase":"observing","write_dispatched":true,"cancel_requested":false,"observation_count":2,"error_code":null,"message":null}})",
        "equipment command receipt must encode deterministically");
    EquipmentCommandReceipt invalid_command_receipt = command_receipt;
    invalid_command_receipt.write_dispatched = false;
    require(
        encode_equipment_command_receipt("0000000000000003", invalid_command_receipt) ==
            R"({"protocol_version":1,"request_id":"0000000000000003","status":"error","error":{"code":"equipment_command_receipt_invalid","stage":"agent.protocol","message":"确定未派发的装备命令必须返回错误响应","retry":"never","session_effect":"unchanged","details":{}}})",
        "invalid equipment command receipt must not encode as success");
    invalid_command_receipt = command_receipt;
    invalid_command_receipt.status = EquipmentCommandStatus::Success;
    require(
        encode_equipment_command_receipt("0000000000000004", invalid_command_receipt).find(
            R"("code":"equipment_command_receipt_invalid")") != std::string::npos,
        "mismatched equipment command receipt state must encode as an error");
    invalid_command_receipt = command_receipt;
    invalid_command_receipt.error_code = "observation_failed";
    require(
        encode_equipment_command_receipt("0000000000000005", invalid_command_receipt).find(
            R"("code":"equipment_command_receipt_invalid")") != std::string::npos,
        "unpaired equipment command receipt diagnostics must encode as an error");
    const ShutdownPreparation shutdown{
        .success = true,
        .error = {},
        .worker_tid = 13'900,
        .worker_start_time = 123'456,
        .hook_target = 0x7f0012345000,
        .trampoline_start = 0x7f0098765000,
        .trampoline_size = 4096,
    };
    require(
        encode_shutdown_prepared("0000000000000002", identity, shutdown) ==
            R"({"protocol_version":1,"request_id":"0000000000000002","status":"ok","result":{"state":"prepared","session_id":"000102030405060708090a0b0c0d0e0f","process_id":13882,"worker_tid":13900,"worker_start_time":123456,"hook_target":"00007f0012345000","trampoline_start":"00007f0098765000","trampoline_size":4096}})",
        "shutdown receipt must bind thread and executable ranges deterministically");

    const std::string awaiting_bag =
        encode_capabilities(
            "0000000000000002", true, false, false, false, false, false, false, false, false);
    require(
        awaiting_bag.find(
            R"("read.bag":{"available":false,"reason_code":"bag_proxy_not_ready")") !=
            std::string::npos,
        "bag capability must remain unavailable before a complete snapshot");
    require(
        awaiting_bag.find(
            R"("write.equip":{"available":false,"reason_code":"owned_state_not_ready")") !=
                std::string::npos &&
            awaiting_bag.find(
                R"("write.destroy":{"available":false,"reason_code":"owned_state_not_ready")") !=
                std::string::npos,
        "equipment writes must wait for a complete owned-state snapshot");
    const std::string ready_bag =
        encode_capabilities(
            "0000000000000003", true, true, false, false, true, true, true, true, true);
    require(
        ready_bag.find(R"("read.bag":{"available":true,"reason_code":"ready")") !=
            std::string::npos,
        "bag capability must become ready after a complete snapshot");
    require(
        ready_bag.find(
            R"("read.owned_state":{"available":false,"reason_code":"owned_state_not_ready")") !=
            std::string::npos,
        "owned-state capability must remain unavailable before a complete snapshot");
    require(
        ready_bag.find(
            R"("read.ship_details":{"available":false,"reason_code":"ship_details_not_ready")") !=
            std::string::npos,
        "ship-details capability must remain unavailable before a complete snapshot");
    require(
        ready_bag.find(
            R"("read.equipment_configs":{"available":true,"reason_code":"ready")") !=
            std::string::npos,
        "equipment-config capability must follow main-thread readiness");
    const std::string ready_equipment_writes =
        encode_capabilities(
            "0000000000000004", true, true, true, true, true, true, true, true, true);
    require(
        ready_equipment_writes.find(
            R"("write.equip":{"available":true,"reason_code":"ready")") !=
            std::string::npos &&
            ready_equipment_writes.find(
                R"("write.unequip":{"available":true,"reason_code":"ready")") !=
                std::string::npos &&
            ready_equipment_writes.find(
                R"("write.destroy":{"available":true,"reason_code":"ready")") !=
                std::string::npos &&
            ready_equipment_writes.find(
                R"("write.compose":{"available":true,"reason_code":"ready")") !=
                std::string::npos &&
            ready_equipment_writes.find(
                R"("write.enhance":{"available":true,"reason_code":"ready")") !=
                std::string::npos,
        "equipment writes must become ready after complete owned-state evidence");
    const std::string enhance_waiting_for_bag =
        encode_capabilities(
            "0000000000000005", true, false, true, true, true, true, true, true, true);
    require(
        enhance_waiting_for_bag.find(
            R"("write.enhance":{"available":false,"reason_code":"bag_proxy_not_ready")") !=
            std::string::npos,
        "enhance writes must wait for a complete bag snapshot");
    const std::string enhance_waiting_for_configs =
        encode_capabilities(
            "0000000000000006", true, true, true, true, false, true, true, true, true);
    require(
        enhance_waiting_for_configs.find(
            R"("write.enhance":{"available":false,"reason_code":"equipment_configs_not_ready")") !=
            std::string::npos,
        "enhance writes must wait for equipment config evidence");
    require(
        ready_bag.find(R"("read.compose_recipes":{"available":true,"reason_code":"ready")") !=
            std::string::npos,
        "compose-recipe capability must follow main-thread readiness");
    require(
        ready_bag.find(
            R"("read.equipment_weapons":{"available":true,"reason_code":"ready")") !=
            std::string::npos,
        "equipment-weapon capability must follow completed batch state");
    require(
        ready_bag.find(
            R"("read.skill_effects":{"available":true,"reason_code":"ready")") !=
            std::string::npos,
        "skill-effect capability must follow completed batch state");
    require(
        ready_bag.find(
            R"("read.equipment_reference_names":{"available":true,"reason_code":"ready")") !=
            std::string::npos,
        "equipment-reference capability must follow completed batch state");

    BagSnapshot snapshot;
    snapshot.complete = true;
    snapshot.items.push_back(BagItem{
        .item_id = 20'001,
        .quantity = 37,
        .resolved_name = "物资样本",
        .compose_recipe = std::nullopt,
    });
    const std::string encoded_snapshot = encode_snapshot("0000000000000002", snapshot);
    require(encoded_snapshot.find(R"("compose_recipe":null)") != std::string::npos, "nullable recipe must be emitted");
    require(encoded_snapshot.find(R"("count":1)") != std::string::npos, "snapshot count must match items");

    OwnedStateSnapshot owned_state;
    owned_state.complete = true;
    owned_state.dock.complete = true;
    OwnedShip ship;
    ship.ship_id = 9'001;
    ship.config_id = 101'174;
    ship.level = 100;
    ship.experience_in_level = 3'000'000;
    ship.intimacy_raw = 10'000;
    ship.energy = 150;
    ship.proficiency = 0;
    ship.fleet_memberships.push_back(ShipFleetMembership{
        .fleet_id = 1,
        .display_name = "第一舰队",
        .kind = "regular",
        .team = "vanguard",
        .position = 1,
    });
    ship.skills.push_back(OwnedShipSkill{
        .skill_id = 10'410,
        .level = 1,
        .experience = 0,
    });
    for (std::uint32_t slot_index = 1; slot_index <= kShipEquipmentSlotCount; ++slot_index) {
        ship.slots.push_back(ShipEquipmentSlot{
            .slot_index = slot_index,
            .equipment = slot_index == 1
                             ? std::optional<EquipmentSnapshot>(EquipmentSnapshot{
                                   .equipment_id = 500,
                                   .config_id = 500,
                                   .enhance_level = 10,
                               })
                             : std::nullopt,
        });
    }
    owned_state.dock.ships.push_back(std::move(ship));
    owned_state.warehouse.complete = true;
    owned_state.warehouse.items.push_back(WarehouseEquipment{
        .equipment = EquipmentSnapshot{
            .equipment_id = 600,
            .config_id = 600,
            .enhance_level = 0,
        },
        .quantity = 2,
    });
    owned_state.bag = snapshot;
    owned_state.player = PlayerResources{
        .gold = 123'456,
        .equipment_capacity = 2,
        .equipment_limit = 300,
    };
    const std::string encoded_owned_state =
        encode_owned_state_snapshot("0000000000000004", owned_state);
    require(
        encoded_owned_state.find(
            R"("schema_version":3,"complete":true,"dock":{"complete":true)") !=
            std::string::npos,
        "owned-state schema must be version 3");
    require(
        encoded_owned_state.find(R"("ship_id":9001,"config_id":101174,"level":100,"experience_in_level":3000000,"intimacy_raw":10000,"energy":150,"proficiency":0,"fleet_memberships":[{"fleet_id":1,"display_name":"第一舰队","kind":"regular","team":"vanguard","position":1}],"skills":[{"skill_id":10410,"level":1,"experience":0}],"slots":[{"slot_index":1,"equipment":{"equipment_id":500,"config_id":500,"enhance_level":10}})") !=
            std::string::npos,
        "owned-state ship growth, fleets, skills and slots must encode deterministically");
    require(
        encoded_owned_state.find(
            R"("player":{"gold":123456,"equipment_capacity":2,"equipment_limit":300})") !=
            std::string::npos,
        "owned-state player resources must encode deterministically");

    OwnedStateSnapshot diagnostic_state;
    diagnostic_state.dock.read_errors.push_back(ShipReadError{
        .ship_id = 9'001,
        .skill_id = std::nullopt,
        .slot_index = std::nullopt,
        .code = "ship_growth_invalid",
        .message = "舰船养成字段无效",
    });
    const std::string encoded_diagnostic =
        encode_owned_state_snapshot("0000000000000005", diagnostic_state);
    require(
        encoded_diagnostic.find(
            R"("ship_id":9001,"skill_id":null,"slot_index":null,"code":"ship_growth_invalid")") !=
            std::string::npos,
        "owned-state errors must emit nullable skill_id deterministically");

    ShipDetailsSnapshot details;
    details.complete = true;
    details.source.module_sha256 = std::string(64, 'a');
    ShipDetail detail;
    detail.ship_id = 9'001;
    detail.config_id = 101'174;
    detail.name = "拉菲";
    detail.level = 100;
    detail.max_level = 100;
    detail.experience_in_level = 3'000'000;
    detail.total_experience = 4'000'000;
    detail.next_level_experience = 0;
    detail.intimacy_raw = 10'000;
    detail.intimacy_maximum = 100;
    detail.intimacy_stage_id = 5;
    detail.intimacy_stage_description = "爱";
    detail.combat_power = 3'425;
    detail.locked = true;
    detail.oil_cost_start = 2;
    detail.oil_cost_end = 8;
    detail.oil_cost_total = 10;
    detail.classification = ShipClassification{
        .group_id = 101'17,
        .ship_type_id = 1,
        .ship_type_name = "驱逐",
        .armor_type_id = 1,
        .armor_type_name = "轻型装甲",
        .nation_id = 1,
        .nation_name = "白鹰",
        .rarity = 4,
        .star = 5,
        .max_star = 5,
        .skin_id = 101'170,
    };
    detail.base_attributes.durability = 1'721;
    detail.base_attributes.speed = 16.5;
    detail.equipment_applied_attributes = detail.base_attributes;
    detail.equipment_applied_attributes.cannon = 77;
    detail.effective_attributes = detail.equipment_applied_attributes;
    detail.effective_attributes.cannon = 80;
    detail.slot_rules = {
        ShipEquipmentSlotRule{.slot_index = 1, .allowed_equipment_type_ids = {1, 2}},
        ShipEquipmentSlotRule{.slot_index = 2, .allowed_equipment_type_ids = {5}},
        ShipEquipmentSlotRule{.slot_index = 3, .allowed_equipment_type_ids = {6, 21}},
        ShipEquipmentSlotRule{.slot_index = 4, .allowed_equipment_type_ids = {10}},
        ShipEquipmentSlotRule{.slot_index = 5, .allowed_equipment_type_ids = {10}},
    };
    detail.skills.push_back(ShipSkillDetail{
        .skill_id = 10'410,
        .effective_skill_id = 10'410,
        .name = "所罗门的战神",
        .level = 10,
        .max_level = 10,
        .experience = 0,
        .next_level_experience = 0,
        .description_template = "炮击提高$1",
        .current_effect = "",
    });
    details.ships.push_back(std::move(detail));
    const std::string encoded_details =
        encode_ship_details_snapshot("0000000000000006", details);
    require(
        encoded_details.find(
            R"("schema_version":4,"complete":true,"count":1,"truncated":false,"source":{"module_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})") !=
            std::string::npos,
        "ship-details source and completeness must encode deterministically");
    require(
        encoded_details.find(
            R"("slot_rules":[{"slot_index":1,"allowed_equipment_type_ids":[1,2]},{"slot_index":2,"allowed_equipment_type_ids":[5]},{"slot_index":3,"allowed_equipment_type_ids":[6,21]},{"slot_index":4,"allowed_equipment_type_ids":[10]},{"slot_index":5,"allowed_equipment_type_ids":[10]}])") !=
            std::string::npos,
        "ship slot equipment types must encode deterministically");
    require(
        encoded_details.find(
            R"("base_attributes":{"durability":1721,"cannon":0,"torpedo":0,"anti_aircraft":0,"air":0,"reload":0,"hit":0,"dodge":0,"anti_sub":0,"luck":0,"speed":16.5})") !=
            std::string::npos,
        "fractional ship attributes must remain numeric and deterministic");
    require(
        encoded_details.find(
            R"("skill_id":10410,"effective_skill_id":10410,"name":"所罗门的战神","level":10,"max_level":10,"experience":0,"next_level_experience":0,"description_template":"炮击提高$1","current_effect":""})") !=
            std::string::npos,
        "ship skill template and explicitly empty current effect must be preserved");

    ShipCatalogPage ship_catalog_page;
    ship_catalog_page.table_key = "ship_data_statistics";
    ship_catalog_page.complete = true;
    ship_catalog_page.module_sha256 = std::string(64, 'a');
    ship_catalog_page.start_index = 0;
    ship_catalog_page.total_count = 2;
    ship_catalog_page.next_index = 1;
    ShipCatalogRecord ship_catalog_record;
    ship_catalog_record.id = 101'021;
    ship_catalog_record.raw.kind = LuaValueKind::Object;
    ship_catalog_record.raw.keys.push_back(LuaValueKey{
        .kind = LuaValueKey::Kind::String,
        .number = 0.0,
        .text = "name",
        .lua_type = {},
    });
    LuaValue ship_name;
    ship_name.kind = LuaValueKind::String;
    ship_name.text = "测试舰船";
    ship_catalog_record.raw.values.push_back(std::move(ship_name));
    ship_catalog_page.records.push_back(std::move(ship_catalog_record));
    const std::string encoded_ship_catalog_page =
        encode_ship_catalog_page("0000000000000007", ship_catalog_page);
    require(
        encoded_ship_catalog_page.find(
            R"("table_key":"ship_data_statistics","source":{"module_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"start_index":0,"total_count":2,"next_index":1,"records":[{"id":101021,"raw":{"name":"测试舰船"}}],"read_errors":[],"complete":true)") !=
            std::string::npos,
        "ship-catalog page must preserve fixed table identity, cursor, source, and raw values");

    EquipmentConfigPage equipment_page;
    equipment_page.complete = true;
    equipment_page.source.module_sha256 = std::string(64, 'a');
    equipment_page.start_index = 0;
    equipment_page.total_count = 2;
    equipment_page.next_index = 1;
    EquipmentConfigRecord equipment_config;
    equipment_config.config_id = 500;
    equipment_config.root_config_id = 500;
    equipment_config.raw_config.kind = LuaValueKind::Object;
    equipment_config.raw_config.keys.push_back(LuaValueKey{
        .kind = LuaValueKey::Kind::String,
        .number = 0.0,
        .text = "name",
        .lua_type = {},
    });
    LuaValue equipment_name;
    equipment_name.kind = LuaValueKind::String;
    equipment_name.text = "测试装备";
    equipment_config.raw_config.values.push_back(std::move(equipment_name));
    equipment_config.attributes.kind = LuaValueKind::Array;
    equipment_config.properties.kind = LuaValueKind::Object;
    equipment_config.skill.kind = LuaValueKind::Null;
    equipment_config.property_rate.kind = LuaValueKind::Table;
    equipment_config.property_rate.keys.push_back(LuaValueKey{
        .kind = LuaValueKey::Kind::Number,
        .number = 2.0,
        .text = {},
        .lua_type = {},
    });
    LuaValue property_rate;
    property_rate.kind = LuaValueKind::Number;
    property_rate.number = 1.5;
    equipment_config.property_rate.values.push_back(std::move(property_rate));
    equipment_config.weapon_ids = {1'001, 1'002};
    equipment_config.gear_score = 25;
    equipment_config.anti_siren_power = std::nullopt;
    equipment_config.is_device = false;
    equipment_config.is_aircraft = false;
    equipment_config.complete = true;
    equipment_page.configs.push_back(std::move(equipment_config));
    const std::string encoded_equipment_page =
        encode_equipment_config_page("0000000000000007", equipment_page);
    require(
        encoded_equipment_page.find(
            R"("schema_version":1,"complete":true,"count":1,"source":{"module_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"start_index":0,"total_count":2,"next_index":1)") !=
            std::string::npos,
        "equipment-config page cursor and source must encode deterministically");
    require(
        encoded_equipment_page.find(
            R"("raw_config":{"name":"测试装备"},"attributes":[],"properties":{},"skill":null,"property_rate":{"lua_type":"table","entries":[{"key_type":"number","key":2,"lua_type":null,"value":1.5}],"truncated":false,"reason":null})") !=
            std::string::npos,
        "equipment-config Lua values must preserve ordinary and mixed tables");

    ComposeRecipePage compose_page;
    compose_page.complete = true;
    compose_page.source.module_sha256 = std::string(64, 'a');
    compose_page.start_index = 0;
    compose_page.total_count = 1;
    compose_page.recipes.push_back(EquipmentComposeRecipe{
        .recipe_id = 1,
        .material_id = 20'001,
        .material_count = 5,
        .gold = 100,
        .equipment_id = 500,
    });
    const std::string encoded_compose_page =
        encode_compose_recipe_page("0000000000000008", compose_page);
    require(
        encoded_compose_page.find(
            R"("start_index":0,"total_count":1,"next_index":null,"recipes":[{"recipe_id":1,"material_id":20001,"material_count":5,"gold":100,"equipment_id":500}])") !=
            std::string::npos,
        "compose-recipe page must encode static costs deterministically");

    EquipmentWeaponBatch weapon_batch;
    weapon_batch.complete = true;
    weapon_batch.source.module_sha256 = std::string(64, 'a');
    EquipmentWeaponDetail weapon;
    weapon.weapon_id = 1'001;
    weapon.raw.kind = LuaValueKind::Object;
    weapon.raw.keys.push_back(LuaValueKey{
        .kind = LuaValueKey::Kind::String,
        .number = 0.0,
        .text = "damage",
        .lua_type = {},
    });
    LuaValue damage;
    damage.kind = LuaValueKind::Number;
    damage.number = 25;
    weapon.raw.values.push_back(std::move(damage));
    weapon.complete = true;
    weapon_batch.weapons.push_back(std::move(weapon));
    const std::string encoded_weapon_batch =
        encode_equipment_weapon_batch("0000000000000009", weapon_batch);
    require(
        encoded_weapon_batch.find(
            R"("count":1,"source":{"module_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"weapons":[{"weapon_id":1001,"raw":{"damage":25},"complete":true,"read_errors":[]}])") !=
            std::string::npos,
        "equipment-weapon batch must preserve request order and raw fields");

    SkillEffectBatch skill_batch;
    skill_batch.complete = true;
    skill_batch.source.module_sha256 = std::string(64, 'a');
    SkillEffectDetail skill;
    skill.skill_id = 60'720;
    skill.level = 1;
    skill.display.available = true;
    skill.display.complete = true;
    skill.display.value.kind = LuaValueKind::Object;
    skill.battle_skill.available = true;
    skill.battle_skill.complete = true;
    skill.battle_skill.value.kind = LuaValueKind::Object;
    skill.battle_buff.error = "GetBuffTemplate 不存在";
    skill.complete = true;
    skill_batch.skills.push_back(std::move(skill));
    const std::string encoded_skill_batch =
        encode_skill_effect_batch("000000000000000a", skill_batch);
    require(
        encoded_skill_batch.find(
            R"("battle_buff":{"available":false,"complete":false,"value":null,"error":"GetBuffTemplate 不存在","read_errors":[]},"complete":true)") !=
            std::string::npos,
        "skill-effect batch must preserve unavailable source without losing complete peer source");

    EquipmentReferenceNameBatch reference_batch;
    reference_batch.complete = true;
    reference_batch.source.module_sha256 = std::string(64, 'a');
    reference_batch.equipment_types.push_back(EquipmentReferenceName{
        .identifier = 4,
        .name = "主炮",
        .error = std::nullopt,
    });
    reference_batch.attributes.push_back(EquipmentAttributeName{
        .key = "cannon",
        .name = "炮击",
        .error = std::nullopt,
    });
    const std::string encoded_reference_batch =
        encode_equipment_reference_name_batch("000000000000000b", reference_batch);
    require(
        encoded_reference_batch.find(
            R"("count":2,"source":{"module_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"equipment_types":[{"equipment_type_id":4,"name":"主炮","error":null}],"nations":[],"ship_types":[],"attributes":[{"attribute_key":"cannon","name":"炮击","error":null}])") !=
            std::string::npos,
        "equipment-reference batch must preserve namespaces and request order");

    {
        RpcRequest selected;
        AgentError error;
        bool selected_can_respond = false;
        require(parse_rpc_request(R"({"protocol_version":1,"request_id":"0000000000000001","operation":"snapshot_equipment_configs","timeout_ms":5000,"payload":{"ids":[100,200]}})", &selected, &selected_can_respond, &error), "显式配置 ID 应被接受");
        require(std::get<SnapshotEquipmentConfigsRpcRequest>(selected.body).ids == std::vector<std::uint64_t>({100,200}), "配置 ID 应保留");
        for (const auto payload : {R"({"ids":[]})", R"({"ids":[2,1]})", R"({"ids":[1,1]})", R"({"ids":[0]})", R"({"ids":[1],"start_index":0,"page_size":1})"}) {
            error = {};
            const auto json = std::string(R"({"protocol_version":1,"request_id":"0000000000000001","operation":"snapshot_equipment_configs","timeout_ms":5000,"payload":)") + payload + "}";
            require(!parse_rpc_request(json, &selected, &selected_can_respond, &error), "无效选择器应拒绝");
        }
        error = {};
        require(parse_rpc_request(R"({"protocol_version":1,"request_id":"0000000000000001","operation":"snapshot_resources","timeout_ms":5000,"payload":{}})", &selected, &selected_can_respond, &error), "资源读取应接受空载荷");
        require(operation_of(selected) == Operation::SnapshotResources, "资源操作分类错误");
    }
    std::cout << "PASS agent_protocol_test" << std::endl;
    return 0;
}

// 编码握手、错误、能力与关闭响应。

#include "json_codec.h"
#include "json_codec_internal.h"

#include <algorithm>
#include <array>
#include <charconv>

namespace azlw::agent {

using json_codec_internal::begin_ok_response;

namespace {

/// 将进程地址编码为固定 16 位小写十六进制，避免 JSON 数字精度差异。
std::string fixed_hex_address(std::uint64_t value) {
    std::array<char, 16> result{};
    result.fill('0');
    std::array<char, 16> digits{};
    const auto encoded = std::to_chars(digits.data(), digits.data() + digits.size(), value, 16);
    const std::size_t length = static_cast<std::size_t>(encoded.ptr - digits.data());
    std::copy(digits.data(), encoded.ptr, result.end() - static_cast<std::ptrdiff_t>(length));
    return std::string(result.data(), result.size());
}

/// 写入带原因码和至少一条证据的单项能力状态。
void write_capability(
    JsonWriter* writer,
    std::string_view key,
    bool available,
    std::string_view reason,
    std::string_view evidence) {
    writer->key(key);
    writer->begin_object();
    writer->key("available");
    writer->boolean(available);
    writer->key("reason_code");
    writer->string(reason);
    writer->key("evidence");
    writer->begin_array();
    writer->string(evidence);
    writer->end_array();
    writer->end_object();
}

}  // namespace

namespace json_codec_internal {

/// 写入所有成功响应共享的顶层字段，并停在 `result` 值位置。
void begin_ok_response(JsonWriter* writer, std::string_view request_id) {
    writer->begin_object();
    writer->key("protocol_version");
    writer->number(kProtocolVersion);
    writer->key("request_id");
    writer->string(request_id);
    writer->key("status");
    writer->string("ok");
    writer->key("result");
}

}  // namespace json_codec_internal

// 按宿主冻结契约顺序编码认证成功身份。
std::string encode_handshake_ok(const AgentIdentity& identity) {
    JsonWriter writer;
    writer.begin_object();
    writer.key("protocol_version");
    writer.number(kProtocolVersion);
    writer.key("message_type");
    writer.string("handshake_ok");
    writer.key("session_id");
    writer.string(identity.session_id);
    writer.key("agent_version");
    writer.string(kAgentVersion);
    writer.key("process_id");
    writer.number(static_cast<std::int64_t>(identity.process_id));
    writer.key("package_name");
    writer.string(identity.package_name);
    writer.key("abi");
    writer.string("x86_64");
    writer.end_object();
    return writer.take();
}

// 始终保留空 details 对象，确保错误响应字段集合稳定。
std::string encode_error(std::string_view request_id, const AgentError& error) {
    JsonWriter writer;
    writer.begin_object();
    writer.key("protocol_version");
    writer.number(kProtocolVersion);
    writer.key("request_id");
    writer.string(request_id);
    writer.key("status");
    writer.string("error");
    writer.key("error");
    writer.begin_object();
    writer.key("code");
    writer.string(error.code);
    writer.key("stage");
    writer.string(error.stage);
    writer.key("message");
    writer.string(error.message);
    writer.key("retry");
    writer.string(error.retry);
    writer.key("session_effect");
    writer.string(error.session_effect);
    writer.key("details");
    writer.begin_object();
    writer.end_object();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

// 编码握手固定身份和当前主线程队列就绪状态。
std::string encode_health(
    std::string_view request_id,
    const AgentIdentity& identity,
    bool main_thread_ready, std::uint64_t catalog_generation) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("agent_version");
    writer.string(kAgentVersion);
    writer.key("process_id");
    writer.number(static_cast<std::int64_t>(identity.process_id));
    writer.key("package_name");
    writer.string(identity.package_name);
    writer.key("abi");
    writer.string("x86_64");
    writer.key("session_state");
    writer.string("ready");
    writer.key("main_thread_queue_ready");
    writer.boolean(main_thread_ready);
    writer.key("catalog_generation");
    writer.number(catalog_generation);
    writer.end_object();
    writer.end_object();
    return writer.take();
}

// 总是列出完整稳定键集合，并分别反映主线程与完整背包读取的就绪状态。
std::string encode_capabilities(
    std::string_view request_id,
    bool main_thread_ready,
    bool bag_read_ready,
    bool owned_state_read_ready,
    bool ship_details_read_ready,
    bool equipment_configs_read_ready,
    bool compose_recipes_read_ready,
    bool equipment_weapons_read_ready,
    bool skill_effects_read_ready,
    bool equipment_reference_names_read_ready) {
    const bool equipment_commands_ready = main_thread_ready && owned_state_read_ready;
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("capabilities");
    writer.begin_object();
    write_capability(&writer, "runtime.health", true, "ready", "agent 身份与 RPC 会话已验证");
    write_capability(
        &writer,
        "runtime.main_thread_queue",
        main_thread_ready,
        main_thread_ready ? "ready" : "awaiting_tolua_update",
        main_thread_ready ? "tolua_update 已捕获主 Lua 状态" : "等待游戏下一次 tolua_update");
    write_capability(
        &writer,
        "read.bag",
        bag_read_ready,
        bag_read_ready ? "ready"
                       : (main_thread_ready ? "bag_proxy_not_ready" : "main_thread_not_ready"),
        bag_read_ready
            ? "已在游戏主线程完成一份完整 BagProxy 快照"
            : (main_thread_ready ? "主线程队列可用，等待 BagProxy 初始化"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.owned_state",
        owned_state_read_ready,
        owned_state_read_ready
            ? "ready"
            : (main_thread_ready ? "owned_state_not_ready" : "main_thread_not_ready"),
        owned_state_read_ready
            ? "已在同一次游戏主线程停顿内取得完整舰船养成、自身技能、装备仓库、背包和玩家资源"
            : (main_thread_ready ? "主线程队列可用，等待完整运行态快照"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.ship_details",
        ship_details_read_ready,
        ship_details_read_ready
            ? "ready"
            : (main_thread_ready ? "ship_details_not_ready" : "main_thread_not_ready"),
        ship_details_read_ready
            ? "已在游戏主线程完整解析舰船分类、属性阶段和自身技能展示详情"
            : (main_thread_ready ? "主线程队列可用，等待舰船详情快照"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.equipment_configs",
        equipment_configs_read_ready,
        equipment_configs_read_ready
            ? "ready"
            : (main_thread_ready ? "equipment_configs_not_ready" : "main_thread_not_ready"),
        equipment_configs_read_ready
            ? "已在游戏主线程完整读取至少一页装备静态配置"
            : (main_thread_ready ? "主线程队列可用，等待装备静态表初始化"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.compose_recipes",
        compose_recipes_read_ready,
        compose_recipes_read_ready
            ? "ready"
            : (main_thread_ready ? "compose_recipes_not_ready" : "main_thread_not_ready"),
        compose_recipes_read_ready
            ? "已在游戏主线程完整读取至少一页静态合成配方"
            : (main_thread_ready ? "主线程队列可用，等待合成配方表初始化"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.equipment_weapons",
        equipment_weapons_read_ready,
        equipment_weapons_read_ready
            ? "ready"
            : (main_thread_ready ? "equipment_weapons_not_ready" : "main_thread_not_ready"),
        equipment_weapons_read_ready
            ? "已在游戏主线程完整读取至少一个装备武器参数批次"
            : (main_thread_ready ? "主线程队列可用，等待武器配置表初始化"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.skill_effects",
        skill_effects_read_ready,
        skill_effects_read_ready
            ? "ready"
            : (main_thread_ready ? "skill_effects_not_ready" : "main_thread_not_ready"),
        skill_effects_read_ready
            ? "已在游戏主线程完整读取至少一个技能效果证据批次"
            : (main_thread_ready ? "主线程队列可用，等待技能效果来源初始化"
                                 : "尚未取得主线程 Lua 状态"));
    write_capability(
        &writer,
        "read.equipment_reference_names",
        equipment_reference_names_read_ready,
        equipment_reference_names_read_ready
            ? "ready"
            : (main_thread_ready ? "equipment_reference_names_not_ready"
                                 : "main_thread_not_ready"),
        equipment_reference_names_read_ready
            ? "已在游戏主线程完整解析装备类型、阵营、舰种和属性显示名称"
            : (main_thread_ready ? "主线程队列可用，等待装备引用名称解析"
                                 : "尚未取得主线程 Lua 状态"));
    const char* equipment_write_reason =
        equipment_commands_ready
            ? "ready"
            : (main_thread_ready ? "owned_state_not_ready" : "main_thread_not_ready");
    const char* equipment_write_message =
        equipment_commands_ready
            ? "主线程局部前检、官方装备通知和状态回读已就绪"
            : (main_thread_ready ? "等待一份完整运行态快照作为写入前置证据"
                                 : "尚未取得主线程 Lua 状态");
    write_capability(
        &writer,
        "write.equip",
        equipment_commands_ready,
        equipment_write_reason,
        equipment_write_message);
    write_capability(
        &writer,
        "write.unequip",
        equipment_commands_ready,
        equipment_write_reason,
        equipment_write_message);
    const bool compose_commands_ready =
        equipment_commands_ready && bag_read_ready && compose_recipes_read_ready;
    const char* compose_write_reason =
        compose_commands_ready
            ? "ready"
            : (!main_thread_ready
                   ? "main_thread_not_ready"
                   : (!owned_state_read_ready
                          ? "owned_state_not_ready"
                          : (!bag_read_ready ? "bag_proxy_not_ready"
                                            : "compose_recipes_not_ready")));
    const char* compose_write_message =
        compose_commands_ready
            ? "合成配方、背包、物资、仓库容量和产物回读已就绪"
            : (!main_thread_ready
                   ? "尚未取得主线程 Lua 状态"
                   : (!owned_state_read_ready
                          ? "等待一份完整运行态快照作为合成前置证据"
                          : (!bag_read_ready ? "等待完整背包快照"
                                            : "等待完整合成配方快照")));
    write_capability(
        &writer,
        "write.compose",
        compose_commands_ready,
        compose_write_reason,
        compose_write_message);
    const bool enhance_commands_ready =
        equipment_commands_ready && bag_read_ready && equipment_configs_read_ready;
    const char* enhance_write_reason =
        enhance_commands_ready
            ? "ready"
            : (!main_thread_ready
                   ? "main_thread_not_ready"
                   : (!owned_state_read_ready
                          ? "owned_state_not_ready"
                          : (!bag_read_ready ? "bag_proxy_not_ready"
                                            : "equipment_configs_not_ready")));
    const char* enhance_write_message =
        enhance_commands_ready
            ? "强化配置、背包、物资、仓库容量和目标装备回读已就绪"
            : (!main_thread_ready
                   ? "尚未取得主线程 Lua 状态"
                   : (!owned_state_read_ready
                          ? "等待一份完整运行态快照作为强化前置证据"
                          : (!bag_read_ready ? "等待完整背包快照"
                                            : "等待完整装备配置快照")));
    write_capability(
        &writer,
        "write.enhance",
        enhance_commands_ready,
        enhance_write_reason,
        enhance_write_message);
    write_capability(
        &writer,
        "write.destroy",
        equipment_commands_ready,
        equipment_write_reason,
        equipment_commands_ready
            ? "拆解命令基础条件已就绪；具体来源的安全条件会在派发前复核"
            : equipment_write_message);
    writer.end_object();
    writer.end_object();
    writer.end_object();
    return writer.take();
}

// 收据同时绑定会话、进程、工作线程和两段可执行内存，供 loader 逐项复核。
std::string encode_shutdown_prepared(
    std::string_view request_id,
    const AgentIdentity& identity,
    const ShutdownPreparation& preparation) {
    JsonWriter writer;
    begin_ok_response(&writer, request_id);
    writer.begin_object();
    writer.key("state");
    writer.string("prepared");
    writer.key("session_id");
    writer.string(identity.session_id);
    writer.key("process_id");
    writer.number(static_cast<std::int64_t>(identity.process_id));
    writer.key("worker_tid");
    writer.number(static_cast<std::int64_t>(preparation.worker_tid));
    writer.key("worker_start_time");
    writer.number(preparation.worker_start_time);
    writer.key("hook_target");
    writer.string(fixed_hex_address(preparation.hook_target));
    writer.key("trampoline_start");
    writer.string(fixed_hex_address(preparation.trampoline_start));
    writer.key("trampoline_size");
    writer.number(preparation.trampoline_size);
    writer.end_object();
    writer.end_object();
    return writer.take();
}

}  // namespace azlw::agent

// 声明严格 RPC JSON 解析器、结构化写入器和各类响应编码入口。

#pragma once

#include <array>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

#include "bootstrap_config.h"
#include "snapshots/equipment_config_snapshot.h"
#include "snapshots/equipment_effect_snapshot.h"
#include "snapshots/equipment_reference_snapshot.h"
#include "snapshots/ship_catalog_snapshot.h"
#include "protocol_types.h"

namespace azlw::agent {

/// 只提供结构化 JSON 写入，避免各 RPC 手工拼接引号、逗号和转义。
class JsonWriter final {
public:
    /// 开始一个对象值。
    void begin_object();
    /// 结束当前对象。
    void end_object();
    /// 开始一个数组值。
    void begin_array();
    /// 结束当前数组。
    void end_array();
    /// 写入当前对象的字段名。
    void key(std::string_view name);
    /// 写入经过 JSON 转义的字符串值。
    void string(std::string_view value);
    /// 写入 32 位无符号整数。
    void number(std::uint32_t value);
    /// 写入 64 位无符号整数。
    void number(std::uint64_t value);
    /// 写入 64 位有符号整数。
    void number(std::int64_t value);
    /// 写入有限双精度数，保留航速等合法小数。
    void number(double value);
    /// 写入布尔值。
    void boolean(bool value);
    /// 写入显式空值。
    void null();

    /// 转移已经写入的完整 JSON 文本。
    std::string take();

private:
    /// 区分逗号和字段值状态不同的对象、数组作用域。
    enum class ScopeKind { Object, Array };
    /// 记录单层容器的首项和待写字段值状态。
    struct Scope final {
        ScopeKind kind;
        bool first = true;
        bool expects_value = false;
    };

    /// 在写值前更新当前容器的分隔与字段状态。
    void before_value();
    /// 追加带引号和控制字符转义的 JSON 字符串。
    void append_escaped(std::string_view value);

    std::string output_;
    std::vector<Scope> scopes_;
    bool root_written_ = false;
};

/// 严格解析 RPC；仅当 request_id 本身有效时才允许编码错误响应。
bool parse_rpc_request(
    std::string_view json,
    RpcRequest* request,
    bool* can_respond,
    AgentError* error);

struct OwnedQueryExecution;
std::string encode_owned_query(std::string_view request_id, const OwnedQueryExecution& execution);

/// 编码认证成功和固定 agent 身份。
std::string encode_handshake_ok(const AgentIdentity& identity);
/// 编码与请求号关联的稳定错误响应。
std::string encode_error(std::string_view request_id, const AgentError& error);
/// 编码 agent 身份及主线程队列状态。
std::string encode_health(
    std::string_view request_id,
    const AgentIdentity& identity,
    bool main_thread_ready, std::uint64_t catalog_generation);
/// 编码完整只读能力集合，并区分主线程、背包和完整运行态读取就绪状态。
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
    bool equipment_reference_names_read_ready);
/// 编码 execute、query 和 cancel 共用的原装备命令状态收据。
std::string encode_equipment_command_receipt(
    std::string_view request_id,
    const EquipmentCommandReceipt& receipt);
/// 编码宿主执行冻结与卸载前必须核对的 Agent 排空收据。
std::string encode_shutdown_prepared(
    std::string_view request_id,
    const AgentIdentity& identity,
    const ShutdownPreparation& preparation);
/// 编码背包条目、可空配方和逐条读取错误。
std::string encode_snapshot(std::string_view request_id, const BagSnapshot& snapshot);
/// 编码同一主线程停顿内取得的舰船养成、自身技能、装备、背包和玩家资源。
std::string encode_resources_snapshot(std::string_view request_id, const PlayerResources& player);

std::string encode_owned_state_snapshot(
    std::string_view request_id,
    const OwnedStateSnapshot& snapshot);
/// 编码当前客户端解析出的舰船分类、属性阶段和技能展示详情。
std::string encode_ship_details_snapshot(
    std::string_view request_id,
    const ShipDetailsSnapshot& snapshot);
/// 编码同一次船坞遍历得到的账号前窗口。dock_frames 是船坞分页帧数。
std::string encode_account_before_snapshot(
    std::string_view request_id,
    const OwnedStateSnapshot& owned,
    const ShipDetailsSnapshot& details,
    std::uint32_t dock_frames);
/// 编码一页固定白名单舰船静态配置。
std::string encode_ship_catalog_page(
    std::string_view request_id,
    const ShipCatalogPage& page);
/// 编码一页完整装备配置、派生信息和逐条诊断。
std::string encode_equipment_config_page(
    std::string_view request_id,
    const EquipmentConfigPage& page);
/// 编码一页不依赖玩家资源数量的静态合成配方。
std::string encode_compose_recipe_page(
    std::string_view request_id,
    const ComposeRecipePage& page);
/// 编码显式武器 ID 批次的原始参数和逐条诊断。
std::string encode_equipment_weapon_batch(
    std::string_view request_id,
    const EquipmentWeaponBatch& batch);
/// 编码显式技能等级批次的三路原始效果来源。
std::string encode_skill_effect_batch(
    std::string_view request_id,
    const SkillEffectBatch& batch);
/// 编码装备分类、阵营、舰种和属性名称批次。
std::string encode_equipment_reference_name_batch(
    std::string_view request_id,
    const EquipmentReferenceNameBatch& batch);

}  // namespace azlw::agent

// 声明 agent 启动事务、Hook 生命周期、主线程任务队列和装备命令账本。

#pragma once

#include <atomic>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <memory>
#include <mutex>
#include <span>
#include <string>
#include <string_view>
#include <vector>
#include <unordered_map>

#include "snapshots/bag_snapshot.h"
#include "bootstrap_config.h"
#include "snapshots/equipment_config_snapshot.h"
#include "commands/equipment_command_ledger.h"
#include "snapshots/equipment_effect_snapshot.h"
#include "snapshots/equipment_reference_snapshot.h"
#include "hook_x86_64.h"
#include "lua/lua_api.h"
#include "lua/module_view.h"
#include "snapshots/owned_state_snapshot.h"
#include "protocol/protocol_types.h"
#include "secure_channel.h"
#include "snapshots/ship_details_snapshot.h"
#include "snapshots/ship_catalog_snapshot.h"
#include "runtime/task_queue.h"

namespace azlw::agent {

class RpcServer;

/// 管理进程内唯一 agent 实例及其一次性会话状态。
class AgentRuntime final {
public:
    /// 返回进程生命周期内的唯一运行态实例。
    static AgentRuntime& instance();

    /// 擦除仍保存在内存中的会话密钥。
    ~AgentRuntime();
    /// 单例及其系统资源不可复制。
    AgentRuntime(const AgentRuntime&) = delete;
    AgentRuntime& operator=(const AgentRuntime&) = delete;

    /// 执行身份、模块、符号、Hook 和 RPC 服务的完整启动事务。
    AgentStartCode start(const BootstrapConfigV2& config) noexcept;
    /// 先调用原始 `tolua_update`，再在同一线程消费任务并观察装备命令。
    int invoke_tolua_update(lua_State* state, float delta_time, float unscaled_delta_time) noexcept;

    /// 使用启动会话凭据构造不含密钥原文的服务端挑战。
    bool create_handshake_challenge(
        const secure_channel::HandshakeNonce& server_nonce,
        secure_channel::ServerChallenge* challenge,
        std::string* error);
    /// 严格核对客户端证明，为本次连接派生独立密钥并保留驻留会话凭据。
    bool authenticate_handshake(
        const secure_channel::ServerChallenge& challenge,
        std::span<const std::uint8_t> frame,
        secure_channel::ChannelKeyMaterial* keys,
        std::string* error);
    /// 停止接收新任务，取消等待任务，并在时限内排空正在执行的任务。
    ShutdownPreparation prepare_shutdown(std::uint32_t timeout_ms);
    /// 仅供冻结后的 loader 调用；非阻塞核对状态并还原 Hook。
    AgentFinalizeCode finalize_shutdown() noexcept;
    /// 幂等擦除启动配置中的会话密钥。
    void clear_session_secret();
    /// 返回启动时固定的 agent 身份。
    const AgentIdentity& identity() const noexcept;
    /// 返回 agent 就绪且已经捕获主线程 Lua 状态的结论。
    bool main_thread_ready() const noexcept;
    /// 返回是否已经在主线程取得至少一份完整背包快照。
    bool bag_read_ready() const noexcept;
    /// 返回是否已经在同一次主线程停顿内取得至少一份完整运行态快照。
    bool owned_state_read_ready() const noexcept;
    /// 返回是否已经在主线程取得至少一份完整舰船详情快照。
    bool ship_details_read_ready() const noexcept;
    /// 返回是否已经完整读取至少一页装备静态配置。
    bool equipment_configs_read_ready() const noexcept;
    /// 返回是否已经完整读取至少一页静态合成配方。
    bool compose_recipes_read_ready() const noexcept;
    /// 返回是否已经完整读取至少一个非空武器参数批次。
    bool equipment_weapons_read_ready() const noexcept;
    /// 返回是否已经完整读取至少一个非空技能效果证据批次。
    bool skill_effects_read_ready() const noexcept;
    /// 返回是否已经完整解析至少一个非空装备引用名称批次。
    bool equipment_reference_names_read_ready() const noexcept;
    /// 返回普通装备装上和卸下是否已具备完整主线程前检基础。
    bool equipment_commands_ready() const noexcept;
    /// 排入唯一只读任务并在调用方时限内等待结果。
    SnapshotExecution execute_snapshot(std::uint32_t max_items, std::uint32_t timeout_ms);
    /// 排入包含舰船养成、自身技能、装备、背包和玩家资源的唯一只读任务。
    /// 排入账号前窗口。船坞按页跨帧续办，仓库和背包在后续帧读取。
    AccountBeforeExecution execute_account_before_snapshot(
        std::uint32_t max_ships,
        std::uint32_t max_equipments,
        std::uint32_t max_items,
        std::uint32_t timeout_ms);
    ResourcesExecution execute_resources_snapshot(std::uint32_t timeout_ms);
    OwnedQueryExecution execute_owned_query(const OwnedQuery& query, std::uint32_t timeout_ms);
    OwnedStateExecution execute_owned_state_snapshot(
        std::uint32_t max_ships,
        std::uint32_t max_equipments,
        std::uint32_t max_items,
        std::uint32_t timeout_ms);
    /// 排入当前客户端舰船分类、派生属性和技能展示详情读取任务。
    ShipDetailsExecution execute_ship_details_snapshot(
        std::uint32_t max_ships,
        std::uint32_t timeout_ms);
    /// 排入一页固定白名单舰船静态配置读取任务。
    ShipCatalogPageExecution execute_ship_catalog_page(
        std::string table_key,
        std::uint32_t start_index,
        std::uint32_t page_size,
        std::uint32_t timeout_ms);
    /// 连接结束或其他操作开始时放下图鉴续页索引。索引只属于当前连接里紧接着的下一页。
    void release_parked_collection_index() noexcept;
    /// 主线程 Lua 状态更换后递增，用于隔离静态目录缓存。
    std::uint64_t catalog_generation() const noexcept { return catalog_generation_.load(std::memory_order_acquire); }
    /// 排入一页装备静态配置读取任务。
    EquipmentConfigPageExecution execute_equipment_config_page(
        std::uint32_t start_index,
        std::uint32_t page_size,
        std::uint32_t timeout_ms,
        const std::vector<std::uint64_t>& ids);
    /// 排入一页静态合成配方读取任务。
    ComposeRecipePageExecution execute_compose_recipe_page(
        std::uint32_t start_index,
        std::uint32_t page_size,
        std::uint32_t timeout_ms);
    /// 排入显式武器 ID 批量读取任务。
    EquipmentWeaponBatchExecution execute_equipment_weapon_batch(
        std::vector<std::uint64_t> weapon_ids,
        std::uint32_t timeout_ms);
    /// 排入显式技能等级批量读取任务。
    SkillEffectBatchExecution execute_skill_effect_batch(
        std::vector<SkillEffectQuery> skills,
        std::uint32_t timeout_ms);
    /// 排入装备类型、阵营、舰种和属性名称批量解析任务。
    EquipmentReferenceNameBatchExecution execute_equipment_reference_name_batch(
        std::vector<std::uint64_t> equipment_type_ids,
        std::vector<std::uint64_t> nation_ids,
        std::vector<std::uint64_t> ship_type_ids,
        std::vector<std::string> attribute_keys,
        std::uint32_t timeout_ms);
    /// 最多派发一次装备命令；派发后立即返回 observing，供后续查询或取消。
    EquipmentCommandExecution execute_equipment_command(
        EquipmentCommand command,
        std::uint32_t timeout_ms);
    /// 查询当前会话中原装备命令的最强设备端证据。
    EquipmentCommandExecution query_equipment_command(std::string_view command_id);
    /// 停止等待原装备命令，不声称服务器请求已经撤回。
    EquipmentCommandExecution cancel_equipment_command(std::string_view command_id);

private:
    friend struct AgentRuntimeCacheTest;
    ShipCatalogPageExecution read_ship_catalog_page(std::string table_key, std::uint32_t start_index,
        std::uint32_t page_size, std::uint32_t timeout_ms);
    using StaticResult = std::variant<ShipCatalogPageExecution, EquipmentConfigPageExecution, ComposeRecipePageExecution,
        EquipmentWeaponBatchExecution, SkillEffectBatchExecution, EquipmentReferenceNameBatchExecution>;
    /// 仅 RPC 线程读写；Lua 线程只递增代次，不接触缓存容器。
    template <typename Read, typename Encode>
    auto read_static_cached(const std::string& key, Read read, Encode encode) {
        using Result = decltype(read());
        const auto generation = catalog_generation();
        if (static_cache_generation_ != generation) {
            static_cache_.clear();
            static_cache_bytes_ = 0;
            static_cache_generation_ = generation;
        }
        if (const auto found = static_cache_.find(key); found != static_cache_.end()) {
            return std::get<Result>(found->second);
        }
        auto result = read();
        const auto encoded_size = encode(result);
        // 以序列化体积约束保留量；满额后仍正常读取，但不继续增长缓存。
        constexpr std::size_t maximum_cache_bytes = 64 * 1024 * 1024;
        if (encoded_size != 0 && encoded_size <= maximum_cache_bytes - static_cache_bytes_ && generation == catalog_generation()) {
            static_cache_.emplace(key, result);
            static_cache_bytes_ += encoded_size;
        }
        return result;
    }

    std::unordered_map<std::string, StaticResult> static_cache_;
    std::size_t static_cache_bytes_ = 0;
    std::uint64_t static_cache_generation_ = 0;
    /// 约束启动只能从初始状态进入一次，失败后不得重复尝试。
    enum class StartState : std::uint8_t {
        NotStarted,
        Starting,
        Ready,
        Preparing,
        Prepared,
        Finalizing,
        Finalized,
        Failed,
    };

    /// 单例只允许通过 `instance` 构造。
    AgentRuntime() = default;

    /// 统一执行启动失败日志、资源回滚、密钥擦除和状态封闭。
    AgentStartCode fail_start(AgentStartCode code, const char* stage, const std::string& message) noexcept;
    /// 在游戏主线程保存 Lua 状态，并至多执行一个等待中的任务。
    void after_tolua_update(lua_State* state) noexcept;
    /// 提交一种具体 Lua 任务并取回它自己的结果。失败只填写该结果里的错误。
    template <typename TaskBody>
    auto execute_queued(TaskBody body, std::uint32_t timeout_ms) -> decltype(body.outcome);

    /// 统一执行单槽排队、等待和超时取消，结果由任务类型对应的字段承载。
    bool wait_for_task(
        const std::shared_ptr<LuaTask>& task,
        std::uint32_t timeout_ms,
        AgentError* error);
    /// 使用调用方冻结的绝对截止时间排队，避免装备账本和派发门各自延长时限。
    bool wait_for_task_until(
        const std::shared_ptr<LuaTask>& task,
        EquipmentCommandLedger::TimePoint deadline,
        AgentError* error);

    std::atomic<StartState> start_state_{StartState::NotStarted};
    BootstrapConfigV2 config_{};
    AgentIdentity identity_;
    ModuleView module_;
    LuaApi lua_api_;
    X64Hook hook_;
    std::unique_ptr<RpcServer> server_;
    std::atomic<std::uintptr_t> original_update_{0};
    std::atomic<std::uint32_t> active_callbacks_{0};
    std::atomic<lua_State*> main_state_{nullptr};
    std::atomic<std::uint64_t> catalog_generation_{0};
    std::atomic<bool> bag_read_ready_{false};
    std::atomic<bool> owned_state_read_ready_{false};
    std::atomic<bool> ship_details_read_ready_{false};
    std::atomic<bool> equipment_configs_read_ready_{false};
    std::atomic<bool> compose_recipes_read_ready_{false};
    std::atomic<bool> equipment_weapons_read_ready_{false};
    std::atomic<bool> skill_effects_read_ready_{false};
    std::atomic<bool> equipment_reference_names_read_ready_{false};
    std::shared_ptr<CollectionReadIndex> parked_collection_index_;
    std::uint32_t parked_collection_next_ = 0;
    std::string parked_collection_module_;
    EquipmentCommandLedger equipment_commands_;
    MainThreadTaskQueue tasks_;
};

}  // namespace azlw::agent

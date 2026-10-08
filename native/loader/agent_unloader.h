// 声明专用 Agent 卸载事务及其统一总时限。

#pragma once

#include <algorithm>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <string>
#include <utility>

namespace azlw::loader {

    /// 从配置读取时刻起只递减一次，整个卸载事务共享同一预算。
class UnloadDeadline final {
public:
    using Clock = std::chrono::steady_clock;
    using TimePoint = Clock::time_point;

    /// 固定总预算；显式起点允许调用方在已有事务时钟上继续计时。
    explicit UnloadDeadline(
        std::uint32_t timeout_ms,
        TimePoint start = Clock::now()) noexcept
        : deadline_(start + std::chrono::milliseconds(timeout_ms)),
          budget_ms_(timeout_ms) {}

    /// 返回当前剩余的正整数毫秒，到期后固定为零。
    [[nodiscard]] std::uint32_t remaining() const noexcept {
        return remaining(Clock::now());
    }

    /// 在指定单调时刻计算剩余预算，供同一时钟域的调用方精确核对边界。
    [[nodiscard]] std::uint32_t remaining(TimePoint now) const noexcept {
        if (budget_ms_ == 0 || now >= deadline_) {
            return 0;
        }
        const auto remaining =
            std::chrono::duration_cast<std::chrono::milliseconds>(deadline_ - now).count();
        return static_cast<std::uint32_t>(
            std::clamp<std::int64_t>(remaining, 1, budget_ms_));
    }

private:
    TimePoint deadline_;
    std::uint32_t budget_ms_;
};

/// 区分卸载事务的稳定失败边界，供 loader 主入口映射退出码。
enum class AgentUnloadStatus : std::uint8_t {
    Ok,
    ConfigInvalid,
    IdentityRejected,
    AttachFailed,
    FinalizeFailed,
    CleanupFailed,
};

/// 包含稳定码、诊断信息和目标 PID 的卸载事务结果。
struct AgentUnloadOutcome final {
    AgentUnloadStatus status = AgentUnloadStatus::ConfigInvalid;
    std::string stable_code;
    std::string message;
    std::int32_t process_id = 0;
};

/// 卸载收尾实际走的关闭路径，决定成功结果里的现场说明。
enum class UnloadClosePath : std::uint8_t {
    ParkedCarrier,
    CarrierExitedDuringClose,
};

/// 一次卸载的目标身份和关闭路径。结果只从这里生成，现场资源仍由顺序编排持有。
struct UnloadTransaction final {
    std::int32_t process_id = 0;
    UnloadClosePath close_path = UnloadClosePath::ParkedCarrier;

    /// 按关闭路径说明 Hook 恢复和载体退出，并附上不可访问占位数量。
    [[nodiscard]] std::string success_message(std::size_t inert_overlap_count) const {
        std::string message =
            close_path == UnloadClosePath::CarrierExitedDuringClose
                ? "专用 Agent 已还原 Hook 并完整卸载，载体线程在 dlclose 期间正常退出"
                : "专用 Agent 已还原 Hook，由停驻载体完整卸载并退出载体线程";
        if (inert_overlap_count != 0) {
            message += "；原地址范围内存在 " + std::to_string(inert_overlap_count) +
                       " 个不可访问匿名占位";
        }
        return message;
    }

    /// 按身份复核、静止证明、Hook 终结和远程关闭的顺序执行这一次卸载。
    [[nodiscard]] AgentUnloadOutcome execute(
        const std::string& agent_path,
        const std::string& session_file);

    /// 用当前目标身份生成稳定结果。配置尚未读出时进程号保持为零。
    [[nodiscard]] AgentUnloadOutcome result(
        AgentUnloadStatus status,
        std::string stable_code,
        std::string message) const {
        return AgentUnloadOutcome{
            .status = status,
            .stable_code = std::move(stable_code),
            .message = std::move(message),
            .process_id = process_id,
        };
    }
};

/// 从一次性配置恢复 Agent 身份，并在严格静止条件下完成 Hook 回滚与 dlclose。
AgentUnloadOutcome unload_agent(
    const std::string& agent_path,
    const std::string& session_file);

}  // namespace azlw::loader

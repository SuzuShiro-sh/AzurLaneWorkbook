// 声明绑定驻留会话 localabstract 地址并串行接受认证连接的 RPC 服务。

#pragma once

#include <atomic>
#include <cstdint>
#include <mutex>
#include <string>
#include <thread>

namespace azlw::agent {

class AgentRuntime;

/// 区分握手防占用超时和认证后允许正常空闲的会话策略。
enum class RpcSocketPhase : std::uint8_t {
    Handshake,
    Authenticated,
};

/// 关闭请求处理结束后，区分终止连接和必须保留为卸载载体的停驻状态。
enum class ShutdownConnectionDisposition : std::uint8_t {
    CloseAuthenticated,
    ParkWorker,
};

/// 停驻线程只有收到显式停止请求后才允许返回，其余唤醒和错误都继续等待 loader 接管。
enum class WorkerParkAction : std::uint8_t {
    ContinueWaiting,
    Stop,
};

/// 已提交的关闭准备不因收据传输结果回滚，主机未收到收据时仍独立执行既有兜底。
[[nodiscard]] ShutdownConnectionDisposition shutdown_connection_disposition(
    bool preparation_succeeded,
    bool response_delivered) noexcept;

/// 将 poll 结果约束为停驻不变量；参数保留给测试覆盖异常唤醒和系统错误边界。
[[nodiscard]] WorkerParkAction worker_park_action(
    bool stop_requested,
    int poll_result,
    int poll_error) noexcept;

/// 握手阶段限制读写等待；认证后只限制发送，允许主机在业务处理期间保持空闲。
[[nodiscard]] bool configure_rpc_socket_timeouts(
    int connection,
    RpcSocketPhase phase) noexcept;

/// 单会话 RPC 服务；逐个认证并服务连接，普通断线保留监听和运行态。
class RpcServer final {
public:
    /// 创建尚未绑定监听地址的服务对象。
    RpcServer() = default;
    /// 关闭仍由对象持有的监听描述符。
    ~RpcServer();

    /// 监听描述符所有权不可复制。
    RpcServer(const RpcServer&) = delete;
    RpcServer& operator=(const RpcServer&) = delete;

    /// 以独立通道标识绑定唯一 localabstract 名称并开始监听。
    bool bind_channel(const std::string& channel_id, std::string* error);
    /// 将监听描述符转交给单会话后台服务线程。
    bool start(AgentRuntime* runtime, std::string* error);
    /// 幂等请求后台线程停止监听，并中断仍在等待输入的活动连接。
    void close_listener() noexcept;
    /// 返回认证连接关闭后工作线程是否已经进入无锁停驻状态。
    [[nodiscard]] bool worker_parked() const noexcept;
    /// 返回 Linux 工作线程 ID。
    [[nodiscard]] std::int32_t worker_tid() const noexcept;
    /// 返回 `/proc` 记录的工作线程启动时刻。
    [[nodiscard]] std::uint64_t worker_start_time() const noexcept;

private:
    /// 运行单会话接受循环，并在认证连接关闭后停驻为卸载载体。
    void run(AgentRuntime* runtime) noexcept;
    /// 只在后台线程结束或尚未启动时关闭监听和唤醒描述符。
    void close_descriptors() noexcept;

    std::atomic<int> listener_{-1};
    std::atomic<int> stop_event_{-1};
    std::atomic<bool> stop_requested_{false};
    std::mutex connection_mutex_;
    int active_connection_ = -1;
    std::atomic<bool> worker_parked_{false};
    std::atomic<std::int32_t> worker_tid_{0};
    std::atomic<std::uint64_t> worker_start_time_{0};
    std::thread worker_;
};

}  // namespace azlw::agent

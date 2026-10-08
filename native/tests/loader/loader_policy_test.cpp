// 验证加载器始终使用 memfd 且关闭未登记的 AndKitty 行为开关。

#include <dlfcn.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <iostream>
#include <span>
#include <string>
#include <utility>
#include <vector>

#include "load_transaction.h"
#include "mapping/agent_mapping_policy.h"
#include "agent_unloader.h"
#include "process/injector_policy.h"
#include "process/remote_memory_evidence.h"
#include "process/thread_freeze.h"

namespace {

class FaultInjectingRemoteMemory final : public azlw::loader::RemoteMemoryEvidenceBackend {
public:
    explicit FaultInjectingRemoteMemory(std::vector<std::uint8_t> bytes)
        : bytes_(std::move(bytes)) {}

    std::size_t read(std::uintptr_t, void* output, std::size_t size) override {
        const std::size_t transferred = std::min({size, bytes_.size(), read_limit_});
        std::copy_n(bytes_.data(), transferred, static_cast<std::uint8_t*>(output));
        return transferred;
    }

    std::size_t write(std::uintptr_t, const void* input, std::size_t size) override {
        const std::size_t transferred = std::min({size, bytes_.size(), write_limit_});
        std::copy_n(static_cast<const std::uint8_t*>(input), transferred, bytes_.data());
        if (corrupt_after_write_ && transferred == size && !bytes_.empty()) {
            bytes_[0] ^= 0xff;
        }
        return transferred;
    }

    void set_read_limit(std::size_t limit) { read_limit_ = limit; }
    void set_write_limit(std::size_t limit) { write_limit_ = limit; }
    void set_corrupt_after_write(bool value) { corrupt_after_write_ = value; }
    void replace(std::span<const std::uint8_t> bytes) {
        bytes_.assign(bytes.begin(), bytes.end());
    }

private:
    std::vector<std::uint8_t> bytes_;
    std::size_t read_limit_ = static_cast<std::size_t>(-1);
    std::size_t write_limit_ = static_cast<std::size_t>(-1);
    bool corrupt_after_write_ = false;
};

bool remote_memory_evidence_rejects_incomplete_or_unverified_changes() {
    constexpr std::uintptr_t kAddress = 0x7000;
    const std::array<std::uint8_t, 8> original{1, 2, 3, 4, 5, 6, 7, 8};
    const std::array<std::uint8_t, 8> zeros{};
    std::string error;

    FaultInjectingRemoteMemory backend(
        std::vector<std::uint8_t>(original.begin(), original.end()));
    backend.set_write_limit(0);
    if (azlw::loader::write_and_verify_remote_memory(&backend, kAddress, zeros, &error) ||
        error.find("actual=0") == std::string::npos) {
        return false;
    }

    backend.replace(original);
    backend.set_write_limit(zeros.size() - 1);
    if (azlw::loader::write_and_verify_remote_memory(&backend, kAddress, zeros, &error) ||
        error.find("actual=7") == std::string::npos) {
        return false;
    }

    backend.replace(original);
    backend.set_write_limit(zeros.size());
    backend.set_corrupt_after_write(true);
    if (azlw::loader::write_and_verify_remote_memory(&backend, kAddress, zeros, &error) ||
        error.find("内容与预期不一致") == std::string::npos) {
        return false;
    }

    backend.set_corrupt_after_write(false);
    backend.replace(zeros);
    backend.set_read_limit(original.size() - 1);
    if (azlw::loader::verify_remote_memory(&backend, kAddress, original, &error) ||
        error.find("actual=7") == std::string::npos) {
        return false;
    }

    backend.set_read_limit(original.size());
    backend.replace(zeros);
    if (azlw::loader::verify_remote_memory(&backend, kAddress, original, &error) ||
        error.find("内容与预期不一致") == std::string::npos) {
        return false;
    }

    backend.replace(original);
    if (!azlw::loader::write_and_verify_remote_memory(&backend, kAddress, zeros, &error) ||
        !azlw::loader::verify_remote_memory(&backend, kAddress, zeros, &error)) {
        return false;
    }
    return true;
}

}  // namespace

/// 核对固定 SDK、超时、加载标志和全部策略开关。
int main() {
    constexpr int kSdk = 32;
    constexpr std::uint32_t kTimeoutMs = 10'000;
    const std::string memfd_name = "00112233445566778899aabbccddeeff";
    const inject_elf_config_t config =
        azlw::loader::make_injector_config(kSdk, kTimeoutMs, memfd_name);

    if (config.sdk != kSdk || config.timeout != static_cast<int>(kTimeoutMs) ||
        config.rtdl_flags != (RTLD_LOCAL | RTLD_NOW) || !config.memfd || config.watch ||
        config.launch || config.seize || config.bp || config.free || config.hide ||
        config.memfd_name != memfd_name) {
        std::cerr << "FAIL loader_policy_test: AndKitty 配置偏离加载器固定契约\n";
        return 1;
    }

    using namespace std::chrono_literals;
    using Deadline = azlw::loader::UnloadDeadline;
    const Deadline::TimePoint start = Deadline::TimePoint{} + 10s;
    const Deadline deadline(100, start);
    const Deadline expired(0, start);
    if (deadline.remaining(start) != 100 || deadline.remaining(start + 50ms) != 50 ||
        deadline.remaining(start + 99ms + 999us) != 1 ||
        deadline.remaining(start + 100ms) != 0 || deadline.remaining(start + 101ms) != 0 ||
        expired.remaining(start) != 0) {
        std::cerr << "FAIL loader_policy_test: 卸载阶段没有共享严格递减的总时限\n";
        return 1;
    }

    using azlw::loader::DetachExceptResult;
    using azlw::loader::DetachExceptStatus;
    using azlw::loader::DlcloseCarrierSelection;
    if (azlw::loader::select_dlclose_carrier(DetachExceptResult{
            .status = DetachExceptStatus::ReadyForCarrierDlclose,
        }) != DlcloseCarrierSelection::RetainedWorker ||
        azlw::loader::select_dlclose_carrier(DetachExceptResult{
            .status = DetachExceptStatus::CarrierExitedBeforeDlclose,
            .wait_status = 0,
        }) != DlcloseCarrierSelection::Reject ||
        azlw::loader::select_dlclose_carrier(DetachExceptResult{
            .status = DetachExceptStatus::CarrierExitedBeforeDlclose,
            .wait_status = 1 << 8,
        }) != DlcloseCarrierSelection::Reject ||
        azlw::loader::select_dlclose_carrier(DetachExceptResult{
            .status = DetachExceptStatus::Failed,
        }) != DlcloseCarrierSelection::Reject) {
        std::cerr << "FAIL loader_policy_test: dlclose 接受了不再停驻的载体\n";
        return 1;
    }

    using azlw::loader::AgentMappingDisposition;
    constexpr azlw::loader::AgentMappingIdentity inert_mapping{
        .start_address = 0x5800,
        .end_address = 0x5900,
        .readable = false,
        .writeable = false,
        .executable = false,
        .is_private = true,
        .is_shared = false,
        .offset = 0,
        .device = "00:00",
        .inode = 0,
        .pathname = "",
    };
    static_assert(azlw::loader::is_inert_anonymous_mapping(inert_mapping));
    static_assert(
        azlw::loader::classify_agent_mapping(
            inert_mapping, 0x5000, 0x6000, "/memfd:fixture") ==
        AgentMappingDisposition::InertAnonymousOverlap);
    auto exact_mapping = inert_mapping;
    exact_mapping.start_address = 0x7000;
    exact_mapping.end_address = 0x8000;
    exact_mapping.pathname = "/memfd:fixture";
    auto deleted_exact_mapping = exact_mapping;
    deleted_exact_mapping.pathname = "/memfd:fixture (deleted)";
    auto plain_exact_mapping = exact_mapping;
    plain_exact_mapping.pathname = "/memfd:fixture";
    auto adjacent_mapping = inert_mapping;
    adjacent_mapping.start_address = 0x4000;
    adjacent_mapping.end_address = 0x5000;
    auto accessible_mapping = inert_mapping;
    accessible_mapping.readable = true;
    auto shared_mapping = inert_mapping;
    shared_mapping.is_private = false;
    shared_mapping.is_shared = true;
    auto file_backed_mapping = inert_mapping;
    file_backed_mapping.device = "08:06";
    auto named_mapping = inert_mapping;
    named_mapping.pathname = "[anon:stack_and_tls]";
    auto offset_mapping = inert_mapping;
    offset_mapping.offset = 0x1000;
    auto live_anonymous_mapping = inert_mapping;
    live_anonymous_mapping.readable = true;
    live_anonymous_mapping.executable = true;
    auto live_bss_mapping = live_anonymous_mapping;
    live_bss_mapping.executable = false;
    live_bss_mapping.writeable = true;
    live_bss_mapping.pathname = "[anon:.bss]";
    auto named_live_mapping = live_anonymous_mapping;
    named_live_mapping.pathname = "[anon:other]";
    if (azlw::loader::classify_agent_mapping(
            exact_mapping, 0x5000, 0x6000, "/memfd:fixture") !=
            AgentMappingDisposition::Reject ||
        azlw::loader::classify_agent_mapping(
            deleted_exact_mapping, 0x5000, 0x6000, "/memfd:fixture") !=
            AgentMappingDisposition::Reject ||
        azlw::loader::classify_agent_mapping(
            plain_exact_mapping, 0x5000, 0x6000, "/memfd:fixture (deleted)") !=
            AgentMappingDisposition::Reject ||
        azlw::loader::classify_agent_mapping(
            adjacent_mapping, 0x5000, 0x6000, "/memfd:fixture") !=
            AgentMappingDisposition::Unrelated ||
        azlw::loader::is_inert_anonymous_mapping(accessible_mapping) ||
        azlw::loader::is_inert_anonymous_mapping(shared_mapping) ||
        azlw::loader::is_inert_anonymous_mapping(file_backed_mapping) ||
        azlw::loader::is_inert_anonymous_mapping(named_mapping) ||
        azlw::loader::is_inert_anonymous_mapping(offset_mapping) ||
        !azlw::loader::is_live_anonymous_agent_mapping(live_anonymous_mapping) ||
        !azlw::loader::is_live_anonymous_agent_mapping(live_bss_mapping) ||
        azlw::loader::is_live_anonymous_agent_mapping(named_live_mapping) ||
        azlw::loader::is_live_anonymous_agent_mapping(offset_mapping)) {
        std::cerr << "FAIL loader_policy_test: Agent 卸载映射身份边界过宽\n";
        return 1;
    }

    if (!remote_memory_evidence_rejects_incomplete_or_unverified_changes()) {
        std::cerr << "FAIL loader_policy_test: 远程内存证据接受了部分或未验证的传输\n";
        return 1;
    }

    using azlw::loader::FailedLoadCleanup;
    using azlw::loader::LoadTransaction;
    LoadTransaction not_started;
    LoadTransaction started;
    started.agent_started = true;
    LoadTransaction unknown;
    unknown.agent_start_state_unknown = true;
    LoadTransaction unknown_and_started = unknown;
    unknown_and_started.agent_started = true;
    if (not_started.failed_cleanup() != FailedLoadCleanup::RestoreUnstartedAgent ||
        started.failed_cleanup() != FailedLoadCleanup::PreserveUnresolvedScene ||
        unknown.failed_cleanup() != FailedLoadCleanup::PreserveUnresolvedScene ||
        unknown_and_started.failed_cleanup() != FailedLoadCleanup::PreserveUnresolvedScene ||
        std::string(unknown.preserve_scene_summary()) != "Agent 启动状态未知" ||
        std::string(started.preserve_scene_summary()) != "Agent 已启动但远程清理证据不完整") {
        std::cerr << "FAIL loader_policy_test: 启动状态未知被当成尚未启动\n";
        return 1;
    }
    LoadTransaction scratch = not_started;
    scratch.remote_bootstrap_write_attempted = true;
    scratch.remote_bootstrap_scrubbed = true;
    scratch.injector_stack_snapshot_ready = true;
    scratch.injector_stack_restored = true;
    if (!scratch.remote_scratch_ready() || scratch.load_completed(true)) {
        std::cerr << "FAIL loader_policy_test: 远程暂存证据或加载完成条件偏离事务事实\n";
        return 1;
    }
    scratch.callback_invoked = true;
    scratch.agent_started = true;
    scratch.remote_scratch_evidence_failed = true;
    if (scratch.remote_scratch_ready() || scratch.load_completed(true) ||
        scratch.failed_cleanup() != FailedLoadCleanup::PreserveUnresolvedScene) {
        std::cerr << "FAIL loader_policy_test: 已启动但证据失败仍允许按未启动清理\n";
        return 1;
    }

    using azlw::loader::AgentUnloadStatus;
    using azlw::loader::UnloadClosePath;
    using azlw::loader::UnloadTransaction;
    UnloadTransaction unload;
    const auto missing = unload.result(
        AgentUnloadStatus::ConfigInvalid, "unload_config_invalid", "配置缺失");
    unload.process_id = 42;
    unload.close_path = UnloadClosePath::CarrierExitedDuringClose;
    const auto unloaded = unload.result(
        AgentUnloadStatus::Ok, "unloaded", unload.success_message(2));
    if (missing.process_id != 0 || missing.stable_code != "unload_config_invalid" ||
        unloaded.process_id != 42 || unloaded.status != AgentUnloadStatus::Ok ||
        unloaded.message.find("载体线程在 dlclose 期间正常退出") == std::string::npos ||
        unloaded.message.find("2 个不可访问匿名占位") == std::string::npos) {
        std::cerr << "FAIL loader_policy_test: 卸载结果没有沿用事务里的目标和关闭路径\n";
        return 1;
    }

    std::cout << "PASS loader_policy_test\n";
    return 0;
}

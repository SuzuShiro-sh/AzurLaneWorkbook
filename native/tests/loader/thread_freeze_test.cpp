// 在独立子进程中验证冻结、远程调用和恢复，不依赖游戏或 Agent。
#include "process/remote_call_stack.h"
#include "process/thread_freeze.h"
#include "process/tracee_ptrace.h"

#include <array>
#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <string>
#include <thread>

#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include <KittyMemoryMgr.hpp>
#include "Injector/KittyInjectorSyscall.hpp"

namespace {
constexpr unsigned kTimeoutMs = 10000;
constexpr std::uint64_t kMarker = 0x13579bdf2468ace0ULL;

struct Worker {
    int input;
    int reports;
    int role;
};
struct Report {
    int role;
    pid_t tid;
    long result;
    bool stack_intact;
    bool simd_intact;
};

bool transfer(int fd, void* data, std::size_t bytes, bool writing) {
    auto* cursor = static_cast<char*>(data);
    while (bytes != 0) {
        if (!writing) {
            pollfd item{fd, POLLIN, 0};
            if (poll(&item, 1, kTimeoutMs) != 1) {
                return false;
            }
        }
        const ssize_t count = writing ? write(fd, cursor, bytes) : read(fd, cursor, bytes);
        if (count < 0 && errno == EINTR) {
            continue;
        }
        if (count <= 0) {
            return false;
        }
        cursor += count;
        bytes -= static_cast<std::size_t>(count);
    }
    return true;
}

void* blocking_worker(void* argument) {
    const auto& worker = *static_cast<Worker*>(argument);
    volatile std::uint64_t stack[64];
    for (std::size_t i = 0; i < 64; ++i) {
        stack[i] = kMarker + i;
    }
    Report report{worker.role, static_cast<pid_t>(syscall(SYS_gettid)), 0, true, true};
    if (!transfer(worker.reports, &report, sizeof(report), true)) {
        _exit(91);
    }
    const std::array<std::uint64_t, 2> expected{kMarker, ~kMarker};
    std::array<std::uint64_t, 2> observed{};
    char byte = 0;
    long result = SYS_read;
    // 将 SIMD 哨兵和原始 read 放在同一汇编块中，避免编译器在阻塞点间复用寄存器。
    asm volatile("movdqu %[expected], %%xmm15\n\t"
                 "syscall\n\t"
                 "movdqu %%xmm15, %[observed]"
                 : "+a"(result), [observed] "=m"(observed)
                 : "D"(static_cast<long>(worker.input)), "S"(&byte), "d"(1L),
                   [expected] "m"(expected)
                 : "rcx", "r11", "xmm15", "memory");
    if (worker.role == 1 && result == 1 && byte == 'q') {
        syscall(SYS_exit_group, 0);
        _exit(93);
    }
    report.result = result;
    report.simd_intact = observed == expected;
    report.stack_intact = byte == 'x';
    for (std::size_t i = 0; i < 64; ++i) {
        report.stack_intact = report.stack_intact && stack[i] == kMarker + i;
    }
    transfer(worker.reports, &report, sizeof(report), true);
    return nullptr;
}

// fork 保留函数地址；远程函数主动改变 SIMD 状态并使用栈，检验真实恢复责任。
__attribute__((noinline)) std::uintptr_t remote_probe() {
    volatile std::uint64_t stack[256];
    for (std::size_t i = 0; i < 256; ++i) {
        stack[i] = kMarker + i;
    }
    asm volatile("pxor %%xmm15, %%xmm15" ::: "xmm15");
    return stack[255];
}

struct Child {
    pid_t pid = -1;
    int reports[2]{-1, -1};
    int input[3][2]{{-1, -1}, {-1, -1}, {-1, -1}};
    ~Child() {
        if (pid > 0) {
            kill(pid, SIGKILL);
            int status = 0;
            while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {}
        }
        for (int fd : reports) {
            if (fd >= 0) close(fd);
        }
        for (auto& pair : input) {
            for (int fd : pair) {
                if (fd >= 0) close(fd);
            }
        }
    }
    bool start() {
        if (pipe(reports) != 0) return false;
        for (auto& pair : input) {
            if (pipe(pair) != 0) return false;
        }
        pid = fork();
        if (pid != 0) return pid > 0;
        Worker workers[3]{{input[0][0], reports[1], 0},
                          {input[1][0], reports[1], 1},
                          {input[2][0], reports[1], 2}};
        pthread_t threads[2];
        if (pthread_create(&threads[0], nullptr, blocking_worker, &workers[1]) != 0 ||
            pthread_create(&threads[1], nullptr, blocking_worker, &workers[2]) != 0) {
            _exit(92);
        }
        blocking_worker(&workers[0]);
        pthread_join(threads[0], nullptr);
        // 载体用例直接执行线程 exit，不走 pthread 清理；主进程由父进程收尾。
        for (;;) pause();
    }
};

bool wait_for_read(pid_t pid, pid_t tid, int fd) {
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
    do {
        std::ifstream status("/proc/" + std::to_string(pid) + "/task/" +
                             std::to_string(tid) + "/syscall");
        long number = -1;
        unsigned long first_argument = 0;
        if (status >> number >> std::hex >> first_argument &&
            number == SYS_read && first_argument == static_cast<unsigned long>(fd)) {
            return true;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    } while (std::chrono::steady_clock::now() < deadline);
    return false;
}

bool remote_call(KittyMemoryMgr* memory, KittyRemoteSys* remote, std::string* error) {
    user_regs_struct before{}, after{};
    if (!memory->trace.getRegs(&before)) return false;
    azlw::loader::RemoteCallStack stack;
    if (!stack.prepare(memory, remote, 64 * 1024, error)) return false;
    const auto result = memory->trace.callFunctionFrom(
        0, reinterpret_cast<std::uintptr_t>(&remote_probe));
    if (!stack.restore_and_release(error)) return false;
    if (!memory->trace.getRegs(&after)) return false;
    if (result.status != KT_RP_CALL_SUCCESS || result.result.val != kMarker + 255 ||
        std::memcmp(&before, &after, sizeof(before)) != 0 || stack.has_remote_mapping()) {
        *error = "远程调用结果、通用寄存器或栈映射恢复不匹配";
        return false;
    }
    return true;
}

struct GroupExitObservation {
    pid_t main_tid = 0;
    pid_t worker_tid = 0;
    pid_t carrier_tid = 0;
    bool observed = false;
};
GroupExitObservation group_exit;

char task_state(pid_t pid, pid_t tid) {
    KittyMemoryEx::ProcStatus status{};
    if (!KittyMemoryEx::ProcStatus::parse(pid, tid, &status)) return '?';
    const std::string state = status.getString("State");
    return state.empty() ? '?' : state.front();
}

void observe_group_exit_after_detach(int operation, pid_t tid, long result) {
    if (operation != PTRACE_DETACH || tid != group_exit.worker_tid || result != 0) return;
    // 仅延迟真实 detach 的返回，让工作线程的 exit_group 进入可观测终态。
    const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
    char main_state = '?';
    char carrier_state = '?';
    do {
        main_state = task_state(group_exit.main_tid, group_exit.main_tid);
        carrier_state = task_state(group_exit.main_tid, group_exit.carrier_tid);
        if ((main_state == 'X' || main_state == 'Z') &&
            (carrier_state == 'X' || carrier_state == 'Z')) {
            group_exit.observed = true;
            break;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    } while (std::chrono::steady_clock::now() < deadline);
    std::printf("group_exit_after_detach: worker=%d main=%d state=%c carrier=%d state=%c\n",
                tid, group_exit.main_tid, main_state, group_exit.carrier_tid, carrier_state);
}

struct FreezeOrderObservation {
    pid_t main_tid = 0;
    std::array<pid_t, 2> workers{};
    bool attach_checked = false;
    unsigned detach_checked = 0;
    bool valid = true;
};
FreezeOrderObservation freeze_order;

void observe_freeze_order(int operation, pid_t tid, long result) {
    if (result != 0) return;
    if (operation == PTRACE_SEIZE && tid == freeze_order.main_tid) {
        freeze_order.attach_checked = true;
        for (const pid_t worker : freeze_order.workers) {
            const char state = task_state(freeze_order.main_tid, worker);
            freeze_order.valid = freeze_order.valid && state == 't';
            std::printf("main_seized: worker=%d state=%c\n", worker, state);
        }
    }
    if (operation == PTRACE_DETACH && tid != freeze_order.main_tid) {
        KittyMemoryEx::ProcStatus status{};
        const bool parsed = KittyMemoryEx::ProcStatus::parse(
            freeze_order.main_tid, freeze_order.main_tid, &status);
        const long long tracer = parsed ? status.getInt("TracerPid") : -1;
        freeze_order.valid = freeze_order.valid && parsed && tracer == 0;
        ++freeze_order.detach_checked;
        std::printf("worker_detached: tid=%d main_tracer=%lld\n", tid, tracer);
    }
}

struct PtraceHookScope {
    ~PtraceHookScope() { azlw::loader::clear_ptrace_test_hooks(); }
};

enum class Scenario { DetachAll, RetainedWorker, CarrierExit, GroupExit };

bool run(Scenario scenario, std::string* error) {
    Child child;
    if (!child.start()) return false;
    pid_t tids[3]{};
    for (int i = 0; i < 3; ++i) {
        Report report{};
        if (!transfer(child.reports[0], &report, sizeof(report), false) ||
            report.role < 0 || report.role > 2) return false;
        tids[report.role] = report.tid;
    }
    for (int role = 0; role < 3; ++role) {
        if (!wait_for_read(child.pid, tids[role], child.input[role][0])) {
            *error = "未观察到线程进入原始 read 系统调用";
            return false;
        }
    }
    PtraceHookScope hook_scope;
    const bool check_order = scenario == Scenario::DetachAll || scenario == Scenario::RetainedWorker;
    if (check_order) {
        freeze_order = {child.pid, {tids[1], tids[2]}, false, 0, true};
        if (!azlw::loader::install_ptrace_test_hooks(nullptr, observe_freeze_order)) return false;
    }
    azlw::loader::ThreadFreeze freeze;
    if (!freeze.attach_all(child.pid, kTimeoutMs, error)) return false;
    KittyMemoryMgr memory;
    memory.trace = KittyTraceMgr(child.pid, 0, true, kTimeoutMs);
    if (!memory.initialize(child.pid, EK_MEM_OP_SYSCALL, false)) return false;
    KittyRemoteSys remote;
    if (!remote.init(&memory) || !remote_call(&memory, &remote, error)) return false;

    if (scenario == Scenario::GroupExit) {
        char command = 'q';
        if (!transfer(child.input[1][1], &command, 1, true)) return false;
        group_exit = {child.pid, tids[1], tids[2], false};
        if (!azlw::loader::install_ptrace_test_hooks(nullptr, observe_group_exit_after_detach)) {
            *error = "无法安装 group exit 时序观察器";
            return false;
        }
        const auto detached = freeze.detach_all_except(tids[2], kTimeoutMs, error);
        azlw::loader::clear_ptrace_test_hooks();
        const auto selected = azlw::loader::select_dlclose_carrier(detached);
        user_regs_struct registers{};
        errno = 0;
        const bool readable = memory.trace.getRegs(&registers);
        const int register_error = errno;
        std::printf("group_exit_result: detach_status=%d wait_status=%d selection=%d "
                    "main_getregs=%d errno=%d detail=%s\n",
                    static_cast<int>(detached.status), detached.wait_status,
                    static_cast<int>(selected), readable, register_error, error->c_str());
        if (!group_exit.observed) {
            *error = "未观察到 group exit 的主线程和载体终态";
            return false;
        }
        if (selected != azlw::loader::DlcloseCarrierSelection::Reject) {
            *error = "整个目标退出后仍选择了可执行 dlclose 的载体";
            return false;
        }
        return true;
    }

    if (scenario == Scenario::CarrierExit) {
        // 保留真实线程终态给 detach_all_except 消费；不模拟 waitpid 或 ptrace。
        KittyTraceMgr carrier(tids[2], 0, false, kTimeoutMs, memory.trace.syscallGadget());
        user_regs_struct registers{};
        if (!carrier.getRegs(&registers)) return false;
        registers.rip = memory.trace.syscallGadget();
        registers.rax = SYS_exit;
        registers.orig_rax = static_cast<unsigned long long>(-1);
        registers.rdi = 0;
        if (registers.rip == 0 || !carrier.setRegs(&registers) ||
            ptrace(PTRACE_CONT, tids[2], nullptr, nullptr) != 0) return false;
        siginfo_t info{};
        const auto deadline = std::chrono::steady_clock::now() + std::chrono::seconds(3);
        do {
            if (waitid(P_PID, tids[2], &info, WEXITED | WNOHANG | WNOWAIT | __WALL) != 0) {
                *error = "观察载体终态失败: " + std::string(std::strerror(errno));
                return false;
            }
            if (info.si_pid == tids[2]) break;
            std::this_thread::sleep_for(std::chrono::milliseconds(1));
        } while (std::chrono::steady_clock::now() < deadline);
        if (info.si_pid != tids[2] || info.si_code != CLD_EXITED || info.si_status != 0) {
            *error = "载体未进入 exit(0) 终态";
            return false;
        }
    }
    if (scenario != Scenario::DetachAll) {
        const auto detached = freeze.detach_all_except(tids[2], kTimeoutMs, error);
        const auto expected = scenario == Scenario::CarrierExit
            ? azlw::loader::DlcloseCarrierSelection::Reject
            : azlw::loader::DlcloseCarrierSelection::RetainedWorker;
        if (azlw::loader::select_dlclose_carrier(detached) != expected) return false;
        if (scenario == Scenario::CarrierExit &&
            (detached.status != azlw::loader::DetachExceptStatus::CarrierExitedBeforeDlclose ||
             detached.wait_status != 0)) return false;
    }
    if (!freeze.detach_all(error)) return false;
    if (check_order && (!freeze_order.valid || !freeze_order.attach_checked ||
                        freeze_order.detach_checked != 2)) {
        *error = "主线程跟踪期间存在未停止或已恢复的工作线程";
        return false;
    }
    const int count = scenario == Scenario::CarrierExit ? 2 : 3;
    for (int role = 0; role < count; ++role) {
        char byte = 'x';
        if (!transfer(child.input[role][1], &byte, 1, true)) return false;
    }
    unsigned received = 0;
    for (int i = 0; i < count; ++i) {
        Report report{};
        if (!transfer(child.reports[0], &report, sizeof(report), false)) {
            *error = "分离后子进程未返回 read 完成报告";
            return false;
        }
        std::printf("role=%d tid=%d read=%ld stack_intact=%d simd_intact=%d\n",
                    report.role, report.tid, report.result, report.stack_intact,
                    report.simd_intact);
        if (report.role < 0 || report.role >= count ||
            (received & (1U << report.role)) || report.result != 1 ||
            !report.stack_intact || !report.simd_intact) {
            *error = "分离后原始 read、栈或 SIMD 哨兵损坏";
            return false;
        }
        received |= 1U << report.role;
    }
    return kill(child.pid, 0) == 0;
}
}  // namespace

int main() {
    const std::array scenarios{Scenario::DetachAll, Scenario::RetainedWorker,
                              Scenario::CarrierExit, Scenario::GroupExit};
    const char* names[]{"detach_all", "retained_worker", "carrier_exit_rejected", "group_exit"};
    bool success = true;
    for (std::size_t i = 0; i < scenarios.size(); ++i) {
        std::string error;
        const bool passed = run(scenarios[i], &error);
        std::printf("%s: %s %s\n", names[i], passed ? "PASS" : "FAIL", error.c_str());
        success = success && passed;
    }
    return success ? 0 : 1;
}

// 声明指定 ptrace 载体终态的作用域观察接口。

#pragma once

#include <sys/types.h>

namespace azlw::loader {

/// 保存一次指定 TID 的 waitpid 终态；未观察到终态时 observed 为 false。
struct TraceeWaitStatus final {
    bool observed = false;
    int value = 0;
};

/// 在当前调用线程内观察指定 TID 的退出或信号终态，不改变 waitpid 行为。
class TraceeWaitStatusCapture final {
public:
    explicit TraceeWaitStatusCapture(pid_t target_tid) noexcept;
    ~TraceeWaitStatusCapture();

    TraceeWaitStatusCapture(const TraceeWaitStatusCapture&) = delete;
    TraceeWaitStatusCapture& operator=(const TraceeWaitStatusCapture&) = delete;

    /// 只有参数有效且当前线程不存在其他观察器时才会成功启用。
    bool active() const noexcept;
    /// 返回已观察的终态并立即关闭当前观察器。
    TraceeWaitStatus finish() noexcept;

private:
    bool active_ = false;
};

}  // namespace azlw::loader

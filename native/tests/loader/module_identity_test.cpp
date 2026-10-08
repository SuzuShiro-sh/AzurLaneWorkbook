// 验证进程身份读取的中断恢复、失败分类和原有包名边界。

#include <array>
#include <cerrno>
#include <iostream>
#include <stdexcept>
#include <string>
#include <unistd.h>

#include "mapping/module_identity.h"

namespace {
enum class ReadFault { None, InterruptOnce, IoError, Empty };
ReadFault fault = ReadFault::None;
int injected_reads = 0;

bool is_cmdline(int descriptor) {
    const std::string path = "/proc/self/fd/" + std::to_string(descriptor);
    std::array<char, 256> target{};
    const ssize_t size = readlink(path.c_str(), target.data(), target.size());
    return size > 0 && std::string(target.data(), static_cast<std::size_t>(size)).ends_with("/cmdline");
}

void require(bool condition, const std::string& message) {
    if (!condition) {
        throw std::runtime_error(message);
    }
}
}  // namespace

extern "C" ssize_t __real_read(int descriptor, void* buffer, size_t count);
extern "C" ssize_t __wrap_read(int descriptor, void* buffer, size_t count) {
    if (fault != ReadFault::None && is_cmdline(descriptor)) {
        ++injected_reads;
        switch (fault) {
            case ReadFault::InterruptOnce:
                fault = ReadFault::None;
                errno = EINTR;
                return -1;
            case ReadFault::IoError:
                errno = EIO;
                return -1;
            case ReadFault::Empty:
                return 0;
            case ReadFault::None:
                break;
        }
    }
    return __real_read(descriptor, buffer, count);
}

int main(int, char** arguments) {
    try {
        std::string error;
        const std::string name = arguments[0];
        require(azlw::loader::verify_process_identity(getpid(), name, &error), error);
        fault = ReadFault::InterruptOnce;
        require(azlw::loader::verify_process_identity(getpid(), name, &error), error);
        require(injected_reads == 1, "EINTR 应只重做同一次读取");

        fault = ReadFault::IoError;
        error.clear();
        require(!azlw::loader::verify_process_identity(getpid(), name, &error), "读取失败应拒绝身份");
        require(error.find("read_count=-1") != std::string::npos &&
                    error.find("errno=" + std::to_string(EIO)) != std::string::npos,
                "读取失败应保留 errno 和长度: " + error);
        require(injected_reads == 2, "非 EINTR 错误不应重试");

        fault = ReadFault::Empty;
        error.clear();
        require(!azlw::loader::verify_process_identity(getpid(), name, &error), "空身份应拒绝");
        require(error.find("read_count=0") != std::string::npos && error.find("errno=") == std::string::npos,
                "空值应独立于读取错误: " + error);

        fault = ReadFault::None;
        require(!azlw::loader::verify_process_identity(getpid(), name + ".other", &error),
                "包名不匹配应拒绝");
        require(!azlw::loader::verify_process_identity(-1, name, &error), "缺失进程应拒绝");
        require(error.find("打开目标 cmdline 失败") != std::string::npos, error);
        std::cout << "PASS module_identity_test\n";
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "FAILED: " << error.what() << '\n';
        return 1;
    }
}

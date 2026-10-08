// 使用 Android 全版本可用的系统随机设备生成握手随机数。

#include "secure_random.h"

#include <cerrno>
#include <cstring>
#include <fcntl.h>
#include <string>
#include <unistd.h>

#include "secure_memory.h"

namespace azlw::agent {

bool fill_secure_random(std::span<std::uint8_t> output, std::string* error) noexcept {
    if (output.empty()) {
        if (error != nullptr) {
            *error = "安全随机输出缓冲区为空";
        }
        return false;
    }
    const int descriptor = open("/dev/urandom", O_RDONLY | O_CLOEXEC);
    if (descriptor < 0) {
        if (error != nullptr) {
            *error = "打开系统随机设备失败: " + std::string(std::strerror(errno));
        }
        return false;
    }

    std::size_t offset = 0;
    int failure = 0;
    while (offset < output.size()) {
        const ssize_t count = read(descriptor, output.data() + offset, output.size() - offset);
        if (count > 0) {
            offset += static_cast<std::size_t>(count);
            continue;
        }
        if (count < 0 && errno == EINTR) {
            continue;
        }
        failure = count == 0 ? EIO : errno;
        break;
    }
    if (close(descriptor) != 0 && failure == 0) {
        failure = errno;
    }
    if (failure != 0) {
        secure_zero(output.data(), output.size());
        if (error != nullptr) {
            *error = "读取系统随机设备失败: " + std::string(std::strerror(failure));
        }
        return false;
    }
    if (error != nullptr) {
        error->clear();
    }
    return true;
}

}  // namespace azlw::agent

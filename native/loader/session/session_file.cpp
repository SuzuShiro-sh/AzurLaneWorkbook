// 实现 root 一次性会话文件的符号链接拒绝、权限校验和幂等删除。

#include "session_file.h"

#include <cerrno>
#include <cstdint>
#include <cstring>
#include <fcntl.h>
#include <sys/stat.h>
#include <unistd.h>

namespace azlw::loader {
namespace {

/// 处理短读和信号中断，直到固定长度内容全部读入。
bool read_exact(int descriptor, void* destination, std::size_t size, std::string* error) {
    auto* output = static_cast<std::uint8_t*>(destination);
    std::size_t offset = 0;
    while (offset < size) {
        const ssize_t count = read(descriptor, output + offset, size - offset);
        if (count == 0) {
            *error = "会话文件提前结束";
            return false;
        }
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            *error = std::string("读取会话文件失败: ") + std::strerror(errno);
            return false;
        }
        offset += static_cast<std::size_t>(count);
    }
    return true;
}

}  // namespace

bool read_root_session_file(
    const std::string& path,
    void* destination,
    std::size_t size,
    std::string* owned_path,
    std::string* error) {
    if (path.empty() || destination == nullptr || size == 0 || !owned_path->empty()) {
        *error = "会话文件读取参数无效";
        return false;
    }
    const int descriptor = open(path.c_str(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        *error = std::string("打开会话文件失败: ") + std::strerror(errno);
        return false;
    }
    *owned_path = path;

    struct stat info {};
    if (fstat(descriptor, &info) != 0) {
        *error = std::string("读取会话文件属性失败: ") + std::strerror(errno);
        close(descriptor);
        return false;
    }
    if (!S_ISREG(info.st_mode) || info.st_uid != 0 || (info.st_mode & 0777) != 0600 ||
        info.st_size != static_cast<off_t>(size)) {
        *error = "会话文件必须是 root 拥有、权限为 0600 且长度精确的普通文件";
        close(descriptor);
        return false;
    }

    const bool read_ok = read_exact(descriptor, destination, size, error);
    const int close_result = close(descriptor);
    if (!read_ok) {
        return false;
    }
    if (close_result != 0) {
        *error = std::string("关闭会话文件失败: ") + std::strerror(errno);
        return false;
    }
    return true;
}

bool remove_root_session_file(std::string* owned_path, std::string* error) {
    if (owned_path->empty()) {
        return true;
    }
    if (unlink(owned_path->c_str()) != 0 && errno != ENOENT) {
        *error = std::string("删除会话文件失败: ") + std::strerror(errno);
        return false;
    }
    owned_path->clear();
    return true;
}

}  // namespace azlw::loader

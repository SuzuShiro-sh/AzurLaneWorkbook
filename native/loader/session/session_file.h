// 声明 root 一次性会话文件的安全读取和确定性删除基础能力。

#pragma once

#include <cstddef>
#include <string>

namespace azlw::loader {

/// 拒绝符号链接，核对 root 所有权、0600 边界和精确长度后完整读取文件。
bool read_root_session_file(
    const std::string& path,
    void* destination,
    std::size_t size,
    std::string* owned_path,
    std::string* error);

/// 删除已取得所有权的一次性文件；成功后清空路径以保证幂等。
bool remove_root_session_file(std::string* owned_path, std::string* error);

}  // namespace azlw::loader

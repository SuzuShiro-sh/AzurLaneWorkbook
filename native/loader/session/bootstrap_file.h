// 声明一次性启动文件的安全读取、校验、删除和敏感内容清理接口。

#pragma once

#include <string>

#include "bootstrap_config.h"

namespace azlw::loader {

/// 持有一次性会话文件；读取成功后无论后续结果如何都会删除并擦除内容。
class BootstrapFile final {
public:
    /// 创建尚未绑定启动文件的空持有者。
    BootstrapFile() = default;
    /// 删除仍存在的启动文件，并擦除内存中的配置副本。
    ~BootstrapFile();

    /// 启动文件包含会话密钥，因此禁止复制所有权。
    BootstrapFile(const BootstrapFile&) = delete;
    BootstrapFile& operator=(const BootstrapFile&) = delete;

    /// 安全打开、精确读取并校验固定长度启动配置。
    bool load(const std::string& path, std::string* error);
    /// 立即删除已加载的启动文件，并解除析构器的路径责任。
    bool remove_now(std::string* error);

    /// 返回只读启动配置。
    [[nodiscard]] const BootstrapConfigV2& config() const noexcept;
    /// 返回仅供远程传输后擦除使用的可写配置。
    [[nodiscard]] BootstrapConfigV2& mutable_config() noexcept;
    /// 返回安全打开后记录的启动文件路径。
    [[nodiscard]] const std::string& path() const noexcept;

private:
    BootstrapConfigV2 config_{};
    std::string path_;
};

/// 将二进制匿名映射标识转换为 memfd 使用的小写十六进制文本。
[[nodiscard]] std::string mapping_id_hex(const BootstrapConfigV2& config);

}  // namespace azlw::loader

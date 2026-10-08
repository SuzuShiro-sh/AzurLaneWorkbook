// 声明一次性卸载文件的安全读取、校验、删除和内存清理接口。

#pragma once

#include <string>

#include "bootstrap_config.h"

namespace azlw::loader {

/// 持有一次性卸载配置；读取后所有退出路径都会删除文件并擦除地址信息。
class UnloadFile final {
public:
    UnloadFile() = default;
    ~UnloadFile();

    UnloadFile(const UnloadFile&) = delete;
    UnloadFile& operator=(const UnloadFile&) = delete;

    /// 安全打开、精确读取并校验固定 512 字节卸载配置。
    bool load(const std::string& path, std::string* error);
    /// 立即删除已加载的卸载文件，并解除析构器的路径责任。
    bool remove_now(std::string* error);

    [[nodiscard]] const AgentUnloadConfigV1& config() const noexcept;
    [[nodiscard]] const std::string& path() const noexcept;

private:
    AgentUnloadConfigV1 config_{};
    std::string path_;
};

}  // namespace azlw::loader

// 实现一次性启动文件的安全读取、严格字段校验和生命周期清理。

#include "bootstrap_file.h"

#include "bootstrap_validation.h"
#include "secure_memory.h"
#include "session_file.h"

namespace azlw::loader {

// 析构路径是所有提前返回的最后清理保障。
BootstrapFile::~BootstrapFile() {
    std::string ignored;
    remove_root_session_file(&path_, &ignored);
    secure_zero(&config_, sizeof(config_));
}

// 先使用 O_NOFOLLOW 打开，再核对所有权、权限、类型和固定长度。
bool BootstrapFile::load(const std::string& path, std::string* error) {
    return read_root_session_file(path, &config_, sizeof(config_), &path_, error) &&
           validate_bootstrap_config(config_, error);
}

// 显式删除成功后清空路径，防止析构器重复处理。
bool BootstrapFile::remove_now(std::string* error) {
    return remove_root_session_file(&path_, error);
}

// 暴露只读视图，保持默认调用路径不能改写会话字段。
const BootstrapConfigV2& BootstrapFile::config() const noexcept {
    return config_;
}

// 可写视图只用于远程传输及随后的确定性擦除。
BootstrapConfigV2& BootstrapFile::mutable_config() noexcept {
    return config_;
}

// 返回已通过安全打开流程记录的原始启动文件路径。
const std::string& BootstrapFile::path() const noexcept {
    return path_;
}

// 采用固定小写编码，确保 Loader 与 Agent 映射收据使用同一身份。
std::string mapping_id_hex(const BootstrapConfigV2& config) {
    return bootstrap_mapping_id_hex(config);
}

}  // namespace azlw::loader

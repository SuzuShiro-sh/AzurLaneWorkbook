// 实现一次性卸载文件的安全读取、严格校验和确定性清理。

#include "unload_file.h"

#include "bootstrap_validation.h"
#include "secure_memory.h"
#include "session_file.h"

namespace azlw::loader {

UnloadFile::~UnloadFile() {
    std::string ignored;
    remove_root_session_file(&path_, &ignored);
    secure_zero(&config_, sizeof(config_));
}

bool UnloadFile::load(const std::string& path, std::string* error) {
    return read_root_session_file(path, &config_, sizeof(config_), &path_, error) &&
           validate_unload_config(config_, error);
}

bool UnloadFile::remove_now(std::string* error) {
    return remove_root_session_file(&path_, error);
}

const AgentUnloadConfigV1& UnloadFile::config() const noexcept {
    return config_;
}

const std::string& UnloadFile::path() const noexcept {
    return path_;
}

}  // namespace azlw::loader

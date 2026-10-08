// 实现远程内存完整传输与写后读回证据，拒绝部分成功和静默降级。

#include "remote_memory_evidence.h"

#include <algorithm>
#include <vector>

#include "secure_memory.h"

namespace azlw::loader {
namespace {

void set_error(std::string* error, const std::string& message) {
    if (error != nullptr) {
        *error = message;
    }
}

}  // namespace

bool verify_remote_memory(
    RemoteMemoryEvidenceBackend* backend,
    std::uintptr_t address,
    std::span<const std::uint8_t> expected,
    std::string* error) {
    if (backend == nullptr || address == 0 || expected.empty()) {
        set_error(error, "远程内存比对参数无效");
        return false;
    }

    std::vector<std::uint8_t> observed(expected.size(), 0);
    const std::size_t read = backend->read(address, observed.data(), observed.size());
    if (read != observed.size()) {
        secure_zero(observed.data(), observed.size());
        set_error(
            error,
            "远程内存读取不完整: expected=" + std::to_string(expected.size()) +
                ", actual=" + std::to_string(read));
        return false;
    }

    const bool matches = std::equal(observed.begin(), observed.end(), expected.begin());
    secure_zero(observed.data(), observed.size());
    if (!matches) {
        set_error(error, "远程内存读回内容与预期不一致");
        return false;
    }
    return true;
}

bool write_and_verify_remote_memory(
    RemoteMemoryEvidenceBackend* backend,
    std::uintptr_t address,
    std::span<const std::uint8_t> expected,
    std::string* error) {
    if (backend == nullptr || address == 0 || expected.empty()) {
        set_error(error, "远程内存写入参数无效");
        return false;
    }

    const std::size_t written = backend->write(address, expected.data(), expected.size());
    if (written != expected.size()) {
        set_error(
            error,
            "远程内存写入不完整: expected=" + std::to_string(expected.size()) +
                ", actual=" + std::to_string(written));
        return false;
    }
    return verify_remote_memory(backend, address, expected, error);
}

}  // namespace azlw::loader

// 实现 AndKitty linker 快照与精确指针读写后端。

#include "solist_visibility_kitty.h"

#include <KittyMemoryMgr.hpp>

namespace azlw::loader {
namespace {

class KittySolistVisibilityBackend final : public SolistVisibilityBackend {
public:
    explicit KittySolistVisibilityBackend(KittyMemoryMgr* memory) : memory_(memory) {}

    bool snapshot(SolistSnapshot* output) override {
        if (memory_ == nullptr || !memory_->isMemValid() || output == nullptr) {
            last_error_ = "AndKitty 远程内存或快照输出无效";
            return false;
        }
        LinkerScannerMgr& linker = memory_->linkerScanner;
        const kitty_soinfo_offsets_t soinfo_offsets = linker.soinfo_offsets();
        const kitty_linker_syms_t linker_offsets = linker.linker_offsets();
        if (!linker.isInitialized() || soinfo_offsets.next == kitty_soinfo_offsets_t::noff ||
            linker_offsets.sonext == 0) {
            last_error_ = "AndKitty linker 扫描器缺少 next 或 sonext";
            return false;
        }

        const std::vector<kitty_soinfo_t> soinfos = linker.allSoInfo();
        const std::uintptr_t tail = linker.sonext();
        if (soinfos.empty() || tail == 0) {
            last_error_ = "AndKitty linker solist 或尾节点为空";
            return false;
        }
        *output = SolistSnapshot{
            .nodes = {},
            .tail = tail,
            .tail_pointer_address = linker_offsets.sonext,
            .next_offset = soinfo_offsets.next,
        };
        output->nodes.reserve(soinfos.size());
        for (const kitty_soinfo_t& soinfo : soinfos) {
            output->nodes.push_back(SolistNode{
                .address = soinfo.ptr,
                .next = soinfo.next,
            });
        }
        last_error_.clear();
        return true;
    }

    bool read_pointer(std::uintptr_t address, std::uintptr_t* output) override {
        if (memory_ == nullptr || output == nullptr ||
            memory_->readMem(address, output, sizeof(*output)) != sizeof(*output)) {
            last_error_ = "AndKitty 远程指针读取不完整";
            return false;
        }
        last_error_.clear();
        return true;
    }

    bool write_pointer(std::uintptr_t address, std::uintptr_t value) override {
        if (memory_ == nullptr ||
            memory_->trace.pokeMem(address, &value, sizeof(value)) != sizeof(value)) {
            last_error_ = "AndKitty ptrace 指针写入不完整";
            return false;
        }
        last_error_.clear();
        return true;
    }

    [[nodiscard]] std::string last_error() const override {
        return last_error_.empty() ? "未知 AndKitty linker 操作错误" : last_error_;
    }

private:
    KittyMemoryMgr* memory_;
    std::string last_error_;
};

}  // namespace

SolistVisibilityState hide_kitty_solisted_tail(
    KittyMemoryMgr* memory,
    std::uintptr_t soinfo_address,
    SolistVisibilityEvidence* evidence,
    std::string* error) {
    KittySolistVisibilityBackend backend(memory);
    return hide_solisted_tail(&backend, soinfo_address, evidence, error);
}

SolistVisibilityState restore_kitty_solisted_tail(
    KittyMemoryMgr* memory,
    const SolistVisibilityEvidence& evidence,
    std::string* error) {
    KittySolistVisibilityBackend backend(memory);
    return restore_solisted_tail(&backend, evidence, error);
}

}  // namespace azlw::loader

// 实现基于动态加载器程序头的唯一模块查找和地址区间校验。

#include "module_view.h"

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <elf.h>
#include <fcntl.h>
#include <link.h>
#include <limits>
#include <sys/stat.h>
#include <string_view>
#include <unistd.h>
#include <utility>

namespace azlw::agent {
namespace {

/// 累积同名模块匹配，供遍历结束后执行唯一性判定。
struct SearchContext final {
    std::string_view module_name;
    std::vector<ModuleView> matches;
};

/// 返回路径末段，避免目录差异影响模块文件名匹配。
std::string_view basename(std::string_view path) {
    const std::size_t separator = path.find_last_of('/');
    return separator == std::string_view::npos ? path : path.substr(separator + 1);
}

/// 收集名称匹配模块的非空 PT_LOAD 段，并跳过地址溢出区间。
int collect_module(dl_phdr_info* info, std::size_t, void* opaque) {
    auto* context = static_cast<SearchContext*>(opaque);
    if (info == nullptr || info->dlpi_name == nullptr) {
        return 0;
    }
    const std::string_view path = info->dlpi_name;
    if (path.empty() || basename(path) != context->module_name) {
        return 0;
    }

    ModuleView module;
    module.load_bias = static_cast<std::uintptr_t>(info->dlpi_addr);
    module.path.assign(path);
    for (std::size_t index = 0; index < info->dlpi_phnum; ++index) {
        const ElfW(Phdr)& header = info->dlpi_phdr[index];
        if (header.p_type != PT_LOAD || header.p_memsz == 0) {
            continue;
        }
        const std::uintptr_t start = module.load_bias + static_cast<std::uintptr_t>(header.p_vaddr);
        const std::uintptr_t size = static_cast<std::uintptr_t>(header.p_memsz);
        if (start > std::numeric_limits<std::uintptr_t>::max() - size) {
            continue;
        }
        module.segments.push_back(ModuleSegment{
            .start = start,
            .end = start + size,
            .flags = header.p_flags,
        });
    }
    if (!module.segments.empty()) {
        context->matches.push_back(std::move(module));
    }
    return 0;
}

/// 在文件边界内完成指定长度的定位读取，并统一处理短读与中断重试。
bool read_exact_at(
    int descriptor,
    std::uint64_t offset,
    void* output,
    std::size_t size,
    std::uint64_t file_size,
    std::string_view stage,
    std::string* error) {
    if (offset > file_size || size > file_size - offset) {
        *error = std::string(stage) + " 超出 ELF 文件边界";
        return false;
    }
    auto* bytes = static_cast<std::uint8_t*>(output);
    std::size_t completed = 0;
    while (completed < size) {
        const std::uint64_t current = offset + completed;
        if (current > static_cast<std::uint64_t>(std::numeric_limits<off_t>::max())) {
            *error = std::string(stage) + " 偏移无法表示为 off_t";
            return false;
        }
        const ssize_t count = pread(
            descriptor,
            bytes + completed,
            size - completed,
            static_cast<off_t>(current));
        if (count < 0) {
            if (errno == EINTR) {
                continue;
            }
            *error = std::string(stage) + " 读取失败: " + std::strerror(errno);
            return false;
        }
        if (count == 0) {
            *error = std::string(stage) + " 提前到达文件末尾";
            return false;
        }
        completed += static_cast<std::size_t>(count);
    }
    return true;
}

/// 校验目标文件是否为当前运行态支持的 x86_64 ELF64 动态库。
bool validate_elf_header(const Elf64_Ehdr& header, std::string* error) {
    if (std::memcmp(header.e_ident, ELFMAG, SELFMAG) != 0 ||
        header.e_ident[EI_CLASS] != ELFCLASS64 || header.e_ident[EI_DATA] != ELFDATA2LSB ||
        header.e_ident[EI_VERSION] != EV_CURRENT || header.e_version != EV_CURRENT ||
        header.e_machine != EM_X86_64 || header.e_type != ET_DYN ||
        header.e_ehsize != sizeof(Elf64_Ehdr) || header.e_shentsize != sizeof(Elf64_Shdr)) {
        *error = "模块文件不是受支持的小端 x86_64 ELF64 动态库";
        return false;
    }
    if (header.e_shnum == 0 || header.e_shnum > 4'096) {
        *error = "ELF section 数量为空、使用扩展编码或超过 4096";
        return false;
    }
    return true;
}

/// 在受限字符串表内比较以空字符结尾的动态符号名称。
bool symbol_name_matches(
    const std::vector<char>& string_table,
    std::uint32_t name_offset,
    std::string_view expected) {
    if (name_offset >= string_table.size()) {
        return false;
    }
    const char* start = string_table.data() + name_offset;
    const std::size_t remaining = string_table.size() - name_offset;
    const auto* terminator = static_cast<const char*>(std::memchr(start, '\0', remaining));
    if (terminator == nullptr) {
        return false;
    }
    return std::string_view(start, static_cast<std::size_t>(terminator - start)) == expected;
}

/// 从已经打开的模块文件中唯一定位动态函数，并验证其运行时可执行地址。
bool resolve_from_descriptor(
    int descriptor,
    std::uint64_t file_size,
    const ModuleView& module,
    std::string_view symbol_name,
    std::uintptr_t* address,
    std::string* error) {
    Elf64_Ehdr header{};
    if (!read_exact_at(
            descriptor, 0, &header, sizeof(header), file_size, "ELF header", error) ||
        !validate_elf_header(header, error)) {
        return false;
    }

    const std::size_t section_count = header.e_shnum;
    if (section_count > std::numeric_limits<std::size_t>::max() / sizeof(Elf64_Shdr)) {
        *error = "ELF section header 总尺寸溢出";
        return false;
    }
    std::vector<Elf64_Shdr> sections(section_count);
    if (!read_exact_at(
            descriptor,
            header.e_shoff,
            sections.data(),
            sections.size() * sizeof(Elf64_Shdr),
            file_size,
            "ELF section headers",
            error)) {
        return false;
    }

    const Elf64_Shdr* dynamic_symbols = nullptr;
    for (const Elf64_Shdr& section : sections) {
        if (section.sh_type != SHT_DYNSYM || section.sh_size == 0) {
            continue;
        }
        if (dynamic_symbols != nullptr) {
            *error = "ELF 存在多个非空动态符号表";
            return false;
        }
        dynamic_symbols = &section;
    }
    if (dynamic_symbols == nullptr || dynamic_symbols->sh_entsize != sizeof(Elf64_Sym) ||
        dynamic_symbols->sh_size % sizeof(Elf64_Sym) != 0 ||
        dynamic_symbols->sh_link >= sections.size()) {
        *error = "ELF 动态符号表缺失或结构无效";
        return false;
    }
    if (dynamic_symbols->sh_offset > file_size ||
        dynamic_symbols->sh_size > file_size - dynamic_symbols->sh_offset) {
        *error = "ELF 动态符号表超出文件边界";
        return false;
    }
    const Elf64_Shdr& strings = sections[dynamic_symbols->sh_link];
    if (strings.sh_type != SHT_STRTAB || strings.sh_size == 0 ||
        strings.sh_size > 64ULL * 1024ULL * 1024ULL ||
        strings.sh_size > std::numeric_limits<std::size_t>::max()) {
        *error = "ELF 动态字符串表缺失或尺寸无效";
        return false;
    }
    std::vector<char> string_table(static_cast<std::size_t>(strings.sh_size));
    if (!read_exact_at(
            descriptor,
            strings.sh_offset,
            string_table.data(),
            string_table.size(),
            file_size,
            "ELF dynamic string table",
            error)) {
        return false;
    }

    const std::uint64_t symbol_count = dynamic_symbols->sh_size / sizeof(Elf64_Sym);
    if (symbol_count == 0 || symbol_count > 1'000'000) {
        *error = "ELF 动态符号数量为空或超过上限";
        return false;
    }
    bool found = false;
    std::uintptr_t resolved = 0;
    for (std::uint64_t index = 0; index < symbol_count; ++index) {
        Elf64_Sym symbol{};
        const std::uint64_t entry_offset =
            dynamic_symbols->sh_offset + index * sizeof(Elf64_Sym);
        if (!read_exact_at(
                descriptor,
                entry_offset,
                &symbol,
                sizeof(symbol),
                file_size,
                "ELF dynamic symbol",
                error)) {
            return false;
        }
        if (!symbol_name_matches(string_table, symbol.st_name, symbol_name)) {
            continue;
        }
        const unsigned char binding = ELF64_ST_BIND(symbol.st_info);
        const unsigned char type = ELF64_ST_TYPE(symbol.st_info);
        if ((binding != STB_GLOBAL && binding != STB_WEAK) || type != STT_FUNC ||
            symbol.st_shndx == SHN_UNDEF || symbol.st_value == 0) {
            *error = "目标动态符号不是已定义的 GLOBAL/WEAK FUNC";
            return false;
        }
        if (symbol.st_value > std::numeric_limits<std::uintptr_t>::max() - module.load_bias) {
            *error = "目标动态符号运行时地址溢出";
            return false;
        }
        const std::uintptr_t candidate =
            module.load_bias + static_cast<std::uintptr_t>(symbol.st_value);
        if (!module.contains_executable(candidate, 1)) {
            *error = "目标动态符号不属于当前模块可执行段";
            return false;
        }
        if (found) {
            *error = "ELF 动态符号表存在多个同名目标函数";
            return false;
        }
        found = true;
        resolved = candidate;
    }
    if (!found) {
        *error = "ELF 动态符号表缺少函数 " + std::string(symbol_name);
        return false;
    }
    *address = resolved;
    return true;
}

}  // namespace

// 使用半开区间判断，段尾地址不属于映射。
bool ModuleView::contains(std::uintptr_t address) const {
    return std::any_of(segments.begin(), segments.end(), [address](const ModuleSegment& segment) {
        return address >= segment.start && address < segment.end;
    });
}

// 整个范围必须位于同一可执行段，禁止跨段或整数回绕。
bool ModuleView::contains_executable(std::uintptr_t address, std::size_t size) const {
    if (size == 0 || address > std::numeric_limits<std::uintptr_t>::max() - size) {
        return false;
    }
    const std::uintptr_t end = address + size;
    return std::any_of(segments.begin(), segments.end(), [address, end](const ModuleSegment& segment) {
        return (segment.flags & PF_X) != 0 && address >= segment.start && end <= segment.end;
    });
}

// 同名零个或多个实例都不满足冻结 profile 的唯一身份要求。
bool find_loaded_module(std::string_view module_name, ModuleView* module, std::string* error) {
    SearchContext context{.module_name = module_name, .matches = {}};
    dl_iterate_phdr(collect_module, &context);
    if (context.matches.empty()) {
        *error = "目标进程未加载模块 " + std::string(module_name);
        return false;
    }
    if (context.matches.size() != 1) {
        *error = "目标进程存在多个同名模块 " + std::string(module_name);
        return false;
    }
    *module = std::move(context.matches.front());
    return true;
}

// 只读取当前已加载模块对应的普通文件，避免全局符号域带来的同名歧义。
bool resolve_exported_function(
    const ModuleView& module,
    std::string_view symbol_name,
    std::uintptr_t* address,
    std::string* error) {
    if (module.path.empty() || symbol_name.empty() || symbol_name.find('\0') != std::string_view::npos ||
        address == nullptr || error == nullptr) {
        if (error != nullptr) {
            *error = "动态符号解析参数无效";
        }
        return false;
    }
    const int descriptor = open(module.path.c_str(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) {
        *error = "打开已加载模块文件失败: " + std::string(std::strerror(errno));
        return false;
    }
    struct stat info {};
    bool resolved = false;
    if (fstat(descriptor, &info) != 0) {
        *error = "读取已加载模块属性失败: " + std::string(std::strerror(errno));
    } else if (!S_ISREG(info.st_mode) || info.st_size <= 0) {
        *error = "已加载模块路径不是非空普通文件";
    } else {
        resolved = resolve_from_descriptor(
            descriptor,
            static_cast<std::uint64_t>(info.st_size),
            module,
            symbol_name,
            address,
            error);
    }
    const int close_result = close(descriptor);
    if (close_result != 0) {
        const std::string close_error =
            "关闭已加载模块文件失败: " + std::string(std::strerror(errno));
        if (resolved) {
            *error = close_error;
        } else {
            *error += "; " + close_error;
        }
        return false;
    }
    return resolved;
}

}  // namespace azlw::agent

// 声明进程内已加载模块及其 PT_LOAD 内存区间的只读视图。

#pragma once

#include <cstddef>
#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace azlw::agent {

/// 单个已加载 ELF 段的半开地址区间和权限标志。
struct ModuleSegment final {
    std::uintptr_t start = 0;
    std::uintptr_t end = 0;
    std::uint32_t flags = 0;
};

/// 唯一已加载模块的基址、路径和可访问段集合。
struct ModuleView final {
    std::uintptr_t load_bias = 0;
    std::string path;
    std::vector<ModuleSegment> segments;

    /// 判断单个地址是否落在任一已加载段内。
    bool contains(std::uintptr_t address) const;
    /// 判断完整地址范围是否落在同一可执行段内。
    bool contains_executable(std::uintptr_t address, std::size_t size) const;
};

/// 通过动态加载器视图精确查找一个已加载模块，不接受同名多实例。
bool find_loaded_module(std::string_view module_name, ModuleView* module, std::string* error);

/// 从已校验模块的 ELF64 动态符号表解析唯一函数，并复核运行时可执行段归属。
bool resolve_exported_function(
    const ModuleView& module,
    std::string_view symbol_name,
    std::uintptr_t* address,
    std::string* error);

}  // namespace azlw::agent

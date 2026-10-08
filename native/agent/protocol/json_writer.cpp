// 维护结构化 JSON 写入状态、字符转义和数值格式。

#include "json_codec.h"

#include <array>
#include <charconv>
#include <cmath>
#include <limits>
#include <utility>

namespace azlw::agent {

// 根据容器类型写入数组分隔符或消费对象的待写值状态。
void JsonWriter::before_value() {
    if (scopes_.empty()) {
        root_written_ = true;
        return;
    }
    Scope& scope = scopes_.back();
    if (scope.kind == ScopeKind::Array) {
        if (!scope.first) {
            output_.push_back(',');
        }
        scope.first = false;
    } else {
        scope.expects_value = false;
    }
}

// 转义引号、反斜杠和控制字符，其余 UTF-8 字节保持原样。
void JsonWriter::append_escaped(std::string_view value) {
    constexpr char kHex[] = "0123456789abcdef";
    output_.push_back('"');
    for (const unsigned char current : value) {
        switch (current) {
            case '"': output_ += "\\\""; break;
            case '\\': output_ += "\\\\"; break;
            case '\b': output_ += "\\b"; break;
            case '\f': output_ += "\\f"; break;
            case '\n': output_ += "\\n"; break;
            case '\r': output_ += "\\r"; break;
            case '\t': output_ += "\\t"; break;
            default:
                if (current < 0x20) {
                    output_ += "\\u00";
                    output_.push_back(kHex[current >> 4]);
                    output_.push_back(kHex[current & 0x0f]);
                } else {
                    output_.push_back(static_cast<char>(current));
                }
                break;
        }
    }
    output_.push_back('"');
}

// 在当前位置开始对象并压入新的字段作用域。
void JsonWriter::begin_object() {
    before_value();
    output_.push_back('{');
    scopes_.push_back(Scope{.kind = ScopeKind::Object});
}

// 结束并弹出当前对象作用域。
void JsonWriter::end_object() {
    output_.push_back('}');
    scopes_.pop_back();
}

// 在当前位置开始数组并压入新的元素作用域。
void JsonWriter::begin_array() {
    before_value();
    output_.push_back('[');
    scopes_.push_back(Scope{.kind = ScopeKind::Array});
}

// 结束并弹出当前数组作用域。
void JsonWriter::end_array() {
    output_.push_back(']');
    scopes_.pop_back();
}

// 写入对象字段名，并标记下一次值写入不需要额外逗号。
void JsonWriter::key(std::string_view name) {
    Scope& scope = scopes_.back();
    if (!scope.first) {
        output_.push_back(',');
    }
    scope.first = false;
    append_escaped(name);
    output_.push_back(':');
    scope.expects_value = true;
}

// 写入经过统一转义的字符串值。
void JsonWriter::string(std::string_view value) {
    before_value();
    append_escaped(value);
}

// 写入 JSON 布尔字面量。
void JsonWriter::boolean(bool value) {
    before_value();
    output_ += value ? "true" : "false";
}

// 写入显式 JSON 空值。
void JsonWriter::null() {
    before_value();
    output_ += "null";
}

// 转移内部文本，避免响应编码结束时额外复制。
std::string JsonWriter::take() {
    return std::move(output_);
}

// 以规范十进制形式写入 64 位无符号整数。
void JsonWriter::number(std::uint64_t value) {
    before_value();
    output_ += std::to_string(value);
}

// 复用 64 位无符号整数路径保持数值格式一致。
void JsonWriter::number(std::uint32_t value) {
    number(static_cast<std::uint64_t>(value));
}

// 以规范十进制形式写入 64 位有符号整数。
void JsonWriter::number(std::int64_t value) {
    before_value();
    output_ += std::to_string(value);
}

// 使用 round-trip 精度编码有限小数，避免区域设置或尾随零改变语义哈希。
void JsonWriter::number(double value) {
    std::array<char, 64> buffer{};
    const auto encoded = std::to_chars(
        buffer.data(),
        buffer.data() + buffer.size(),
        value,
        std::chars_format::general,
        std::numeric_limits<double>::max_digits10);
    if (!std::isfinite(value) || encoded.ec != std::errc{}) {
        // 调用方先验证有限数；此处仍保证内部不变量失败时输出合法 JSON。
        before_value();
        output_ += "null";
        return;
    }
    before_value();
    output_.append(buffer.data(), encoded.ptr);
}

}  // namespace azlw::agent

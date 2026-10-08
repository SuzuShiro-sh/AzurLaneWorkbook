// 实现 JSON 请求解析与响应编码共享的私有协议原语。

#include "json_codec_internal.h"

#include <algorithm>
#include <array>
#include <utility>

namespace azlw::agent::json_codec_internal {

/// 将固定长度小写十六进制文本解码到调用方缓冲区。
bool parse_lower_hex(std::string_view value, std::uint8_t* output, std::size_t output_size) {
    if (value.size() != output_size * 2) {
        return false;
    }
    auto nibble = [](char current) -> int {
        if (current >= '0' && current <= '9') {
            return current - '0';
        }
        if (current >= 'a' && current <= 'f') {
            return current - 'a' + 10;
        }
        return -1;
    };
    for (std::size_t index = 0; index < output_size; ++index) {
        const int high = nibble(value[index * 2]);
        const int low = nibble(value[index * 2 + 1]);
        if (high < 0 || low < 0) {
            return false;
        }
        output[index] = static_cast<std::uint8_t>((high << 4) | low);
    }
    return true;
}

/// 构造不会改变会话状态的稳定协议错误。
AgentError protocol_error(std::string code, std::string message) {
    return AgentError{
        .code = std::move(code),
        .stage = "agent.protocol",
        .message = std::move(message),
        .retry = "never",
        .session_effect = "unchanged",
    };
}

/// 在写入成功响应前维护与 Rust 客户端一致的装备命令收据不变量。
std::optional<AgentError> validate_equipment_command_receipt(
    const EquipmentCommandReceipt& receipt) {
    std::array<std::uint8_t, 32> command_id{};
    if (receipt.schema_version != 1 ||
        !parse_lower_hex(receipt.command_id, command_id.data(), command_id.size())) {
        return protocol_error(
            "equipment_command_receipt_invalid",
            "装备命令收据版本或 command_id 无效");
    }
    if (!receipt.write_dispatched) {
        return protocol_error(
            "equipment_command_receipt_invalid",
            "确定未派发的装备命令必须返回错误响应");
    }

    const bool state_matches =
        (receipt.status == EquipmentCommandStatus::Success &&
         receipt.phase == EquipmentCommandPhase::Succeeded) ||
        (receipt.status == EquipmentCommandStatus::Failed &&
         receipt.phase == EquipmentCommandPhase::Failed) ||
        (receipt.status == EquipmentCommandStatus::Unknown &&
         (receipt.phase == EquipmentCommandPhase::Observing ||
          receipt.phase == EquipmentCommandPhase::Uncertain));
    if (!state_matches) {
        return protocol_error(
            "equipment_command_receipt_invalid",
            "装备命令收据的 status 与 phase 不一致");
    }

    if (receipt.error_code.has_value() != receipt.message.has_value()) {
        return protocol_error(
            "equipment_command_receipt_invalid",
            "装备命令收据的 error_code 与 message 必须同时出现或同时为空");
    }
    if (receipt.status == EquipmentCommandStatus::Success && receipt.error_code.has_value()) {
        return protocol_error(
            "equipment_command_receipt_invalid",
            "成功的装备命令收据不得携带错误诊断");
    }
    if (receipt.error_code.has_value()) {
        const std::string_view code = *receipt.error_code;
        if (code.empty() || code.size() > 128 ||
            !std::all_of(code.begin(), code.end(), [](char current) {
                return (current >= 'a' && current <= 'z') ||
                       (current >= '0' && current <= '9') || current == '.' ||
                       current == '_' || current == '-';
            })) {
            return protocol_error(
                "equipment_command_receipt_invalid",
                "装备命令收据的 error_code 不是稳定小写标识");
        }
        const std::string_view message = *receipt.message;
        if (message.empty() || message.size() > 4096 ||
            message.find_first_not_of(" \t\r\n") == std::string_view::npos) {
            return protocol_error(
                "equipment_command_receipt_invalid",
                "装备命令收据的 message 必须是有界非空文本");
        }
    }
    return std::nullopt;
}

}  // namespace azlw::agent::json_codec_internal

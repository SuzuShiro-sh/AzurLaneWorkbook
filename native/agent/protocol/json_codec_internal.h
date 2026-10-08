// 声明 JSON 解析与编码实现共享的私有协议原语。

#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>

#include "protocol_types.h"

namespace azlw::agent {
class JsonWriter;
}

namespace azlw::agent::json_codec_internal {

void begin_ok_response(JsonWriter* writer, std::string_view request_id);

bool parse_lower_hex(
    std::string_view value,
    std::uint8_t* output,
    std::size_t output_size);

AgentError protocol_error(std::string code, std::string message);

std::optional<AgentError> validate_equipment_command_receipt(
    const EquipmentCommandReceipt& receipt);

}  // namespace azlw::agent::json_codec_internal

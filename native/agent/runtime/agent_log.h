// 提供 agent 统一 Android 日志标签和带阶段上下文的日志入口。

#pragma once

#include <android/log.h>

#include <string>

namespace azlw::agent {

/// 使用不包含产品身份的通用标签，阶段键继续提供诊断上下文。
inline constexpr const char* kLogTag = "runtime";

/// 记录带稳定阶段键的错误消息。
inline void log_error(const char* stage, const std::string& message) {
    __android_log_print(ANDROID_LOG_ERROR, kLogTag, "%s: %s", stage, message.c_str());
}

/// 记录带稳定阶段键的状态消息。
inline void log_info(const char* stage, const std::string& message) {
    __android_log_print(ANDROID_LOG_INFO, kLogTag, "%s: %s", stage, message.c_str());
}

}  // namespace azlw::agent

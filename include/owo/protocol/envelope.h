#pragma once

#include <cstdint>
#include <string>
#include <string_view>

namespace owo::protocol {

inline constexpr std::uint32_t kProtocolVersion = 9;
inline constexpr std::uint32_t kMaximumPayloadBytes = 1024U * 1024U;

enum class ErrorCode {
    none,
    unsupported_protocol,
    invalid_payload,
    payload_too_large,
    transport_unavailable,
    timeout,
    cancelled,
};

struct Envelope {
    std::uint32_t protocol_version{kProtocolVersion};
    std::uint64_t request_id{};
    std::uint64_t context_generation{};
    std::string payload_json;
};

struct ValidationResult {
    ErrorCode error{ErrorCode::none};
    std::string message;

    [[nodiscard]] explicit operator bool() const noexcept {
        return error == ErrorCode::none;
    }
};

/// 验证进程间消息信封的版本和大小边界；不解析业务 JSON。
/// @thread_safety 可并发调用。
[[nodiscard]] ValidationResult validate(const Envelope& envelope);

/// 生成 4 字节小端长度前缀。内部原型帧格式，不是稳定公共协议。
/// @thread_safety 可并发调用。
[[nodiscard]] std::string frame(std::string_view payload);

}  // namespace owo::protocol

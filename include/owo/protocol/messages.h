#pragma once

#include "owo/protocol/envelope.h"

#include <cstdint>
#include <string>
#include <vector>

namespace owo::protocol {

enum class MessageType {
    candidate_request,
    candidate_response,
    candidate_update_request,
    candidate_update_response,
    candidate_committed,
    acknowledgement,
    shutdown_request,
    error_response,
};

struct Message {
    MessageType type{MessageType::error_response};
    std::uint64_t request_id{};
    std::uint64_t context_generation{};
    std::string text;
    std::vector<std::string> candidates;
    std::uint64_t page{};
    bool has_more{};
    bool model_pending{};
    std::vector<std::string> syllables;
    std::vector<std::uint64_t> candidate_consumed;
    bool expanded{};
    std::uint64_t page_size{};
    bool correction_enabled{true};
    std::string input;
    std::string context;
};

struct DecodeResult {
    Message message;
    ValidationResult validation;
};

/// 将内部消息编码为 UTF-8 JSON。该表示尚不是稳定公共协议。
[[nodiscard]] std::string encode_message(const Message& message);

/// 严格解析内部消息；拒绝未知字段、重复字段和非法转义。
[[nodiscard]] DecodeResult decode_message(std::string_view json);

}  // namespace owo::protocol

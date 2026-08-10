#pragma once

#include "owo/model/model_backend.h"
#include "owo/protocol/envelope.h"

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace owo::model {

inline constexpr std::uint32_t kModelProtocolVersion = 2;

enum class ModelMessageType : std::uint8_t {
    rank_request = 1,
    rank_response = 2,
    shutdown_request = 3,
    acknowledgement = 4,
    error_response = 5,
};

struct ModelMessage {
    ModelMessageType type{ModelMessageType::error_response};
    std::uint64_t request_id{};
    std::uint32_t timeout_ms{};
    ModelStatus status{ModelStatus::backend_error};
    std::string model_id;
    std::string input;
    std::string context;
    std::vector<std::string> candidates;
    std::string diagnostic;
};

struct ModelDecodeResult {
    ModelMessage message;
    protocol::ValidationResult validation;
};

[[nodiscard]] std::string encode_model_message(const ModelMessage& message);
[[nodiscard]] ModelDecodeResult decode_model_message(std::string_view payload);

}  // namespace owo::model

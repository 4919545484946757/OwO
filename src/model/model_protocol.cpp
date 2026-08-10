#include "owo/model/model_protocol.h"

#include <algorithm>
#include <array>
#include <limits>
#include <utility>

namespace owo::model {
namespace {

constexpr std::array<char, 4> kMagic{'O', 'W', 'M', 'H'};
constexpr std::size_t kMaximumCandidates = 256;

template <typename Integer>
void append_integer(std::string& output, const Integer value) {
    for (std::size_t shift = 0; shift < sizeof(Integer) * 8U; shift += 8U)
        output.push_back(static_cast<char>(value >> shift));
}

bool append_string(std::string& output, const std::string_view value) {
    if (value.size() > std::numeric_limits<std::uint32_t>::max()) return false;
    append_integer(output, static_cast<std::uint32_t>(value.size()));
    output.append(value);
    return true;
}

template <typename Integer>
bool read_integer(const std::string_view input, std::size_t& offset, Integer& value) {
    if (offset + sizeof(Integer) > input.size()) return false;
    value = 0;
    for (std::size_t index = 0; index < sizeof(Integer); ++index)
        value |= static_cast<Integer>(static_cast<unsigned char>(input[offset + index])) <<
                 (index * 8U);
    offset += sizeof(Integer);
    return true;
}

bool read_string(const std::string_view input, std::size_t& offset, std::string& value) {
    std::uint32_t size{};
    if (!read_integer(input, offset, size) || offset + size > input.size()) return false;
    value.assign(input.substr(offset, size));
    offset += size;
    return true;
}

bool valid_type(const ModelMessageType type) {
    return type >= ModelMessageType::rank_request && type <= ModelMessageType::error_response;
}

bool valid_status(const ModelStatus status) {
    return status >= ModelStatus::success && status <= ModelStatus::backend_error;
}

}  // namespace

std::string encode_model_message(const ModelMessage& message) {
    if (!valid_type(message.type) || !valid_status(message.status) ||
        message.candidates.size() > kMaximumCandidates) return {};
    std::string output(kMagic.begin(), kMagic.end());
    append_integer(output, kModelProtocolVersion);
    append_integer(output, static_cast<std::uint8_t>(message.type));
    append_integer(output, static_cast<std::uint8_t>(message.status));
    append_integer(output, static_cast<std::uint16_t>(0));
    append_integer(output, message.request_id);
    append_integer(output, message.timeout_ms);
    append_integer(output, static_cast<std::uint32_t>(message.candidates.size()));
    if (!append_string(output, message.model_id) || !append_string(output, message.input) ||
        !append_string(output, message.context)) return {};
    for (const auto& candidate : message.candidates)
        if (!append_string(output, candidate)) return {};
    if (!append_string(output, message.diagnostic) ||
        output.size() > protocol::kMaximumPayloadBytes) return {};
    return output;
}

ModelDecodeResult decode_model_message(const std::string_view payload) {
    ModelDecodeResult result;
    if (payload.size() > protocol::kMaximumPayloadBytes) {
        result.validation = {protocol::ErrorCode::payload_too_large, "model message exceeds limit"};
        return result;
    }
    if (payload.size() < kMagic.size() ||
        !std::equal(kMagic.begin(), kMagic.end(), payload.begin())) goto invalid;
    {
        std::size_t offset = kMagic.size();
        std::uint32_t version{};
        std::uint8_t type{};
        std::uint8_t status{};
        std::uint16_t reserved{};
        std::uint32_t count{};
        if (!read_integer(payload, offset, version) || version != kModelProtocolVersion) {
            result.validation = {protocol::ErrorCode::unsupported_protocol,
                                 "unsupported model protocol version"};
            return result;
        }
        if (!read_integer(payload, offset, type) ||
            !read_integer(payload, offset, status) ||
            !read_integer(payload, offset, reserved) || reserved != 0 ||
            !read_integer(payload, offset, result.message.request_id) ||
            !read_integer(payload, offset, result.message.timeout_ms) ||
            !read_integer(payload, offset, count) || count > kMaximumCandidates)
            goto invalid;
        result.message.type = static_cast<ModelMessageType>(type);
        result.message.status = static_cast<ModelStatus>(status);
        if (!valid_type(result.message.type) || !valid_status(result.message.status) ||
            !read_string(payload, offset, result.message.model_id) ||
            !read_string(payload, offset, result.message.input) ||
            !read_string(payload, offset, result.message.context)) goto invalid;
        result.message.candidates.reserve(count);
        for (std::uint32_t index = 0; index < count; ++index) {
            std::string candidate;
            if (!read_string(payload, offset, candidate)) goto invalid;
            result.message.candidates.push_back(std::move(candidate));
        }
        if (!read_string(payload, offset, result.message.diagnostic) || offset != payload.size())
            goto invalid;
    }
    result.validation = {};
    return result;

invalid:
    result.validation = {protocol::ErrorCode::invalid_payload, "invalid model message schema"};
    return result;
}

}  // namespace owo::model

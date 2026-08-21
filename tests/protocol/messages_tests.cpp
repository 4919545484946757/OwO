#include "owo/protocol/messages.h"

#include <iostream>

int main() {
    using namespace owo::protocol;
    int failures = 0;
    Message original{MessageType::candidate_response, 9, 4, "你好",
                     {"你好", "你号", "a\\\"b\n中文"}, 2, true};
    original.syllables = {"ni", "hao"};
    original.candidate_consumed = {5, 5, 5};
    original.expanded = true;
    original.page_size = 5;
    original.correction_enabled = false;
    const auto encoded = encode_message(original);
    const auto decoded = decode_message(encoded);
    if (!decoded.validation || decoded.message.type != original.type ||
        decoded.message.request_id != original.request_id ||
        decoded.message.context_generation != original.context_generation ||
        decoded.message.text != original.text ||
        decoded.message.candidates != original.candidates ||
        decoded.message.page != original.page ||
        decoded.message.has_more != original.has_more ||
        decoded.message.model_pending != original.model_pending ||
        decoded.message.syllables != original.syllables ||
        decoded.message.candidate_consumed != original.candidate_consumed ||
        decoded.message.expanded != original.expanded ||
        decoded.message.page_size != original.page_size ||
        decoded.message.correction_enabled != original.correction_enabled) {
        std::cerr << "message round trip failed\n";
        ++failures;
    }
    Message feedback{MessageType::candidate_committed, 10, 5, "你好"};
    feedback.input = "Ni'Hao";
    feedback.context = "我说";
    const auto decoded_feedback = decode_message(encode_message(feedback));
    if (!decoded_feedback.validation || decoded_feedback.message.input != feedback.input ||
        decoded_feedback.message.context != feedback.context) {
        std::cerr << "commit feedback input round trip failed\n";
        ++failures;
    }
    feedback.input = "ni hao";
    if (!encode_message(feedback).empty()) {
        std::cerr << "invalid commit feedback input was encoded\n";
        ++failures;
    }
    Message contextual_request{MessageType::candidate_request, 11, 6, "wang"};
    contextual_request.input = "kuang";
    contextual_request.context = "狂";
    const auto decoded_contextual = decode_message(encode_message(contextual_request));
    if (!decoded_contextual.validation ||
        decoded_contextual.message.input != contextual_request.input ||
        decoded_contextual.message.context != contextual_request.context) {
        std::cerr << "candidate context input round trip failed\n";
        ++failures;
    }
    contextual_request.input = "kuang wang";
    if (!encode_message(contextual_request).empty()) {
        std::cerr << "invalid candidate context input was encoded\n";
        ++failures;
    }
    if (decode_message("{}").validation.error != ErrorCode::invalid_payload) {
        std::cerr << "invalid schema was accepted\n";
        ++failures;
    }
    auto wrong_version = encoded;
    const auto version_text = "\"protocol_version\":" + std::to_string(kProtocolVersion);
    const auto version_offset = wrong_version.find(version_text);
    if (version_offset == std::string::npos) {
        std::cerr << "encoded protocol version missing\n";
        ++failures;
    } else {
        wrong_version.replace(version_offset, version_text.size(),
                              "\"protocol_version\":999");
    }
    if (decode_message(wrong_version).validation.error != ErrorCode::unsupported_protocol) {
        std::cerr << "wrong version was accepted\n";
        ++failures;
    }
    auto forged_plugin_call = encoded;
    const auto response_type = std::string("\"type\":\"candidate_response\"");
    const auto type_offset = forged_plugin_call.find(response_type);
    if (type_offset == std::string::npos) {
        std::cerr << "encoded message type missing\n";
        ++failures;
    } else {
        forged_plugin_call.replace(type_offset, response_type.size(),
                                   "\"type\":\"plugin_invoke\"");
    }
    if (decode_message(forged_plugin_call).validation.error != ErrorCode::invalid_payload) {
        std::cerr << "forged plugin invocation type was accepted\n";
        ++failures;
    }
    auto invalid_syllable = original;
    invalid_syllable.syllables = {"ni", "Hao"};
    if (!encode_message(invalid_syllable).empty()) {
        std::cerr << "invalid syllable was encoded\n";
        ++failures;
    }
    auto invalid_consumption = original;
    invalid_consumption.candidate_consumed.pop_back();
    if (!encode_message(invalid_consumption).empty()) {
        std::cerr << "misaligned candidate consumption was encoded\n";
        ++failures;
    }
    auto invalid_page_size = original;
    invalid_page_size.page_size = 10;
    if (!encode_message(invalid_page_size).empty()) {
        std::cerr << "invalid candidate page size was encoded\n";
        ++failures;
    }
    return failures == 0 ? 0 : 1;
}

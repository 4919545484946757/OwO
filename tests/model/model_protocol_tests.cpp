#include "owo/model/model_protocol.h"

int main() {
    owo::model::ModelMessage request;
    request.type = owo::model::ModelMessageType::rank_request;
    request.request_id = 42;
    request.timeout_ms = 25;
    request.status = owo::model::ModelStatus::success;
    request.model_id = "owo.mock.rank.v1";
    request.input = "nihao";
    request.context = "我说";
    request.candidates = {"泥号", "你好"};
    const auto encoded = owo::model::encode_model_message(request);
    const auto decoded = owo::model::decode_model_message(encoded);
    if (!decoded.validation || decoded.message.type != request.type ||
        decoded.message.request_id != 42 || decoded.message.timeout_ms != 25 ||
        decoded.message.model_id != request.model_id || decoded.message.input != request.input ||
        decoded.message.context != request.context ||
        decoded.message.candidates != request.candidates) return 1;

    auto trailing = encoded + "x";
    if (owo::model::decode_model_message(trailing).validation) return 1;
    auto wrong_version = encoded;
    wrong_version[4] = 99;
    const auto rejected = owo::model::decode_model_message(wrong_version);
    if (rejected.validation.error != owo::protocol::ErrorCode::unsupported_protocol) return 1;
    return 0;
}

#include "owo/model/model_inference.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <numeric>
#include <thread>

namespace owo::model {

InferenceResult SyntheticInferenceSession::run(const InferenceBatch& batch,
                                                const std::stop_token stop,
                                                const std::chrono::milliseconds timeout) {
    if (batch.batch_size == 0 || batch.sequence_length == 0 ||
        batch.batch_size > 256 || batch.sequence_length > 512 ||
        batch.input_ids.size() != batch.batch_size * batch.sequence_length ||
        batch.attention_mask.size() != batch.input_ids.size() ||
        batch.token_type_ids.size() != batch.input_ids.size())
        return {ModelStatus::backend_error, {}, "invalid inference batch shape"};
    if (timeout.count() <= 0) return {ModelStatus::timeout, {}, "deadline exceeded"};
    const auto started = std::chrono::steady_clock::now();
    const auto deadline = started + timeout;
    while (std::chrono::steady_clock::now() - started < options_.latency) {
        if (stop.stop_requested()) return {ModelStatus::cancelled, {}, "cancelled"};
        if (std::chrono::steady_clock::now() >= deadline)
            return {ModelStatus::timeout, {}, "deadline exceeded"};
        std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    if (stop.stop_requested()) return {ModelStatus::cancelled, {}, "cancelled"};
    if (options_.fail) return {ModelStatus::backend_error, {}, "synthetic session failure"};

    std::vector<float> scores(batch.batch_size);
    for (std::size_t row = 0; row < batch.batch_size; ++row) {
        std::int64_t score = 0;
        for (std::size_t column = 0; column < batch.sequence_length; ++column) {
            const auto index = row * batch.sequence_length + column;
            if (batch.attention_mask[index] == 1)
                score += batch.input_ids[index] * static_cast<std::int64_t>(column + 1);
        }
        scores[row] = static_cast<float>(score);
    }
    return {ModelStatus::success, std::move(scores), {}};
}

AssetCandidateRanker::AssetCandidateRanker(ModelManifest manifest,
                                           std::vector<std::string> vocabulary,
                                           std::shared_ptr<IInferenceSession> session)
    : manifest_(std::move(manifest)), tokenizer_(std::move(vocabulary)),
      session_(std::move(session)) {}

std::string_view AssetCandidateRanker::id() const noexcept { return manifest_.model_id; }

ModelResult AssetCandidateRanker::rank(const ModelRequest& request, const std::stop_token stop) {
    if (!session_) return {request.request_id, ModelStatus::backend_error, {}, "missing session"};
    if (!tokenizer_.validation().ok)
        return {request.request_id, ModelStatus::backend_error, {}, "invalid tokenizer"};
    if (request.candidates.empty() || request.candidates.size() > manifest_.maximum_candidates)
        return {request.request_id, ModelStatus::backend_error, {}, "candidate count exceeds limit"};
    if (std::any_of(request.candidates.begin(), request.candidates.end(),
                    [](const auto& candidate) { return candidate.empty(); }))
        return {request.request_id, ModelStatus::backend_error, {}, "candidate is empty"};
    if (request.timeout.count() <= 0)
        return {request.request_id, ModelStatus::timeout, {}, "deadline exceeded"};

    InferenceBatch batch;
    batch.batch_size = request.candidates.size();
    batch.sequence_length = manifest_.maximum_sequence_length;
    batch.input_ids.assign(batch.batch_size * batch.sequence_length, tokenizer_.pad_token_id());
    batch.attention_mask.assign(batch.input_ids.size(), 0);
    batch.token_type_ids.assign(batch.input_ids.size(), 0);
    for (std::size_t row = 0; row < request.candidates.size(); ++row) {
        if (stop.stop_requested())
            return {request.request_id, ModelStatus::cancelled, {}, "cancelled"};
        std::string contextual_input = request.context;
        if (!contextual_input.empty() && !request.input.empty()) contextual_input.push_back(' ');
        contextual_input += request.input;
        const auto encoded = tokenizer_.encode_pair(contextual_input, request.candidates[row],
                                                    batch.sequence_length);
        if (!encoded.ok)
            return {request.request_id, ModelStatus::backend_error, {}, encoded.diagnostic};
        const auto offset = row * batch.sequence_length;
        std::copy(encoded.value.input_ids.begin(), encoded.value.input_ids.end(),
                  batch.input_ids.begin() + static_cast<std::ptrdiff_t>(offset));
        std::copy(encoded.value.attention_mask.begin(), encoded.value.attention_mask.end(),
                  batch.attention_mask.begin() + static_cast<std::ptrdiff_t>(offset));
        std::copy(encoded.value.token_type_ids.begin(), encoded.value.token_type_ids.end(),
                  batch.token_type_ids.begin() + static_cast<std::ptrdiff_t>(offset));
    }

    auto inference = session_->run(batch, stop, request.timeout);
    if (inference.status != ModelStatus::success)
        return {request.request_id, inference.status, {}, std::move(inference.diagnostic)};
    if (inference.scores.size() != request.candidates.size())
        return {request.request_id, ModelStatus::backend_error, {}, "session score count mismatch"};
    if (!std::all_of(inference.scores.begin(), inference.scores.end(),
                     [](const float score) { return std::isfinite(score); }))
        return {request.request_id, ModelStatus::backend_error, {}, "session returned non-finite score"};

    std::vector<std::size_t> order(request.candidates.size());
    std::iota(order.begin(), order.end(), 0);
    std::stable_sort(order.begin(), order.end(), [&](const auto left, const auto right) {
        return inference.scores[left] > inference.scores[right];
    });
    std::vector<std::string> ranked;
    ranked.reserve(order.size());
    for (const auto index : order) ranked.push_back(request.candidates[index]);
    return {request.request_id, ModelStatus::success, std::move(ranked), {}};
}

}  // namespace owo::model

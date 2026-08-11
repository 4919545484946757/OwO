#pragma once

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <numeric>
#include <string>
#include <string_view>
#include <vector>

namespace owo::ipc {

[[nodiscard]] inline std::size_t utf8_character_count(const std::string_view text) {
    return static_cast<std::size_t>(std::count_if(
        text.begin(), text.end(), [](const unsigned char byte) {
            return (byte & 0xc0U) != 0x80U;
        }));
}

// Model ranking is authoritative inside one structural tier. Tier boundaries remain stable:
// whole-input candidates, then multi-character prefixes, then single-character fallbacks.
inline void apply_contextual_candidate_order(
    const std::vector<std::string>& model_order,
    const std::size_t full_input_bytes,
    const std::vector<std::string>& original_candidates,
    const std::vector<std::uint64_t>& original_consumed,
    std::vector<std::string>& ordered_candidates,
    std::vector<std::uint64_t>& ordered_consumed) {
    if (original_candidates.size() != original_consumed.size()) return;

    const auto model_rank = [&model_order](const std::string& candidate) {
        const auto found = std::find(model_order.begin(), model_order.end(), candidate);
        return found == model_order.end()
                   ? model_order.size()
                   : static_cast<std::size_t>(found - model_order.begin());
    };
    const auto tier = [full_input_bytes](const std::string& candidate,
                                         const std::uint64_t consumed) {
        if (consumed == full_input_bytes) return 0;
        if (utf8_character_count(candidate) >= 2) return 1;
        return 2;
    };

    std::vector<std::size_t> indices(original_candidates.size());
    std::iota(indices.begin(), indices.end(), 0);
    std::stable_sort(indices.begin(), indices.end(), [&](const std::size_t left,
                                                         const std::size_t right) {
        const auto left_tier = tier(original_candidates[left], original_consumed[left]);
        const auto right_tier = tier(original_candidates[right], original_consumed[right]);
        if (left_tier != right_tier) return left_tier < right_tier;
        if (left_tier == 1 && original_consumed[left] != original_consumed[right])
            return original_consumed[left] > original_consumed[right];
        const auto left_rank = model_rank(original_candidates[left]);
        const auto right_rank = model_rank(original_candidates[right]);
        if (left_rank != right_rank) return left_rank < right_rank;
        return left < right;
    });

    ordered_candidates.clear();
    ordered_consumed.clear();
    ordered_candidates.reserve(indices.size());
    ordered_consumed.reserve(indices.size());
    for (const auto index : indices) {
        ordered_candidates.push_back(original_candidates[index]);
        ordered_consumed.push_back(original_consumed[index]);
    }
}

}  // namespace owo::ipc

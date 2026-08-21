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

inline void promote_preferred_candidates(
    const std::vector<std::string>& preferences,
    std::vector<std::string>& candidates,
    std::vector<std::uint64_t>& consumed) {
    if (candidates.size() != consumed.size()) return;
    for (auto preference = preferences.rbegin(); preference != preferences.rend();
         ++preference) {
        const auto found = std::find(candidates.begin(), candidates.end(), *preference);
        if (found == candidates.end()) continue;
        const auto index = static_cast<std::size_t>(found - candidates.begin());
        const auto tier = consumed[index];
        const auto tier_begin = std::find(consumed.begin(), consumed.begin() + index, tier);
        if (tier_begin == consumed.begin() + index) continue;
        const auto destination = static_cast<std::size_t>(tier_begin - consumed.begin());
        auto text = std::move(candidates[index]);
        candidates.erase(candidates.begin() + static_cast<std::ptrdiff_t>(index));
        consumed.erase(consumed.begin() + static_cast<std::ptrdiff_t>(index));
        candidates.insert(candidates.begin() + static_cast<std::ptrdiff_t>(destination),
                          std::move(text));
        consumed.insert(consumed.begin() + static_cast<std::ptrdiff_t>(destination), tier);
    }
}

inline void restore_preferred_candidate_positions(
    const std::vector<std::string>& preferences,
    const std::vector<std::string>& base_candidates,
    const std::vector<std::uint64_t>& base_consumed,
    std::vector<std::string>& candidates,
    std::vector<std::uint64_t>& consumed) {
    if (candidates.size() != consumed.size() ||
        candidates.size() != base_candidates.size() ||
        base_candidates.size() != base_consumed.size()) return;
    for (const auto& preference : preferences) {
        const auto base = std::find(base_candidates.begin(), base_candidates.end(),
                                    preference);
        const auto current = std::find(candidates.begin(), candidates.end(), preference);
        if (base == base_candidates.end() || current == candidates.end()) continue;
        const auto base_index = static_cast<std::size_t>(base - base_candidates.begin());
        const auto source = static_cast<std::size_t>(current - candidates.begin());
        const auto consumed_bytes = consumed[source];
        if (base_consumed[base_index] != consumed_bytes) continue;
        const auto tier_rank = static_cast<std::size_t>(std::count(
            base_consumed.begin(),
            base_consumed.begin() + static_cast<std::ptrdiff_t>(base_index),
            consumed_bytes));
        std::size_t destination = candidates.size();
        std::size_t seen = 0;
        for (std::size_t index = 0; index < consumed.size(); ++index) {
            if (consumed[index] != consumed_bytes) continue;
            if (seen++ == tier_rank) {
                destination = index;
                break;
            }
        }
        if (destination == candidates.size()) continue;
        if (source == destination) continue;
        auto text = std::move(candidates[source]);
        candidates.erase(candidates.begin() + static_cast<std::ptrdiff_t>(source));
        consumed.erase(consumed.begin() + static_cast<std::ptrdiff_t>(source));
        candidates.insert(candidates.begin() + static_cast<std::ptrdiff_t>(destination),
                          std::move(text));
        consumed.insert(consumed.begin() + static_cast<std::ptrdiff_t>(destination),
                        consumed_bytes);
    }
}

}  // namespace owo::ipc

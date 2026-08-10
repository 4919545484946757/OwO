#pragma once

#include "owo/engine/lexicon.h"

#include <algorithm>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

namespace owo::engine::detail {

inline std::optional<std::vector<std::string>> mixed_abbreviation_segments(
    const std::span<const std::string_view> syllables, const std::string_view input) {
    if (syllables.size() < 2 || input.size() < 3 ||
        input.find('\'') != std::string_view::npos ||
        !input.starts_with(syllables.front()) || input.size() <= syllables.front().size())
        return std::nullopt;

    std::vector<std::string> segments;
    segments.emplace_back(syllables.front());
    const auto visit = [&](const auto& self, const std::size_t syllable_index,
                           const std::size_t input_offset,
                           const bool abbreviated) -> bool {
        if (syllable_index == syllables.size())
            return input_offset == input.size() && abbreviated;
        const auto syllables_left = syllables.size() - syllable_index;
        const auto input_left = input.size() - input_offset;
        if (input_left < syllables_left) return false;
        const auto& syllable = syllables[syllable_index];
        const auto maximum = std::min(syllable.size(), input_left - (syllables_left - 1));
        for (std::size_t consumed = maximum; consumed > 0; --consumed) {
            const auto source = input.substr(input_offset, consumed);
            if (!syllable.starts_with(source)) continue;
            segments.emplace_back(source);
            if (self(self, syllable_index + 1, input_offset + consumed,
                     abbreviated || consumed < syllable.size()))
                return true;
            segments.pop_back();
        }
        return false;
    };
    if (!visit(visit, 1, syllables.front().size(), false)) return std::nullopt;
    return segments;
}

inline std::optional<std::vector<std::string>> mixed_abbreviation_segments(
    const std::span<const std::string> syllables, const std::string_view input) {
    std::vector<std::string_view> views;
    views.reserve(syllables.size());
    for (const auto& syllable : syllables) views.push_back(syllable);
    return mixed_abbreviation_segments(std::span<const std::string_view>(views), input);
}

inline void retain_best_mixed_matches(std::vector<AbbreviatedLexiconMatch>& matches,
                                      const std::size_t limit) {
    std::sort(matches.begin(), matches.end(), [](const auto& left, const auto& right) {
        if (left.entry.frequency != right.entry.frequency)
            return left.entry.frequency > right.entry.frequency;
        if (left.entry.syllables.size() != right.entry.syllables.size())
            return left.entry.syllables.size() > right.entry.syllables.size();
        return left.entry.text < right.entry.text;
    });
    if (matches.size() > limit) matches.resize(limit);
}

}  // namespace owo::engine::detail

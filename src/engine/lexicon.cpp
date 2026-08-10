#include "owo/engine/lexicon.h"

#include "lexicon_match.h"

#include <algorithm>
#include <utility>

namespace owo::engine {

MemoryLexicon::MemoryLexicon(std::vector<LexiconEntry> entries) : entries_(std::move(entries)) {
    for (const auto& entry : entries_)
        maximum_reading_length_ = std::max(maximum_reading_length_, entry.syllables.size());
}

std::vector<LexiconEntry> MemoryLexicon::lookup(
    const std::span<const std::string_view> syllables) const {
    std::vector<LexiconEntry> matches;
    for (const auto& entry : entries_) {
        if (entry.syllables.size() != syllables.size()) continue;
        if (std::equal(entry.syllables.begin(), entry.syllables.end(), syllables.begin(),
                       [](const std::string& left, const std::string_view right) {
                           return left == right;
                       })) {
            matches.push_back(entry);
        }
    }
    return matches;
}

std::vector<LexiconEntry> MemoryLexicon::lookup_initial(const char initial,
                                                        const std::size_t limit) const {
    std::vector<LexiconEntry> matches;
    for (const auto& entry : entries_) {
        if (entry.syllables.size() == 1 && !entry.syllables.front().empty() &&
            entry.syllables.front().front() == initial)
            matches.push_back(entry);
    }
    std::sort(matches.begin(), matches.end(), [](const auto& left, const auto& right) {
        if (left.frequency != right.frequency) return left.frequency > right.frequency;
        if (left.syllables.front().size() != right.syllables.front().size())
            return left.syllables.front().size() < right.syllables.front().size();
        return left.text < right.text;
    });
    if (matches.size() > limit) matches.resize(limit);
    return matches;
}

std::vector<AbbreviatedLexiconMatch> MemoryLexicon::lookup_mixed_abbreviation(
    const std::string_view input, const std::size_t limit) const {
    if (limit == 0) return {};
    std::vector<AbbreviatedLexiconMatch> matches;
    for (const auto& entry : entries_) {
        const auto segments = detail::mixed_abbreviation_segments(entry.syllables, input);
        if (!segments) continue;
        matches.push_back({entry, *segments});
    }
    detail::retain_best_mixed_matches(matches, limit);
    return matches;
}

}  // namespace owo::engine

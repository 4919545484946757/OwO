#include "owo/engine/candidate_generator.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <string_view>
#include <unordered_map>

namespace owo::engine {
namespace {

struct SearchState {
    std::string text;
    std::string previous;
    std::vector<std::string> syllables;
    std::int64_t score{};
};

std::int64_t unigram_score(const std::uint32_t frequency) {
    return static_cast<std::int64_t>(std::log1p(static_cast<double>(frequency)) * 1000.0);
}

// Raw corpus frequencies are attached to entries of different lengths. Without
// a token transition cost, summing several high-frequency characters always
// overwhelms a valid multi-syllable word and rewards pathological over-segmentation.
constexpr std::int64_t kAdditionalSegmentPenalty = 22'000;
constexpr std::int64_t kIncompleteBasePenalty = 1'500;
constexpr std::int64_t kIncompleteCharacterPenalty = 750;
constexpr std::int64_t kCorrectionPenalty = 6'000;
constexpr std::int64_t kAbbreviationBasePenalty = 8'000;
constexpr std::int64_t kMixedAbbreviationPhraseBonus = 8'000;
constexpr std::int64_t kPreferredInitialBonus = 6'000;

std::string_view preferred_initial_syllable(const char initial) {
    switch (initial) {
        case 'n': return "ni";
        case 'm': return "ma";
        default: return {};
    }
}

std::int64_t source_initial_bonus(const ParseResult& parsed, const ParsePath& path) {
    std::int64_t bonus = 0;
    for (const auto& syllable : path.syllables) {
        if (syllable.end != syllable.begin + 1 ||
            syllable.end > parsed.normalized_input.size()) continue;
        if (preferred_initial_syllable(parsed.normalized_input[syllable.begin]) ==
            syllable.text)
            bonus += kPreferredInitialBonus;
    }
    return bonus;
}

std::int64_t input_match_penalty(const ParsePath& path) {
    if (path.match_kind == InputMatchKind::incomplete_completion) {
        return kIncompleteBasePenalty +
               static_cast<std::int64_t>(path.completion_characters) *
                   kIncompleteCharacterPenalty;
    }
    if (path.match_kind == InputMatchKind::corrected)
        return static_cast<std::int64_t>(path.edit_count) * kCorrectionPenalty;
    if (path.match_kind == InputMatchKind::abbreviated_completion) {
        return kAbbreviationBasePenalty +
               static_cast<std::int64_t>(path.completion_characters) *
                   kIncompleteCharacterPenalty;
    }
    return 0;
}

void prune(std::vector<SearchState>& states, const std::size_t width) {
    std::sort(states.begin(), states.end(), [](const SearchState& left, const SearchState& right) {
        if (left.score != right.score) return left.score > right.score;
        return left.text < right.text;
    });
    if (states.size() > width) states.resize(width);
}

std::vector<std::string> source_segments(const ParseResult& parsed,
                                         const ParsePath& path) {
    std::vector<std::string> segments;
    segments.reserve(path.syllables.size());
    for (const auto& syllable : path.syllables) {
        if (syllable.begin >= syllable.end ||
            syllable.end > parsed.normalized_input.size()) return {};
        segments.push_back(parsed.normalized_input.substr(
            syllable.begin, syllable.end - syllable.begin));
    }
    return segments;
}

bool candidate_better(const Candidate& candidate, const Candidate& existing) {
    if (candidate.consumed_input_bytes != existing.consumed_input_bytes)
        return candidate.consumed_input_bytes > existing.consumed_input_bytes;
    if (candidate.score != existing.score) return candidate.score > existing.score;
    return candidate.match_kind < existing.match_kind;
}

bool score_less(const Candidate& left, const Candidate& right) {
    if (left.score != right.score) return left.score > right.score;
    if (left.match_kind != right.match_kind) return left.match_kind < right.match_kind;
    if (left.syllables.size() != right.syllables.size())
        return left.syllables.size() < right.syllables.size();
    return left.text < right.text;
}

std::size_t utf8_character_count(const std::string_view text) {
    return static_cast<std::size_t>(std::count_if(
        text.begin(), text.end(), [](const unsigned char byte) {
            return (byte & 0xc0U) != 0x80U;
        }));
}

void prioritize_two_character_words(std::vector<Candidate>& candidates) {
    std::vector<std::size_t> positions;
    std::vector<Candidate> two_character;
    std::vector<Candidate> single_character;
    const bool preserve_leading_whole_input = !candidates.empty() &&
        utf8_character_count(candidates.front().text) == 2 &&
        std::all_of(candidates.begin() + 1, candidates.end(),
                    [&candidates](const Candidate& candidate) {
                        return candidate.consumed_input_bytes <=
                               candidates.front().consumed_input_bytes;
                    });
    const std::size_t begin = preserve_leading_whole_input ? 1 : 0;
    for (std::size_t index = begin; index < candidates.size(); ++index) {
        const auto characters = utf8_character_count(candidates[index].text);
        if (characters != 1 && characters != 2) continue;
        positions.push_back(index);
    }
    const auto two_count = static_cast<std::size_t>(std::count_if(
        positions.begin(), positions.end(), [&candidates](const std::size_t index) {
            return utf8_character_count(candidates[index].text) == 2;
        }));
    if (two_count == 0 || two_count == positions.size()) return;
    two_character.reserve(two_count);
    single_character.reserve(positions.size() - two_count);
    for (const auto index : positions) {
        if (utf8_character_count(candidates[index].text) == 2)
            two_character.push_back(std::move(candidates[index]));
        else
            single_character.push_back(std::move(candidates[index]));
    }
    std::sort(two_character.begin(), two_character.end(), score_less);
    std::sort(single_character.begin(), single_character.end(), score_less);

    constexpr std::int64_t kPreferredTwoCharacterMinimumScore = 5'000;
    constexpr std::size_t kMaximumPreferredTwoCharacterWords = 10;
    std::size_t preferred = 0;
    while (preferred < two_character.size() &&
           preferred < kMaximumPreferredTwoCharacterWords &&
           two_character[preferred].score >= kPreferredTwoCharacterMinimumScore)
        ++preferred;

    std::vector<Candidate> reordered;
    reordered.reserve(positions.size());
    for (std::size_t index = 0; index < preferred; ++index)
        reordered.push_back(std::move(two_character[index]));
    for (auto& candidate : single_character)
        reordered.push_back(std::move(candidate));
    for (std::size_t index = preferred; index < two_character.size(); ++index)
        reordered.push_back(std::move(two_character[index]));
    for (std::size_t index = 0; index < positions.size(); ++index)
        candidates[positions[index]] = std::move(reordered[index]);
}

}  // namespace

std::vector<Candidate> CandidateGenerator::generate(const ParseResult& parsed,
                                                    const std::size_t limit) const {
    if (!parsed.valid || limit == 0) return {};
    const auto search_limit = std::max<std::size_t>(32, limit);

    std::unordered_map<std::string, Candidate> unique;
    const auto store_candidate = [&unique](Candidate candidate) {
        const auto found = unique.find(candidate.text);
        if (found == unique.end() || candidate_better(candidate, found->second))
            unique.insert_or_assign(candidate.text, std::move(candidate));
    };
    std::vector<const ParsePath*> ordered_paths;
    ordered_paths.reserve(parsed.paths.size());
    for (const auto kind : {InputMatchKind::exact,
                            InputMatchKind::incomplete_completion,
                            InputMatchKind::corrected,
                            InputMatchKind::abbreviated_completion}) {
        for (const auto& path : parsed.paths) {
            if (path.match_kind == kind) ordered_paths.push_back(&path);
        }
    }
    bool exact_candidate_found = false;
    constexpr std::size_t kMaximumAssistedPathsPerKind = 16;
    std::array<std::size_t,
               static_cast<std::size_t>(InputMatchKind::abbreviated_completion) + 1>
        evaluated_paths{};
    for (const auto* path_pointer : ordered_paths) {
        const auto& path = *path_pointer;
        // Corrections are a fallback for spellings not covered by the lexicon.
        // Prefix completions remain available beside exact candidates.
        if (path.match_kind == InputMatchKind::corrected && exact_candidate_found) continue;
        if (std::any_of(path.syllables.begin(), path.syllables.end(),
                        [](const Syllable& value) { return !value.complete; })) continue;

        const auto raw_segments = source_segments(parsed, path);
        if (raw_segments.size() != path.syllables.size()) continue;
        const auto kind_index = static_cast<std::size_t>(path.match_kind);
        if (path.match_kind != InputMatchKind::exact &&
            evaluated_paths[kind_index] >= kMaximumAssistedPathsPerKind) continue;
        ++evaluated_paths[kind_index];

        std::vector<std::vector<SearchState>> chart(path.syllables.size() + 1);
        SearchState initial;
        initial.score = -input_match_penalty(path) + source_initial_bonus(parsed, path);
        chart[0].push_back(std::move(initial));
        const std::size_t beam_width = std::max<std::size_t>(16, search_limit * 4);
        const std::size_t maximum_reading_length = lexicon_.maximum_reading_length();
        for (std::size_t begin = 0; begin < path.syllables.size(); ++begin) {
            if (chart[begin].empty()) continue;
            prune(chart[begin], beam_width);
            const auto maximum_end = std::min(path.syllables.size(),
                                              begin + maximum_reading_length);
            for (std::size_t end = begin + 1; end <= maximum_end; ++end) {
                std::vector<std::string_view> reading;
                reading.reserve(end - begin);
                for (std::size_t index = begin; index < end; ++index)
                    reading.push_back(path.syllables[index].text);
                const auto entries = lexicon_.lookup(reading);
                for (const auto& state : chart[begin]) {
                    for (const auto& entry : entries) {
                        SearchState next = state;
                        next.text += entry.text;
                        next.score += unigram_score(entry.frequency);
                        if (!state.previous.empty()) next.score -= kAdditionalSegmentPenalty;
                        if (bigram_ != nullptr && !state.previous.empty())
                            next.score += bigram_->score(state.previous, entry.text);
                        next.previous = entry.text;
                        next.syllables.insert(next.syllables.end(), entry.syllables.begin(),
                                              entry.syllables.end());
                        chart[end].push_back(std::move(next));
                    }
                }
                prune(chart[end], beam_width);
            }
        }

        for (auto& state : chart.back()) {
            if (user_frequency_ != nullptr) state.score += user_frequency_->score(state.text);
            Candidate candidate{std::move(state.text), std::move(state.syllables), state.score,
                                path.match_kind, raw_segments,
                                path.syllables.back().end};
            if (path.match_kind == InputMatchKind::exact) exact_candidate_found = true;
            store_candidate(std::move(candidate));
        }

        // In addition to whole-input sentences, expose dictionary words that
        // consume a leading range. TSF can commit one of these and request new
        // candidates for the unconsumed suffix.
        const auto maximum_prefix = std::min(path.syllables.size() - 1,
                                             lexicon_.maximum_reading_length());
        for (std::size_t end = 1; end <= maximum_prefix; ++end) {
            std::vector<std::string_view> reading;
            reading.reserve(end);
            for (std::size_t index = 0; index < end; ++index)
                reading.push_back(path.syllables[index].text);
            for (const auto& entry : lexicon_.lookup(reading)) {
                auto score = unigram_score(entry.frequency) - input_match_penalty(path) +
                             source_initial_bonus(parsed, path);
                if (user_frequency_ != nullptr) score += user_frequency_->score(entry.text);
                store_candidate(Candidate{entry.text, entry.syllables, score,
                                          path.match_kind, raw_segments,
                                          path.syllables[end - 1].end});
            }
        }

        // Long input can have dozens of valid pinyin segmentations. Once an
        // exact full-input path has already supplied the requested number of
        // candidates, evaluating every lower-priority segmentation only adds
        // latency and cannot improve paging capacity.
        if (exact_candidate_found && unique.size() >= search_limit) break;
    }

    // A string such as "nm" is technically parseable as two interjection
    // syllables, but users normally intend one abbreviated character per
    // consonant. This lexicon-aware fallback avoids spending the parser's
    // bounded path budget on every possible syllable completion.
    constexpr std::string_view vowels = "aeiouv";
    const bool pure_initial_sequence = parsed.normalized_input.size() >= 2 &&
        parsed.normalized_input.size() <= 256 &&
        std::all_of(parsed.normalized_input.begin(), parsed.normalized_input.end(),
                    [vowels](const char value) {
            return value >= 'a' && value <= 'z' && vowels.find(value) == std::string_view::npos;
        });
    if (pure_initial_sequence) {
        std::vector<std::string> raw_segments;
        raw_segments.reserve(parsed.normalized_input.size());
        for (const char initial : parsed.normalized_input)
            raw_segments.emplace_back(1, initial);

        const auto beam_width = std::max<std::size_t>(16, search_limit * 4);
        std::vector<std::vector<SearchState>> chart(parsed.normalized_input.size() + 1);
        SearchState initial;
        initial.score = -kAbbreviationBasePenalty;
        chart.front().push_back(std::move(initial));
        for (std::size_t offset = 0; offset < parsed.normalized_input.size(); ++offset) {
            auto entries = lexicon_.lookup_initial(parsed.normalized_input[offset]);
            std::sort(entries.begin(), entries.end(), [](const LexiconEntry& left,
                                                         const LexiconEntry& right) {
                if (left.frequency != right.frequency) return left.frequency > right.frequency;
                if (left.syllables.front().size() != right.syllables.front().size())
                    return left.syllables.front().size() < right.syllables.front().size();
                return left.text < right.text;
            });
            if (entries.size() > beam_width) entries.resize(beam_width);
            for (const auto& state : chart[offset]) {
                for (const auto& entry : entries) {
                    SearchState next = state;
                    next.text += entry.text;
                    next.score += unigram_score(entry.frequency);
                    next.score -= static_cast<std::int64_t>(
                        entry.syllables.front().size() - 1) * kIncompleteCharacterPenalty;
                    if (preferred_initial_syllable(parsed.normalized_input[offset]) ==
                        entry.syllables.front())
                        next.score += kPreferredInitialBonus;
                    if (!state.previous.empty()) next.score -= kAdditionalSegmentPenalty;
                    if (bigram_ != nullptr && !state.previous.empty())
                        next.score += bigram_->score(state.previous, entry.text);
                    next.previous = entry.text;
                    next.syllables.push_back(entry.syllables.front());
                    chart[offset + 1].push_back(std::move(next));
                }
            }
            prune(chart[offset + 1], beam_width);
        }
        for (std::size_t consumed = 1; consumed < chart.size(); ++consumed) {
            for (auto state : chart[consumed]) {
                if (user_frequency_ != nullptr) state.score += user_frequency_->score(state.text);
                store_candidate(Candidate{std::move(state.text), std::move(state.syllables),
                                          state.score,
                                          InputMatchKind::abbreviated_completion,
                                          raw_segments, consumed});
            }
        }
    }

    // If the leading syllable is complete, let the lexicon resolve the
    // remaining compact prefixes as a whole phrase. This covers inputs such
    // as "bugd" -> bu/gan/dang without enumerating and pruning thousands of
    // g*/d* parser combinations before dictionary evidence is available.
    const auto mixed_limit = search_limit > 32 ? std::size_t{256}
                                               : std::max<std::size_t>(64, search_limit * 8);
    const auto mixed_matches = exact_candidate_found
                                   ? std::vector<AbbreviatedLexiconMatch>{}
                                   : lexicon_.lookup_mixed_abbreviation(
                                         parsed.normalized_input, mixed_limit);
    for (auto match : mixed_matches) {
        std::size_t abbreviated_segments = 0;
        for (std::size_t index = 0; index < match.entry.syllables.size(); ++index) {
            if (match.source_segments[index].size() < match.entry.syllables[index].size())
                ++abbreviated_segments;
        }
        if (abbreviated_segments < 2) continue;
        auto score = unigram_score(match.entry.frequency) + kMixedAbbreviationPhraseBonus -
                     static_cast<std::int64_t>(abbreviated_segments) *
                         kIncompleteCharacterPenalty;
        if (user_frequency_ != nullptr) score += user_frequency_->score(match.entry.text);
        store_candidate(Candidate{std::move(match.entry.text),
                                  std::move(match.entry.syllables), score,
                                  InputMatchKind::abbreviated_completion,
                                  std::move(match.source_segments),
                                  parsed.normalized_input.size()});
    }

    std::vector<Candidate> candidates;
    candidates.reserve(unique.size());
    for (auto& [text, candidate] : unique) candidates.push_back(std::move(candidate));
    const auto full_input_bytes = parsed.normalized_input.size();
    std::vector<Candidate> full;
    std::vector<Candidate> prefixes;
    for (auto& candidate : candidates) {
        if (candidate.consumed_input_bytes == full_input_bytes)
            full.push_back(std::move(candidate));
        else
            prefixes.push_back(std::move(candidate));
    }
    std::sort(full.begin(), full.end(), score_less);
    std::sort(prefixes.begin(), prefixes.end(), [](const Candidate& left,
                                                   const Candidate& right) {
        if (left.consumed_input_bytes != right.consumed_input_bytes)
            return left.consumed_input_bytes > right.consumed_input_bytes;
        return score_less(left, right);
    });
    const auto exact = std::find_if(full.begin(), full.end(), [](const Candidate& candidate) {
        return candidate.match_kind == InputMatchKind::exact;
    });
    if (exact != full.end() && exact != full.begin())
        std::rotate(full.begin(), exact, exact + 1);

    std::vector<Candidate> ordered;
    ordered.reserve(full.size() + prefixes.size());
    if (!full.empty()) {
        ordered.push_back(std::move(full.front()));
        full.erase(full.begin());
    }
    for (auto& candidate : prefixes) ordered.push_back(std::move(candidate));
    for (auto& candidate : full) ordered.push_back(std::move(candidate));
    prioritize_two_character_words(ordered);
    if (ordered.size() > limit) ordered.resize(limit);
    return ordered;
}

}  // namespace owo::engine

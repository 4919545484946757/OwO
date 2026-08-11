#include "owo/engine/candidate_generator.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <chrono>
#include <limits>
#include <string_view>
#include <unordered_map>

namespace owo::engine {
namespace {

struct SearchState {
    std::string text;
    std::string previous;
    std::vector<std::string> syllables;
    std::size_t segment_count{};
    std::int64_t score{};
    std::vector<std::size_t> segment_lengths;
};

struct OrderedParsePath {
    const ParsePath* path{};
    std::uint32_t full_span_frequency{};
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
constexpr std::int64_t kCommonInitialSeedBonus = 100'000;
constexpr std::int64_t kMaximumBoundaryCoherenceBonus = 12'000;
constexpr std::int64_t kUncoveredBoundaryCharacterPenalty = 12'000;

std::int64_t common_initial_seed_bonus(const char initial,
                                       const std::string_view text) {
    // Raw dictionary counts mix several corpora and otherwise put 大/多 ahead
    // of the conventional high-utility d shortcuts. Keep a very small seed
    // list for the default single-initial experience; all remaining characters
    // still follow learned and dictionary frequency.
    if (initial != 'd') return 0;
    constexpr std::array<std::string_view, 6> seeds{
        "的", "都", "对", "等", "到", "但"};
    const auto found = std::find(seeds.begin(), seeds.end(), text);
    if (found == seeds.end()) return 0;
    return kCommonInitialSeedBonus -
           static_cast<std::int64_t>(found - seeds.begin()) * 10'000;
}

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

void retain_frequent_entries(std::vector<LexiconEntry>& entries,
                             const std::size_t limit) {
    if (entries.size() <= limit) return;
    const auto better = [](const LexiconEntry& left, const LexiconEntry& right) {
        if (left.frequency != right.frequency) return left.frequency > right.frequency;
        return left.text < right.text;
    };
    std::nth_element(entries.begin(), entries.begin() + limit, entries.end(), better);
    entries.resize(limit);
    std::sort(entries.begin(), entries.end(), better);
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

std::string candidate_structure_key(const Candidate& candidate) {
    std::string key;
    for (const auto& segment : candidate.source_segments) {
        key += segment;
        key.push_back('\'');
    }
    key.push_back('#');
    for (const auto length : candidate.segment_lengths) {
        key += std::to_string(length);
        key.push_back(',');
    }
    return key;
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

std::vector<std::size_t> utf8_boundaries(const std::string_view text) {
    std::vector<std::size_t> result{0};
    for (std::size_t offset = 1; offset < text.size(); ++offset) {
        if ((static_cast<unsigned char>(text[offset]) & 0xc0U) != 0x80U)
            result.push_back(offset);
    }
    result.push_back(text.size());
    return result;
}

std::size_t utf8_difference_count(const std::string_view left,
                                  const std::string_view right) {
    const auto character_bytes = [](const unsigned char lead) {
        if ((lead & 0x80U) == 0) return std::size_t{1};
        if ((lead & 0xe0U) == 0xc0U) return std::size_t{2};
        if ((lead & 0xf0U) == 0xe0U) return std::size_t{3};
        return std::size_t{4};
    };
    std::size_t left_offset = 0;
    std::size_t right_offset = 0;
    std::size_t differences = 0;
    while (left_offset < left.size() && right_offset < right.size()) {
        const auto left_size = character_bytes(
            static_cast<unsigned char>(left[left_offset]));
        const auto right_size = character_bytes(
            static_cast<unsigned char>(right[right_offset]));
        if (left.substr(left_offset, left_size) !=
            right.substr(right_offset, right_size))
            ++differences;
        left_offset += left_size;
        right_offset += right_size;
    }
    differences += utf8_character_count(left.substr(left_offset));
    differences += utf8_character_count(right.substr(right_offset));
    return differences;
}

std::string_view utf8_first_character(const std::string_view text) {
    if (text.empty()) return {};
    std::size_t end = 1;
    while (end < text.size() &&
           (static_cast<unsigned char>(text[end]) & 0xc0U) == 0x80U)
        ++end;
    return text.substr(0, end);
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

void limit_two_syllable_full_candidates(std::vector<Candidate>& candidates) {
    const auto is_composed = [](const Candidate& candidate) {
        return candidate.syllables.size() == 2 && candidate.segment_count > 1;
    };
    const bool has_dictionary_word = std::any_of(
        candidates.begin(), candidates.end(), [](const Candidate& candidate) {
            return candidate.syllables.size() == 2 && candidate.segment_count == 1;
        });
    constexpr std::size_t kMaximumTwoSyllableDictionaryWords = 2;
    std::size_t dictionary_words_kept = 0;
    bool fallback_kept = false;
    candidates.erase(std::remove_if(
        candidates.begin(), candidates.end(), [&](const Candidate& candidate) {
            if (candidate.syllables.size() == 2 && candidate.segment_count == 1) {
                if (dictionary_words_kept >= kMaximumTwoSyllableDictionaryWords)
                    return true;
                ++dictionary_words_kept;
                return false;
            }
            if (!is_composed(candidate)) return false;
            if (has_dictionary_word || fallback_kept) return true;
            fallback_kept = true;
            return false;
        }), candidates.end());
}

}  // namespace

std::vector<Candidate> CandidateGenerator::generate(const ParseResult& parsed,
                                                    const std::size_t limit,
                                                    const bool contextual_ranking,
                                                    const std::string_view language_context,
                                                    CandidateGenerationMetrics* const metrics,
                                                    const std::function<bool()>& cancelled,
                                                    const bool include_model_alternatives) const {
    if (!parsed.valid || limit == 0) return {};
    // A normal first page needs only page_size + one look-ahead candidate.
    // Keeping a small quality margin avoids the former 32-result floor, which
    // expanded every beam to at least 128 states even when only five results
    // were visible.
    constexpr std::size_t kMinimumSearchLimit = 12;
    const auto search_limit = std::max(kMinimumSearchLimit, limit);

    // A lone consonant is a request for common characters across every valid
    // final, not for the first parser completion bucket (da, dai, dan, ...).
    // The lexicon's prebuilt initial index is globally frequency ordered, so
    // use it directly and retain only single-syllable, single-character
    // entries. This gives d -> 的/都/对/等/到/但... instead of allowing one
    // completion path to fill the whole result budget.
    constexpr std::string_view vowels = "aeiouv";
    const bool pure_initial_sequence = parsed.normalized_input.size() >= 2 &&
        parsed.normalized_input.size() <= 256 &&
        std::all_of(parsed.normalized_input.begin(), parsed.normalized_input.end(),
                    [vowels](const char value) {
            return value >= 'a' && value <= 'z' &&
                   vowels.find(value) == std::string_view::npos;
        });
    const bool single_initial = parsed.normalized_input.size() == 1 &&
        parsed.normalized_input.front() >= 'a' &&
        parsed.normalized_input.front() <= 'z' &&
        vowels.find(parsed.normalized_input.front()) == std::string_view::npos;
    if (single_initial) {
        const auto lookup_started = std::chrono::steady_clock::now();
        auto entries = lexicon_.lookup_initial(
            parsed.normalized_input.front(), std::max<std::size_t>(256, search_limit * 16));
        if (metrics != nullptr) {
            metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                std::chrono::duration_cast<std::chrono::microseconds>(
                    std::chrono::steady_clock::now() - lookup_started).count());
            ++metrics->lexicon_lookup_count;
        }
        std::unordered_map<std::string, Candidate> initial_candidates;
        for (const auto& entry : entries) {
            if (cancelled && cancelled()) return {};
            if (entry.syllables.size() != 1 || utf8_character_count(entry.text) != 1)
                continue;
            auto score = unigram_score(entry.frequency) +
                         common_initial_seed_bonus(parsed.normalized_input.front(),
                                                   entry.text);
            if (user_frequency_ != nullptr) {
                score += user_frequency_->score(entry.text);
                if (contextual_ranking)
                    score += user_frequency_->contextual_score(
                        parsed.normalized_input, entry.text);
                if (contextual_ranking && !language_context.empty())
                    score += user_frequency_->language_context_score(
                        language_context, parsed.normalized_input, entry.text);
            }
            Candidate candidate{entry.text, entry.syllables, score,
                                InputMatchKind::incomplete_completion,
                                {parsed.normalized_input}, 1};
            const auto found = initial_candidates.find(candidate.text);
            if (found == initial_candidates.end() ||
                candidate_better(candidate, found->second))
                initial_candidates.insert_or_assign(candidate.text,
                                                    std::move(candidate));
        }
        std::vector<Candidate> candidates;
        candidates.reserve(initial_candidates.size());
        for (auto& [text, candidate] : initial_candidates)
            candidates.push_back(std::move(candidate));
        const auto sort_started = std::chrono::steady_clock::now();
        std::sort(candidates.begin(), candidates.end(), score_less);
        if (candidates.size() > limit) candidates.resize(limit);
        if (metrics != nullptr)
            metrics->sort_us += static_cast<std::uint64_t>(
                std::chrono::duration_cast<std::chrono::microseconds>(
                    std::chrono::steady_clock::now() - sort_started).count());
        return candidates;
    }

    const bool double_initial = pure_initial_sequence &&
                                parsed.normalized_input.size() == 2;
    if (double_initial) {
        // A double-initial request has two distinct candidate sources:
        // real two-character dictionary words and common characters for the
        // first initial. Never synthesize the first group by combining
        // arbitrary characters. Alternation derives the ratio from the
        // requested result count, so no candidate page size is hard-coded.
        std::unordered_map<std::string, Candidate> dictionary_unique;
        std::unordered_map<std::string, Candidate> prefix_unique;
        const auto store_better = [](auto& destination, Candidate candidate) {
            const auto found = destination.find(candidate.text);
            if (found == destination.end() ||
                candidate_better(candidate, found->second))
                destination.insert_or_assign(candidate.text, std::move(candidate));
        };

        const auto lookup_started = std::chrono::steady_clock::now();
        auto dictionary_matches = lexicon_.lookup_mixed_abbreviation(
            parsed.normalized_input, std::max<std::size_t>(64, search_limit * 16));
        auto prefix_entries = lexicon_.lookup_initial(
            parsed.normalized_input.front(),
            std::max<std::size_t>(64, search_limit * 8));
        if (metrics != nullptr) {
            metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                std::chrono::duration_cast<std::chrono::microseconds>(
                    std::chrono::steady_clock::now() - lookup_started).count());
            metrics->lexicon_lookup_count += 2;
        }

        for (auto& match : dictionary_matches) {
            if (cancelled && cancelled()) return {};
            if (match.entry.syllables.size() != 2 ||
                match.source_segments.size() != 2 ||
                utf8_character_count(match.entry.text) != 2)
                continue;
            auto score = unigram_score(match.entry.frequency) +
                         kMixedAbbreviationPhraseBonus -
                         static_cast<std::int64_t>(
                             match.entry.syllables[0].size() +
                             match.entry.syllables[1].size() - 2) *
                             kIncompleteCharacterPenalty;
            if (user_frequency_ != nullptr) {
                score += user_frequency_->score(match.entry.text);
                if (contextual_ranking)
                    score += user_frequency_->contextual_score(
                        parsed.normalized_input, match.entry.text);
                if (contextual_ranking && !language_context.empty())
                    score += user_frequency_->language_context_score(
                        language_context, parsed.normalized_input,
                        match.entry.text);
            }
            store_better(dictionary_unique,
                         Candidate{std::move(match.entry.text),
                                   std::move(match.entry.syllables), score,
                                   InputMatchKind::abbreviated_completion,
                                   std::move(match.source_segments), 2});
        }

        for (const auto& entry : prefix_entries) {
            if (cancelled && cancelled()) return {};
            if (entry.syllables.size() != 1 ||
                utf8_character_count(entry.text) != 1)
                continue;
            auto score = unigram_score(entry.frequency);
            if (user_frequency_ != nullptr) {
                score += user_frequency_->score(entry.text);
                if (contextual_ranking)
                    score += user_frequency_->contextual_score(
                        std::string_view(parsed.normalized_input).substr(0, 1),
                        entry.text);
                if (contextual_ranking && !language_context.empty())
                    score += user_frequency_->language_context_score(
                        language_context,
                        std::string_view(parsed.normalized_input).substr(0, 1),
                        entry.text);
            }
            store_better(prefix_unique,
                         Candidate{entry.text, entry.syllables, score,
                                   InputMatchKind::abbreviated_completion,
                                   {parsed.normalized_input.substr(0, 1)}, 1});
        }

        std::vector<Candidate> dictionary;
        std::vector<Candidate> prefixes;
        dictionary.reserve(dictionary_unique.size());
        prefixes.reserve(prefix_unique.size());
        for (auto& [text, candidate] : dictionary_unique)
            dictionary.push_back(std::move(candidate));
        for (auto& [text, candidate] : prefix_unique)
            prefixes.push_back(std::move(candidate));
        std::sort(dictionary.begin(), dictionary.end(), score_less);
        std::sort(prefixes.begin(), prefixes.end(), score_less);

        std::vector<Candidate> candidates;
        candidates.reserve(std::min(limit, dictionary.size() + prefixes.size()));
        std::size_t dictionary_index = 0;
        std::size_t prefix_index = 0;
        bool dictionary_turn = true;
        while (candidates.size() < limit &&
               (dictionary_index < dictionary.size() ||
                prefix_index < prefixes.size())) {
            if (dictionary_turn && dictionary_index < dictionary.size())
                candidates.push_back(std::move(dictionary[dictionary_index++]));
            else if (!dictionary_turn && prefix_index < prefixes.size())
                candidates.push_back(std::move(prefixes[prefix_index++]));
            else if (dictionary_index < dictionary.size())
                candidates.push_back(std::move(dictionary[dictionary_index++]));
            else
                candidates.push_back(std::move(prefixes[prefix_index++]));
            dictionary_turn = !dictionary_turn;
        }
        return candidates;
    }

    std::unordered_map<std::string, Candidate> unique;
    const auto store_candidate = [&unique](Candidate candidate) {
        const auto found = unique.find(candidate.text);
        if (found == unique.end() || candidate_better(candidate, found->second))
            unique.insert_or_assign(candidate.text, std::move(candidate));
    };
    std::vector<OrderedParsePath> ordered_paths;
    ordered_paths.reserve(parsed.paths.size());
    const bool long_ambiguous_input = parsed.normalized_input.size() >= 12 &&
                                      parsed.normalized_input.find('\'') ==
                                          std::string::npos;
    // Long input uses every exact path already admitted by the parser's
    // bounded result set. A fixed first-N cutoff is unstable as a sentence
    // grows: harmless nearby boundaries can otherwise hide a later coherent
    // path. Short input retains one preferred exact interpretation.
    const std::size_t exact_path_limit = long_ambiguous_input
        ? static_cast<std::size_t>(std::count_if(
              parsed.paths.begin(), parsed.paths.end(), [](const ParsePath& path) {
                  return path.match_kind == InputMatchKind::exact;
              }))
        : 1;
    std::size_t exact_paths_added = 0;
    for (const auto kind : {InputMatchKind::exact,
                            InputMatchKind::incomplete_completion,
                            InputMatchKind::corrected,
                            InputMatchKind::abbreviated_completion}) {
        for (const auto& path : parsed.paths) {
            if (path.match_kind != kind) continue;
            // The first exact path is also the segmentation shown by the
            // pinyin preview. Evaluating other equally valid segmentations
            // lets corpus frequency silently replace wan'an with wa'nan (and
            // nan'an with na'nan), making the candidates contradict the UI.
            // Assisted paths remain plural because they intentionally offer
            // alternative completions and corrections.
            if (kind == InputMatchKind::exact) {
                if (exact_paths_added >= exact_path_limit) continue;
                ++exact_paths_added;
            }
            std::uint32_t full_span_frequency = 0;
            if (std::all_of(path.syllables.begin(), path.syllables.end(),
                            [](const Syllable& syllable) {
                                return syllable.complete;
                            })) {
                std::vector<std::string_view> reading;
                reading.reserve(path.syllables.size());
                for (const auto& syllable : path.syllables)
                    reading.push_back(syllable.text);
                const auto lookup_started = std::chrono::steady_clock::now();
                const auto entries = lexicon_.lookup(reading);
                if (metrics != nullptr) {
                    metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                        std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now() - lookup_started).count());
                    ++metrics->lexicon_lookup_count;
                }
                for (const auto& entry : entries)
                    full_span_frequency = std::max(full_span_frequency,
                                                   entry.frequency);
            }
            ordered_paths.push_back({&path, full_span_frequency});
        }
    }
    // Parser ordering is deliberately independent from a concrete lexicon.
    // Before applying the assisted-path budget, use dictionary evidence to
    // prevent an alphabetically early completion family from starving a
    // complete word. Exact paths keep their preview-defined order.
    std::stable_sort(
        ordered_paths.begin(), ordered_paths.end(),
        [](const OrderedParsePath& left, const OrderedParsePath& right) {
            if (left.path->match_kind != right.path->match_kind)
                return left.path->match_kind < right.path->match_kind;
            if (left.path->match_kind == InputMatchKind::exact) return false;
            if ((left.full_span_frequency != 0) !=
                (right.full_span_frequency != 0))
                return left.full_span_frequency != 0;
            if (left.full_span_frequency != right.full_span_frequency)
                return left.full_span_frequency > right.full_span_frequency;
            if (left.path->completion_characters !=
                right.path->completion_characters)
                return left.path->completion_characters <
                       right.path->completion_characters;
            return false;
        });
    const bool expose_long_alternatives =
        long_ambiguous_input && exact_paths_added > 1;
    bool exact_candidate_found = false;
    std::size_t evaluated_exact_paths = 0;
    constexpr std::size_t kMaximumAssistedPathsPerKind = 16;
    std::array<std::size_t,
               static_cast<std::size_t>(InputMatchKind::abbreviated_completion) + 1>
        evaluated_paths{};
    for (const auto& ordered_path : ordered_paths) {
        if (cancelled && cancelled()) return {};
        const auto& path = *ordered_path.path;
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
        if (path.match_kind == InputMatchKind::exact) ++evaluated_exact_paths;

        std::vector<std::vector<SearchState>> chart(path.syllables.size() + 1);
        SearchState initial;
        initial.score = -input_match_penalty(path) + source_initial_bonus(parsed, path);
        chart[0].push_back(std::move(initial));
        const std::size_t beam_width = long_ambiguous_input
            ? std::max<std::size_t>(64, search_limit * 8)
            : std::max<std::size_t>(16, search_limit * 4);
        const std::size_t maximum_reading_length = lexicon_.maximum_reading_length();
        for (std::size_t begin = 0; begin < path.syllables.size(); ++begin) {
            if (cancelled && cancelled()) return {};
            if (chart[begin].empty()) continue;
            prune(chart[begin], beam_width);
            const auto maximum_end = std::min(path.syllables.size(),
                                              begin + maximum_reading_length);
            for (std::size_t end = begin + 1; end <= maximum_end; ++end) {
                std::vector<std::string_view> reading;
                reading.reserve(end - begin);
                for (std::size_t index = begin; index < end; ++index)
                    reading.push_back(path.syllables[index].text);
                const auto lookup_started = std::chrono::steady_clock::now();
                auto entries = lexicon_.lookup(reading);
                if (metrics != nullptr) {
                    metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                        std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now() - lookup_started).count());
                    ++metrics->lexicon_lookup_count;
                }
                retain_frequent_entries(entries, beam_width);
                for (const auto& state : chart[begin]) {
                    for (const auto& entry : entries) {
                        SearchState next = state;
                        next.text += entry.text;
                        next.score += unigram_score(entry.frequency);
                        if (!state.previous.empty()) next.score -= kAdditionalSegmentPenalty;
                        if (bigram_ != nullptr && !state.previous.empty())
                            next.score += bigram_->score(state.previous, entry.text);
                        next.previous = entry.text;
                        ++next.segment_count;
                        next.segment_lengths.push_back(entry.syllables.size());
                        next.syllables.insert(next.syllables.end(), entry.syllables.begin(),
                                              entry.syllables.end());
                        chart[end].push_back(std::move(next));
                    }
                }
                prune(chart[end], beam_width);
            }
        }

        for (auto& state : chart.back()) {
            if (user_frequency_ != nullptr) {
                state.score += user_frequency_->score(state.text);
                if (contextual_ranking)
                    state.score += user_frequency_->contextual_score(
                        parsed.normalized_input, state.text);
                if (contextual_ranking && !language_context.empty())
                    state.score += user_frequency_->language_context_score(
                        language_context, parsed.normalized_input, state.text);
            }
            Candidate candidate{std::move(state.text), std::move(state.syllables), state.score,
                                path.match_kind, raw_segments,
                                path.syllables.back().end, state.segment_count};
            candidate.segment_lengths = std::move(state.segment_lengths);
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
            const auto lookup_started = std::chrono::steady_clock::now();
            auto prefix_entries = lexicon_.lookup(reading);
            if (metrics != nullptr) {
                metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                    std::chrono::duration_cast<std::chrono::microseconds>(
                        std::chrono::steady_clock::now() - lookup_started).count());
                ++metrics->lexicon_lookup_count;
            }
            retain_frequent_entries(prefix_entries, search_limit);
            for (const auto& entry : prefix_entries) {
                auto score = unigram_score(entry.frequency) - input_match_penalty(path) +
                             source_initial_bonus(parsed, path);
                if (user_frequency_ != nullptr) {
                    score += user_frequency_->score(entry.text);
                    if (contextual_ranking)
                        score += user_frequency_->contextual_score(
                            std::string_view(parsed.normalized_input).substr(
                                0, path.syllables[end - 1].end), entry.text);
                    if (contextual_ranking && !language_context.empty())
                        score += user_frequency_->language_context_score(
                            language_context,
                            std::string_view(parsed.normalized_input).substr(
                                0, path.syllables[end - 1].end), entry.text);
                }
                store_candidate(Candidate{entry.text, entry.syllables, score,
                                          path.match_kind, raw_segments,
                                          path.syllables[end - 1].end});
            }
        }

        // Long input can have dozens of valid pinyin segmentations. Once an
        // exact full-input path has already supplied the requested number of
        // candidates, evaluating every lower-priority segmentation only adds
        // latency and cannot improve paging capacity.
        if (((exact_candidate_found &&
              evaluated_exact_paths >= exact_paths_added) ||
             path.match_kind == InputMatchKind::incomplete_completion) &&
            unique.size() >= search_limit)
            break;
    }

    // A string such as "nm" is technically parseable as two interjection
    // syllables, but users normally intend one abbreviated character per
    // consonant. This lexicon-aware fallback avoids spending the parser's
    // bounded path budget on every possible syllable completion.
    if (pure_initial_sequence) {
        std::vector<std::string> raw_segments;
        raw_segments.reserve(parsed.normalized_input.size());
        for (const char initial : parsed.normalized_input)
            raw_segments.emplace_back(1, initial);

        const auto beam_width = std::max<std::size_t>(16, search_limit * 2);
        std::vector<std::vector<SearchState>> chart(parsed.normalized_input.size() + 1);
        SearchState initial;
        initial.score = -kAbbreviationBasePenalty;
        chart.front().push_back(std::move(initial));
        for (std::size_t offset = 0; offset < parsed.normalized_input.size(); ++offset) {
            if (cancelled && cancelled()) return {};
            const auto lookup_started = std::chrono::steady_clock::now();
            auto entries = lexicon_.lookup_initial(parsed.normalized_input[offset], beam_width);
            if (metrics != nullptr) {
                metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                    std::chrono::duration_cast<std::chrono::microseconds>(
                        std::chrono::steady_clock::now() - lookup_started).count());
                ++metrics->lexicon_lookup_count;
            }
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
                if (user_frequency_ != nullptr) {
                    state.score += user_frequency_->score(state.text);
                    if (contextual_ranking)
                        state.score += user_frequency_->contextual_score(
                            std::string_view(parsed.normalized_input).substr(0, consumed),
                            state.text);
                    if (contextual_ranking && !language_context.empty())
                        state.score += user_frequency_->language_context_score(
                            language_context,
                            std::string_view(parsed.normalized_input).substr(0, consumed),
                            state.text);
                }
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
    const bool has_stable_strict_prefix = std::any_of(
        parsed.paths.begin(), parsed.paths.end(), [](const ParsePath& path) {
            if (path.match_kind != InputMatchKind::exact || path.syllables.size() < 2 ||
                path.syllables.back().complete)
                return false;
            return std::all_of(path.syllables.begin(), path.syllables.end() - 1,
                               [](const Syllable& syllable) { return syllable.complete; });
        });
    if (cancelled && cancelled()) return {};
    const auto mixed_lookup_started = std::chrono::steady_clock::now();
    const auto mixed_matches = exact_candidate_found || has_stable_strict_prefix
                                   ? std::vector<AbbreviatedLexiconMatch>{}
                                   : lexicon_.lookup_mixed_abbreviation(
                                         parsed.normalized_input, mixed_limit);
    if (metrics != nullptr && !exact_candidate_found && !has_stable_strict_prefix) {
        metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(
                std::chrono::steady_clock::now() - mixed_lookup_started).count());
        ++metrics->lexicon_lookup_count;
    }
    for (auto match : mixed_matches) {
        if (cancelled && cancelled()) return {};
        std::size_t abbreviated_segments = 0;
        for (std::size_t index = 0; index < match.entry.syllables.size(); ++index) {
            if (match.source_segments[index].size() < match.entry.syllables[index].size())
                ++abbreviated_segments;
        }
        if (abbreviated_segments < 2) continue;
        auto score = unigram_score(match.entry.frequency) + kMixedAbbreviationPhraseBonus -
                     static_cast<std::int64_t>(abbreviated_segments) *
                         kIncompleteCharacterPenalty;
        if (user_frequency_ != nullptr) {
            score += user_frequency_->score(match.entry.text);
            if (contextual_ranking)
                score += user_frequency_->contextual_score(
                    parsed.normalized_input, match.entry.text);
            if (contextual_ranking && !language_context.empty())
                score += user_frequency_->language_context_score(
                    language_context, parsed.normalized_input, match.entry.text);
        }
        store_candidate(Candidate{std::move(match.entry.text),
                                  std::move(match.entry.syllables), score,
                                  InputMatchKind::abbreviated_completion,
                                  std::move(match.source_segments),
                                  parsed.normalized_input.size()});
    }

    std::vector<Candidate> candidates;
    candidates.reserve(unique.size());
    for (auto& [text, candidate] : unique) candidates.push_back(std::move(candidate));
    const auto sort_started = std::chrono::steady_clock::now();
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
    // Keep no more than two whole dictionary words for one two-syllable
    // reading. A composition assembled from arbitrary single-character entries
    // is not a dictionary word: omit it when any whole word exists, otherwise
    // retain one fallback. This prevents both corpus noise and Beam permutations
    // from filling pages for inputs such as chan+wu or ni+jian.
    limit_two_syllable_full_candidates(full);
    const auto apply_cross_boundary_coherence = [this, metrics](Candidate& candidate) {
        constexpr std::size_t kMaximumPhraseCharacters = 4;
        const auto character_count = candidate.syllables.size();
        if (character_count < 3 ||
            utf8_character_count(candidate.text) != character_count)
            return;

        std::vector<std::size_t> boundaries;
        boundaries.reserve(character_count + 1);
        boundaries.push_back(0);
        for (std::size_t offset = 1; offset < candidate.text.size(); ++offset) {
            if ((static_cast<unsigned char>(candidate.text[offset]) & 0xc0U) != 0x80U)
                boundaries.push_back(offset);
        }
        boundaries.push_back(candidate.text.size());
        if (boundaries.size() != character_count + 1) return;

        // Weighted interval scheduling over dictionary phrases gives a phrase
        // chain such as “以我” + “的想法” a continuity advantage without
        // repeatedly counting overlapping fragments such as every adjacent
        // pair in a noisy character sequence.
        constexpr auto unreachable = (std::numeric_limits<std::int64_t>::min)() / 4;
        std::vector<std::int64_t> best(character_count + 1, unreachable);
        best[0] = 0;
        const auto maximum_phrase = std::min(
            {kMaximumPhraseCharacters, character_count,
             lexicon_.maximum_reading_length()});
        for (std::size_t begin = 0; begin < character_count; ++begin) {
            if (best[begin] == unreachable) continue;
            best[begin + 1] = std::max(
                best[begin + 1],
                best[begin] - kUncoveredBoundaryCharacterPenalty);
            std::vector<std::string_view> reading;
            reading.reserve(maximum_phrase);
            for (std::size_t end = begin + 1;
                 end <= character_count && end - begin <= maximum_phrase; ++end) {
                reading.push_back(candidate.syllables[end - 1]);
                if (end - begin < 2 || (begin == 0 && end == character_count))
                    continue;
                const auto lookup_started = std::chrono::steady_clock::now();
                const auto entries = lexicon_.lookup(reading);
                if (metrics != nullptr) {
                    metrics->lexicon_lookup_us += static_cast<std::uint64_t>(
                        std::chrono::duration_cast<std::chrono::microseconds>(
                            std::chrono::steady_clock::now() - lookup_started).count());
                    ++metrics->lexicon_lookup_count;
                }
                const auto expected = std::string_view(candidate.text).substr(
                    boundaries[begin], boundaries[end] - boundaries[begin]);
                std::int64_t phrase_score = 0;
                for (const auto& entry : entries) {
                    if (entry.text != expected) continue;
                    phrase_score = std::max(
                        phrase_score,
                        std::min(kMaximumBoundaryCoherenceBonus,
                                 unigram_score(entry.frequency) * 3 / 2));
                }
                best[end] = std::max(best[end], best[begin] + phrase_score);
            }
        }
        candidate.coherence_score = std::max<std::int64_t>(0, best.back());
        candidate.score += candidate.coherence_score;
    };
    // Only the candidates already close enough to reach the visible/model
    // comparison set need the more detailed phrase-chain pass.
    const auto coherence_limit = std::min(
        full.size(), std::max<std::size_t>(16, limit * 2));
    for (std::size_t index = 0; index < coherence_limit; ++index)
        apply_cross_boundary_coherence(full[index]);
    if (expose_long_alternatives) {
        std::vector<std::string> scored_segmentations;
        for (std::size_t index = 0; index < coherence_limit; ++index) {
            if (full[index].match_kind == InputMatchKind::exact)
                scored_segmentations.push_back(candidate_structure_key(full[index]));
        }
        for (std::size_t index = coherence_limit; index < full.size(); ++index) {
            if (full[index].match_kind != InputMatchKind::exact) continue;
            auto key = candidate_structure_key(full[index]);
            if (std::find(scored_segmentations.begin(), scored_segmentations.end(), key) !=
                scored_segmentations.end()) continue;
            scored_segmentations.push_back(std::move(key));
            apply_cross_boundary_coherence(full[index]);
            if (scored_segmentations.size() >= exact_path_limit) break;
        }
    }
    const auto sentence_less = [](const Candidate& left, const Candidate& right) {
        constexpr std::int64_t kCoherenceDecisionMargin = 3'000;
        if (left.segment_count > 1 && right.segment_count > 1) {
            const auto difference = left.coherence_score - right.coherence_score;
            if (difference >= kCoherenceDecisionMargin) return true;
            if (difference <= -kCoherenceDecisionMargin) return false;
        }
        return score_less(left, right);
    };
    std::sort(full.begin(), full.end(), sentence_less);
    std::vector<Candidate> model_seeds;
    if (include_model_alternatives && !full.empty() &&
        full.front().syllables.size() >= 3) {
        constexpr std::size_t kMaximumModelStructures = 24;
        std::vector<std::string> texts;
        texts.reserve(kMaximumModelStructures);
        for (const auto& candidate : full) {
            if (candidate.consumed_input_bytes != full_input_bytes) continue;
            if (std::find(texts.begin(), texts.end(), candidate.text) !=
                texts.end())
                continue;
            texts.push_back(candidate.text);
            model_seeds.push_back(candidate);
            if (model_seeds.size() == kMaximumModelStructures) break;
        }
    }
    // Long compact input may expose a second segmentation, but a noisier
    // composition from the winning segmentation must not consume that slot.
    // Preserve equal-complexity dictionary variants such as 自定/自订; skip a
    // candidate such as 一宽高度自定义 when it needs more lexicon fragments
    // than 一款高度自定义.
    if (expose_long_alternatives && full.size() > 1) {
        const auto first_segments = full.front().source_segments;
        const auto first_segment_count = full.front().segment_count;
        const auto first_segment_lengths = full.front().segment_lengths;
        const auto first_text = full.front().text;
        std::vector<Candidate> selected;
        selected.reserve(2);
        selected.push_back(std::move(full.front()));
        const auto lexical_variant = std::find_if(
            full.begin() + 1, full.end(), [&](const Candidate& candidate) {
                return candidate.source_segments == first_segments &&
                       candidate.segment_lengths == first_segment_lengths &&
                       candidate.segment_count == first_segment_count &&
                       utf8_difference_count(first_text, candidate.text) == 1;
            });
        auto alternative = lexical_variant;
        if (alternative == full.end()) {
            alternative = std::find_if(
                full.begin() + 1, full.end(), [&](const Candidate& candidate) {
                    return candidate.source_segments != first_segments &&
                           candidate.segment_lengths != first_segment_lengths;
                });
        }
        if (alternative == full.end()) {
            alternative = std::find_if(
                full.begin() + 1, full.end(), [&](const Candidate& candidate) {
                    return candidate.source_segments != first_segments;
                });
        }
        if (alternative == full.end()) {
            alternative = std::find_if(
                full.begin() + 1, full.end(), [&](const Candidate& candidate) {
                    return candidate.segment_count <= first_segment_count;
                });
        }
        if (alternative != full.end())
            selected.push_back(std::move(*alternative));
        full = std::move(selected);
    }
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
    const bool multiword_input = !pure_initial_sequence && !full.empty() &&
                                 full.front().syllables.size() >= 3;
    if (multiword_input) {
        constexpr std::int64_t kSecondSentenceCoherenceMargin = 2'000;
        constexpr std::int64_t kSecondSentenceScoreMargin = 4'000;
        std::size_t sentence_count = expose_long_alternatives
                                         ? std::min<std::size_t>(2, full.size())
                                         : 1;
        if (!expose_long_alternatives && full.size() >= 2) {
            const auto& first = full[0];
            const auto& second = full[1];
            const auto coherence_gap = first.coherence_score >= second.coherence_score
                ? first.coherence_score - second.coherence_score
                : second.coherence_score - first.coherence_score;
            const auto score_gap = first.score >= second.score
                ? first.score - second.score : second.score - first.score;
            const auto first_head = utf8_first_character(first.text);
            const auto second_head = utf8_first_character(second.text);
            const auto first_tail = std::string_view(first.text).substr(first_head.size());
            const auto second_tail = std::string_view(second.text).substr(second_head.size());
            // A homophone substitution followed by the identical suffix is a
            // generated variant, not a useful second sentence. A genuinely
            // different sentence may occupy the second slot when both scores
            // remain close enough.
            if (first_tail != second_tail &&
                coherence_gap <= kSecondSentenceCoherenceMargin &&
                score_gap <= kSecondSentenceScoreMargin)
                sentence_count = 2;
        }
        for (std::size_t index = 0; index < sentence_count; ++index)
            ordered.push_back(std::move(full[index]));

        const auto leading_text = utf8_first_character(ordered.front().text);
        std::vector<Candidate> initial_characters;
        std::vector<Candidate> repeated_initial;
        std::vector<Candidate> matching_prefixes;
        std::vector<Candidate> other_prefixes;
        for (auto& candidate : prefixes) {
            if (candidate.syllables.size() == 1 &&
                utf8_character_count(candidate.text) == 1) {
                if (candidate.text == leading_text)
                    repeated_initial.push_back(std::move(candidate));
                else
                    initial_characters.push_back(std::move(candidate));
            } else if (candidate.syllables.size() == 2 &&
                       std::string_view(ordered.front().text).starts_with(
                           candidate.text)) {
                matching_prefixes.push_back(std::move(candidate));
            } else {
                other_prefixes.push_back(std::move(candidate));
            }
        }
        std::sort(initial_characters.begin(), initial_characters.end(), score_less);
        // Put the most useful partial commit from the winning sentence near
        // that sentence, followed by its first character. This keeps 这款/这
        // reachable without allowing every nested prefix to fill the page.
        const bool promoted_leading_word = !matching_prefixes.empty();
        if (promoted_leading_word) {
            ordered.push_back(std::move(matching_prefixes.front()));
            matching_prefixes.erase(matching_prefixes.begin());
        }
        if (promoted_leading_word && !repeated_initial.empty()) {
            ordered.push_back(std::move(repeated_initial.front()));
            repeated_initial.erase(repeated_initial.begin());
        }
        std::size_t initial_index = 0;
        if (!promoted_leading_word && !repeated_initial.empty()) {
            // The caller asks for one look-ahead candidate beyond its visible
            // page. Preserve the normal frequency order, but reserve the last
            // visible position for the character already used by the winning
            // sentence so it cannot disappear behind every homophone.
            const auto before_leading = limit > ordered.size() + 2
                ? std::min(initial_characters.size(),
                           limit - ordered.size() - 2)
                : std::size_t{0};
            while (initial_index < before_leading)
                ordered.push_back(std::move(initial_characters[initial_index++]));
            ordered.push_back(std::move(repeated_initial.front()));
            repeated_initial.erase(repeated_initial.begin());
        }
        while (initial_index < initial_characters.size())
            ordered.push_back(std::move(initial_characters[initial_index++]));
        for (auto& candidate : matching_prefixes)
            ordered.push_back(std::move(candidate));
        for (auto& candidate : other_prefixes)
            ordered.push_back(std::move(candidate));
    } else {
        for (auto& candidate : full) ordered.push_back(std::move(candidate));
        for (auto& candidate : prefixes) ordered.push_back(std::move(candidate));
    }
    // The pure-initial branch already groups complete abbreviation matches
    // ahead of prefix commits. The general word-length reorder intentionally
    // preserves only its first leading whole-input candidate, which would
    // recreate the old one-double-initial-candidate limit.
    if (!pure_initial_sequence && !multiword_input)
        prioritize_two_character_words(ordered);
    if (ordered.size() > limit) ordered.resize(limit);
    if (include_model_alternatives && !model_seeds.empty()) {
        constexpr std::size_t kMaximumModelAlternatives = 64;
        constexpr std::size_t kAlternativesPerReading = 32;
        std::vector<std::string> known_texts;
        known_texts.reserve(ordered.size() + kMaximumModelAlternatives);
        for (const auto& candidate : ordered)
            known_texts.push_back(candidate.text);

        // Keep a small hidden pool of already generated whole-input variants
        // for sentence-level ranking. They never occupy the basic candidate
        // page, but allow the external language model to choose a coherent
        // homophone sequence that raw single-character frequency would prune.
        for (auto seed : model_seeds) {
            if (ordered.size() >= limit + kMaximumModelAlternatives) break;
            if (std::find(known_texts.begin(), known_texts.end(), seed.text) !=
                known_texts.end())
                continue;
            seed.model_only = true;
            known_texts.push_back(seed.text);
            ordered.push_back(std::move(seed));
        }

        for (const auto& seed : model_seeds) {
            if (ordered.size() >= limit + kMaximumModelAlternatives) break;
            if (seed.syllables.size() < 5 ||
                utf8_character_count(seed.text) != seed.syllables.size())
                continue;
            const auto boundaries = utf8_boundaries(seed.text);
            std::size_t syllable_offset = 0;
            for (std::size_t segment_index = 0;
                 segment_index < seed.segment_lengths.size(); ++segment_index) {
                const auto segment_length = seed.segment_lengths[segment_index];
                const bool bridge_segment =
                    segment_length == 1 && segment_index > 0 &&
                    segment_index + 1 < seed.segment_lengths.size() &&
                    seed.segment_lengths[segment_index - 1] >= 2 &&
                    seed.segment_lengths[segment_index + 1] >= 2;
                if (bridge_segment && syllable_offset > 0 &&
                    syllable_offset + 1 < seed.syllables.size()) {
                    const std::array<std::string_view, 1> reading{
                        seed.syllables[syllable_offset]};
                    auto entries = lexicon_.lookup(reading);
                    retain_frequent_entries(entries, kAlternativesPerReading);
                    for (const auto& entry : entries) {
                        if (utf8_character_count(entry.text) != 1) continue;
                        auto text = seed.text;
                        text.replace(boundaries[syllable_offset],
                                     boundaries[syllable_offset + 1] -
                                         boundaries[syllable_offset],
                                     entry.text);
                        if (std::find(known_texts.begin(), known_texts.end(), text) !=
                            known_texts.end())
                            continue;
                        auto alternative = seed;
                        alternative.text = std::move(text);
                        alternative.model_only = true;
                        known_texts.push_back(alternative.text);
                        ordered.push_back(std::move(alternative));
                        if (ordered.size() >= limit + kMaximumModelAlternatives)
                            break;
                    }
                }
                syllable_offset += segment_length;
                if (ordered.size() >= limit + kMaximumModelAlternatives) break;
            }
        }
    }
    if (metrics != nullptr)
        metrics->sort_us += static_cast<std::uint64_t>(
            std::chrono::duration_cast<std::chrono::microseconds>(
                std::chrono::steady_clock::now() - sort_started).count());
    return ordered;
}

}  // namespace owo::engine

#include "owo/engine/full_pinyin_schema.h"

#include <algorithm>
#include <chrono>
#include <iostream>
#include <string_view>

namespace {

bool contains_path(const owo::engine::ParseResult& result,
                   const std::initializer_list<std::string_view> expected) {
    return std::any_of(result.paths.begin(), result.paths.end(), [&](const auto& path) {
        if (path.syllables.size() != expected.size()) return false;
        return std::equal(path.syllables.begin(), path.syllables.end(), expected.begin(), expected.end(),
                          [](const auto& syllable, const auto text) { return syllable.text == text; });
    });
}

bool contains_path(const owo::engine::ParseResult& result,
                   const std::initializer_list<std::string_view> expected,
                   const owo::engine::InputMatchKind match_kind) {
    return std::any_of(result.paths.begin(), result.paths.end(), [&](const auto& path) {
        if (path.match_kind != match_kind || path.syllables.size() != expected.size()) return false;
        return std::equal(path.syllables.begin(), path.syllables.end(), expected.begin(), expected.end(),
                          [](const auto& syllable, const auto text) { return syllable.text == text; });
    });
}

bool preferred_source_is(const owo::engine::ParseResult& result,
                         const std::initializer_list<std::string_view> expected) {
    const auto path = std::find_if(result.paths.begin(), result.paths.end(), [](const auto& value) {
        return value.match_kind == owo::engine::InputMatchKind::exact &&
               !value.syllables.empty();
    });
    if (path == result.paths.end() || path->syllables.size() != expected.size()) return false;
    return std::equal(path->syllables.begin(), path->syllables.end(), expected.begin(),
                      expected.end(), [&](const auto& syllable, const auto source) {
        return syllable.begin < syllable.end &&
               syllable.end <= result.normalized_input.size() &&
               std::string_view(result.normalized_input).substr(
                   syllable.begin, syllable.end - syllable.begin) == source;
    });
}

int fail(const std::string_view message) {
    std::cerr << message << '\n';
    return 1;
}

}  // namespace

int main() {
    const owo::engine::FullPinyinSchema schema;

    const auto nihao = schema.parse("NiHao");
    if (!nihao.valid || nihao.normalized_input != "nihao" ||
        !contains_path(nihao, {"ni", "hao"})) return fail("nihao parse failed");

    const auto ambiguous = schema.parse("xian");
    if (!contains_path(ambiguous, {"xian"}) || contains_path(ambiguous, {"xi", "an"}))
        return fail("unseparated xian did not prefer the whole syllable");

    const auto separated = schema.parse("xi'an");
    if (!separated.valid || !contains_path(separated, {"xi", "an"}) ||
        contains_path(separated, {"xian"})) return fail("apostrophe boundary failed");

    const auto lan = schema.parse("lan");
    if (!contains_path(lan, {"lan"}) || contains_path(lan, {"la", "n"}))
        return fail("complete lan was over-segmented");
    const auto duo = schema.parse("duo");
    if (!contains_path(duo, {"duo"}) || contains_path(duo, {"du", "o"}))
        return fail("complete duo was over-segmented");
    const auto lin = schema.parse("lin");
    if (!contains_path(lin, {"lin"}) ||
        contains_path(lin, {"ling"}, owo::engine::InputMatchKind::incomplete_completion))
        return fail("complete lin generated a ling completion");
    const auto bin = schema.parse("bin");
    if (!contains_path(bin, {"bin"}) ||
        contains_path(bin, {"bing"}, owo::engine::InputMatchKind::incomplete_completion))
        return fail("complete bin generated a bing completion");

    const auto incomplete = schema.parse("zhongg");
    if (!incomplete.valid || !incomplete.has_incomplete_syllable ||
        !contains_path(incomplete, {"zhong", "g"})) return fail("incomplete suffix failed");
    if (!contains_path(incomplete, {"zhong", "guo"},
                       owo::engine::InputMatchKind::incomplete_completion))
        return fail("incomplete suffix completion failed");

    if (!preferred_source_is(schema.parse("xingb"), {"xing", "b"}))
        return fail("xingb preferred segmentation failed");
    if (!preferred_source_is(schema.parse("xingbaf"), {"xing", "ba", "f"}))
        return fail("xingbaf preferred segmentation failed");
    if (!preferred_source_is(schema.parse("bingb"), {"bing", "b"}))
        return fail("bingb preferred segmentation failed");
    if (!preferred_source_is(schema.parse("lingm"), {"ling", "m"}))
        return fail("lingm preferred segmentation failed");
    if (!preferred_source_is(schema.parse("zhengcanf"), {"zheng", "can", "f"}))
        return fail("zhengcanf preferred segmentation failed");
    if (!preferred_source_is(schema.parse("mingd"), {"ming", "d"}))
        return fail("mingd preferred segmentation failed");
    if (!preferred_source_is(schema.parse("kenengd"), {"ke", "neng", "d"}))
        return fail("kenengd preferred segmentation failed");
    if (!preferred_source_is(schema.parse("nengd"), {"neng", "d"}) ||
        !preferred_source_is(schema.parse("pengd"), {"peng", "d"}) ||
        !preferred_source_is(schema.parse("shengx"), {"sheng", "x"}) ||
        !preferred_source_is(schema.parse("kenengx"), {"ke", "neng", "x"}) ||
        !preferred_source_is(schema.parse("zenengd"), {"ze", "neng", "d"}))
        return fail("complete-final plus trailing-initial segmentation failed");

    owo::engine::FullPinyinParseMetrics long_typo_metrics;
    const auto long_typo_started = std::chrono::steady_clock::now();
    const auto long_typo = schema.parse("muqianshizheyanh", 16, true, &long_typo_metrics);
    const auto long_typo_elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::steady_clock::now() - long_typo_started);
    if (!preferred_source_is(long_typo, {"mu", "qian", "shi", "zhe", "yan", "h"}) ||
        long_typo_elapsed > std::chrono::milliseconds(100))
        return fail("long incomplete pinyin parsing was unstable or too slow");

    const auto initial = schema.parse("b");
    if (!initial.valid || !initial.has_incomplete_syllable ||
        !contains_path(initial, {"ba"}, owo::engine::InputMatchKind::incomplete_completion) ||
        !contains_path(initial, {"bu"}, owo::engine::InputMatchKind::incomplete_completion))
        return fail("single-letter completion failed");

    if (!contains_path(schema.parse("wq"), {"wo", "qu"},
                       owo::engine::InputMatchKind::abbreviated_completion) ||
        !contains_path(schema.parse("ky"), {"ke", "yi"},
                       owo::engine::InputMatchKind::abbreviated_completion))
        return fail("multi-syllable abbreviation completion failed");
    const auto mixed_abbreviation = schema.parse("gom");
    if (!contains_path(mixed_abbreviation, {"gou", "mai"},
                       owo::engine::InputMatchKind::abbreviated_completion) ||
        !contains_path(mixed_abbreviation, {"gou", "ma"},
                       owo::engine::InputMatchKind::abbreviated_completion))
        return fail("mixed abbreviation completion failed");

    if (!contains_path(schema.parse("niaho"), {"ni", "hao"},
                       owo::engine::InputMatchKind::corrected))
        return fail("transposed-letter correction failed");
    if (contains_path(schema.parse("niaho", 32, false), {"ni", "hao"},
                      owo::engine::InputMatchKind::corrected))
        return fail("disabled correction still produced a corrected path");
    const auto separated_typo_without_correction = schema.parse("quan'loi", 32, false);
    if (separated_typo_without_correction.valid ||
        !separated_typo_without_correction.paths.empty())
        return fail("explicit invalid chunk fell back to single-letter readings");
    if (!contains_path(schema.parse("nihap"), {"ni", "hao"},
                       owo::engine::InputMatchKind::corrected))
        return fail("adjacent-key correction failed");
    if (!contains_path(schema.parse("niho"), {"ni", "hao"},
                       owo::engine::InputMatchKind::corrected))
        return fail("missing-letter correction failed");
    if (!contains_path(schema.parse("nihhao"), {"ni", "hao"},
                       owo::engine::InputMatchKind::corrected))
        return fail("extra-letter correction failed");
    if (contains_path(schema.parse("nihax"), {"ni", "hao"},
                      owo::engine::InputMatchKind::corrected))
        return fail("non-adjacent substitution was corrected");
    if (contains_path(schema.parse("nxhap"), {"ni", "hao"},
                      owo::engine::InputMatchKind::corrected))
        return fail("two edits were corrected");
    if (!contains_path(schema.parse("zhonggu"), {"zhong", "guo"},
                       owo::engine::InputMatchKind::incomplete_completion))
        return fail("complete-prefix completion failed");

    const auto capped_assisted = schema.parse("nihap", 8);
    if (capped_assisted.paths.size() > 8 ||
        !contains_path(capped_assisted, {"ni", "hao"},
                       owo::engine::InputMatchKind::corrected))
        return fail("assisted path budget starved correction");

    if (schema.parse("ni hao").valid || schema.parse("'ni").valid ||
        schema.parse("ni''hao").valid || schema.parse("").valid)
        return fail("invalid input was accepted");

    const auto limited = schema.parse("xian", 1);
    if (!limited.valid || limited.paths.size() != 1) return fail("path cap failed");

    std::string long_input;
    for (int index = 0; index < 80; ++index) {
        if (!long_input.empty()) long_input.push_back('\'');
        long_input += "ni";
    }
    const auto long_result = schema.parse(long_input);
    if (!long_result.valid || long_result.paths.empty() ||
        long_result.paths.front().syllables.size() != 80)
        return fail("long pinyin input was truncated");

    const auto long_initials = schema.parse("ffffffffff");
    if (!long_initials.valid ||
        std::none_of(long_initials.paths.begin(), long_initials.paths.end(), [](const auto& path) {
            return path.match_kind == owo::engine::InputMatchKind::abbreviated_completion;
        })) return fail("long initial sequence was not segmented");

    const auto separated_initials = schema.parse("f'f'f'f'f'f'f'f'f'f");
    if (!separated_initials.valid || separated_initials.paths.empty())
        return fail("separated incomplete initials were rejected");

    const auto long_mixed_abbreviation = schema.parse("sefsefsegsegsegsefddsgv");
    if (!long_mixed_abbreviation.valid ||
        std::none_of(long_mixed_abbreviation.paths.begin(),
                     long_mixed_abbreviation.paths.end(), [](const auto& path) {
            return path.match_kind == owo::engine::InputMatchKind::abbreviated_completion &&
                   !path.syllables.empty();
        })) return fail("long mixed abbreviation was rejected");

    owo::engine::FullPinyinParseMetrics cancelled_metrics;
    const auto cancelled_parse = schema.parse(
        "sefsefsegsegsegsefddsgv", 32, true, &cancelled_metrics,
        [] { return true; });
    if (cancelled_parse.valid || !cancelled_parse.paths.empty())
        return fail("cancelled parse continued producing paths");

    owo::engine::FullPinyinIncrementalState incremental_state;
    bool reused = false;
    const auto incremental_prefix = schema.parse_incremental(
        "niha", 16, incremental_state, nullptr, {}, &reused);
    if (!incremental_prefix.valid || reused)
        return fail("incremental parser incorrectly reused its initial request");
    const auto incremental_nihao = schema.parse_incremental(
        "nihao", 16, incremental_state, nullptr, {}, &reused);
    const auto complete_nihao = schema.parse("nihao", 16, false);
    if (!reused || !incremental_nihao.valid ||
        incremental_nihao.normalized_input != complete_nihao.normalized_input ||
        incremental_nihao.paths != complete_nihao.paths)
        return fail("niha to nihao incremental parse diverged from full parse");

    return 0;
}

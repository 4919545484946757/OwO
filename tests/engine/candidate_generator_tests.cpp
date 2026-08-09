#include "owo/engine/candidate_generator.h"
#include "owo/engine/full_pinyin_schema.h"

#include <algorithm>
#include <iostream>
#include <string_view>

namespace {

int fail(const std::string_view message) {
    std::cerr << message << '\n';
    return 1;
}

}  // namespace

int main() {
    // A deliberately small, repository-owned fixture. It validates engine
    // semantics and is not presented as an imported Rime Ice lexicon.
    const owo::engine::MemoryLexicon lexicon({
        {{"ni", "hao"}, "你好", 1000},
        {{"ni", "hao"}, "你号", 50},
        {{"xian"}, "先", 800},
        {{"xian"}, "线", 700},
        {{"xi", "an"}, "西安", 900},
        {{"zhong", "gu"}, "中古", 200},
        {{"zhong", "guo"}, "中国", 2000},
        {{"ba"}, "把", 1200},
        {{"bai"}, "白", 1000},
        {{"niao"}, "鸟", 5000},
        {{"wo", "qu"}, "我去", 1000},
        {{"wei", "qu"}, "委屈", 1200},
        {{"ke", "yi"}, "可以", 1500},
        {{"gou", "mai"}, "购买", 1600},
        {{"gou", "ma"}, "够吗", 1200},
        {{"nian", "hou"}, "年后", 10000},
    });
    const owo::engine::FullPinyinSchema schema;
    const owo::engine::CandidateGenerator generator(lexicon);

    const auto nihao = generator.generate(schema.parse("nihao"));
    if (nihao.size() != 2 || nihao[0].text != "你好" || nihao[1].text != "你号")
        return fail("real Chinese nihao candidates failed");
    const auto uppercase_nihao_parse = schema.parse("NiHao");
    const auto uppercase_nihao = generator.generate(uppercase_nihao_parse);
    if (uppercase_nihao_parse.normalized_input != "nihao" ||
        uppercase_nihao != nihao)
        return fail("uppercase pinyin normalization failed");

    const auto xian = generator.generate(schema.parse("xian"));
    if (xian.size() != 3 || xian[0].text != "西安" || xian[1].text != "先" ||
        xian[2].text != "线") return fail("ambiguous reading ranking failed");

    const auto separated = generator.generate(schema.parse("xi'an"));
    if (separated.size() != 1 || separated[0].text != "西安")
        return fail("explicit syllable boundary candidate failed");

    const auto incomplete = generator.generate(schema.parse("zhongg"));
    if (incomplete.empty() || incomplete[0].text != "中国" ||
        incomplete[0].match_kind != owo::engine::InputMatchKind::incomplete_completion)
        return fail("incomplete suffix did not generate a candidate");

    const auto mixed_incomplete = generator.generate(schema.parse("nih"));
    if (mixed_incomplete.empty() || mixed_incomplete[0].text != "你好" ||
        mixed_incomplete[0].match_kind != owo::engine::InputMatchKind::incomplete_completion)
        return fail("complete and incomplete syllables did not generate a candidate");

    const auto single_initial = generator.generate(schema.parse("b"));
    if (single_initial.empty() || single_initial[0].text != "把")
        return fail("single initial did not generate prefix candidates");

    const auto abbreviated = generator.generate(schema.parse("wq"));
    if (abbreviated.empty() || abbreviated[0].text != "我去" ||
        abbreviated[0].match_kind != owo::engine::InputMatchKind::abbreviated_completion)
        return fail("multi-syllable abbreviation did not generate candidates");

    const auto initial_abbreviation = generator.generate(schema.parse("ky"));
    if (initial_abbreviation.empty() || initial_abbreviation[0].text != "可以")
        return fail("initial abbreviation did not generate candidates");

    const auto mixed_abbreviation = generator.generate(schema.parse("gom"));
    if (std::none_of(mixed_abbreviation.begin(), mixed_abbreviation.end(), [](const auto& value) {
            return value.text == "购买";
        }) ||
        std::none_of(mixed_abbreviation.begin(), mixed_abbreviation.end(), [](const auto& value) {
            return value.text == "够吗";
        })) return fail("mixed abbreviation did not generate expected candidates");

    const auto corrected = generator.generate(schema.parse("niaho"));
    if (corrected.empty() ||
        corrected[0].match_kind != owo::engine::InputMatchKind::corrected ||
        std::none_of(corrected.begin(), corrected.end(), [](const auto& value) {
            return value.text == "你好" &&
                   value.match_kind == owo::engine::InputMatchKind::corrected;
        }))
        return fail("transposed input did not generate corrected candidates");

    const auto exact_and_completed = generator.generate(schema.parse("zhonggu"));
    if (exact_and_completed.size() < 2 || exact_and_completed[0].text != "中古" ||
        exact_and_completed[0].match_kind != owo::engine::InputMatchKind::exact ||
        std::none_of(exact_and_completed.begin(), exact_and_completed.end(), [](const auto& value) {
            return value.text == "中国" &&
                   value.match_kind == owo::engine::InputMatchKind::incomplete_completion;
        })) return fail("exact candidate priority or completion candidate failed");

    const auto limited = generator.generate(schema.parse("nihao"), 1);
    if (limited.size() != 1 || limited[0].text != "你好") return fail("candidate limit failed");

    const auto exact_without_correction = generator.generate(schema.parse("nihao"));
    if (std::any_of(exact_without_correction.begin(), exact_without_correction.end(),
                    [](const auto& value) {
                        return value.match_kind == owo::engine::InputMatchKind::corrected;
                    })) return fail("correction was mixed into an exact dictionary match");

    const owo::engine::MemoryLexicon ranged_lexicon({
        {{"ni", "hao"}, "你好", 3000},
        {{"ni"}, "你", 2500},
        {{"ma"}, "吗", 2400},
        {{"shi", "jie"}, "世界", 2800},
    });
    const owo::engine::CandidateGenerator ranged_generator(ranged_lexicon);
    const auto initial_sequence = ranged_generator.generate(schema.parse("nm"));
    if (initial_sequence.size() < 2 || initial_sequence[0].text != "你吗" ||
        initial_sequence[0].source_segments != std::vector<std::string>{"n", "m"} ||
        initial_sequence[0].consumed_input_bytes != 2 ||
        initial_sequence[1].text != "你" || initial_sequence[1].consumed_input_bytes != 1)
        return fail("initial sequence did not split into source-aligned characters");

    const auto unmodified_initial = ranged_generator.generate(schema.parse("n"));
    if (unmodified_initial.empty() ||
        unmodified_initial[0].source_segments != std::vector<std::string>{"n"} ||
        unmodified_initial[0].syllables != std::vector<std::string>{"ni"})
        return fail("incomplete preview source was replaced by dictionary reading");

    const auto ranged = ranged_generator.generate(schema.parse("nihaoshijie"));
    if (ranged.size() < 2 || ranged[0].text != "你好世界" ||
        ranged[0].consumed_input_bytes != 11 || ranged[1].text != "你好" ||
        ranged[1].consumed_input_bytes != 5 ||
        ranged[0].source_segments !=
            std::vector<std::string>{"ni", "hao", "shi", "jie"})
        return fail("whole sentence and leading-range candidates were not interleaved");

    const owo::engine::MemoryLexicon segmentation({
        {{"ni", "hao"}, "你好", 332885},
        {{"ni"}, "你", 20000000}, {{"hao"}, "好", 18000000},
        {{"ha"}, "哈", 16000000}, {{"o"}, "哦", 15000000},
    });
    const owo::engine::CandidateGenerator segmentation_generator(segmentation);
    const auto segmented = segmentation_generator.generate(schema.parse("nihao"));
    if (segmented.empty() || segmented[0].text != "你好")
        return fail("over-segmentation outranked whole word");

    const owo::engine::MemoryLexicon compositional({
        {{"ni"}, "你", 1000}, {{"ni"}, "泥", 950},
        {{"hao"}, "好", 1000}, {{"hao"}, "号", 950},
    });
    owo::engine::MemoryBigramModel bigram;
    bigram.set("你", "好", 5000);
    bigram.set("泥", "号", -5000);
    const owo::engine::CandidateGenerator beam_generator(compositional, &bigram);
    const auto composed = beam_generator.generate(schema.parse("nihao"));
    if (composed.size() != 6 || composed[0].text != "你好" ||
        composed[0].syllables != std::vector<std::string>{"ni", "hao"})
        return fail("beam search or bigram ranking failed");

    owo::engine::UserFrequencyStore user_frequency;
    user_frequency.record("泥号", 20);
    const owo::engine::CandidateGenerator personalized(compositional, nullptr, &user_frequency);
    const auto learned = personalized.generate(schema.parse("nihao"));
    if (learned.empty() || learned[0].text != "泥号")
        return fail("user frequency ranking failed");

    const owo::engine::MemoryLexicon long_lexicon({
        {{"ni"}, "N", 1000},
        {{"ni", "hao"}, "NH", 2000},
    });
    const owo::engine::CandidateGenerator long_generator(long_lexicon);
    std::string long_input;
    for (int index = 0; index < 100; ++index) long_input += "ni";
    const auto long_candidates = long_generator.generate(schema.parse(long_input));
    if (long_candidates.empty() || long_candidates.front().text != std::string(100, 'N') ||
        long_candidates.front().consumed_input_bytes != long_input.size())
        return fail("long pinyin candidate generation failed");

    const owo::engine::MemoryLexicon initial_lexicon({{{"fa"}, "F", 1000}});
    const owo::engine::CandidateGenerator initial_generator(initial_lexicon);
    const auto initial_candidates = initial_generator.generate(schema.parse("ffffffffff"));
    if (initial_candidates.empty() || initial_candidates.front().text != std::string(10, 'F') ||
        initial_candidates.front().source_segments !=
            std::vector<std::string>(10, "f"))
        return fail("long initial candidates failed");

    const owo::engine::MemoryLexicon mixed_fallback_lexicon({
        {{"shi"}, "S", 1000}, {{"e"}, "E", 1000}, {{"fa"}, "F", 1000},
        {{"ge"}, "G", 1000},  {{"de"}, "D", 1000}, {{"yu"}, "V", 1000},
    });
    const owo::engine::CandidateGenerator mixed_fallback_generator(mixed_fallback_lexicon);
    const std::string mixed_fallback_input = "sefsefsegsegsegsefddsgv";
    const auto mixed_fallback_candidates =
        mixed_fallback_generator.generate(schema.parse(mixed_fallback_input));
    if (mixed_fallback_candidates.empty() ||
        mixed_fallback_candidates.front().text != "SEFSEFSEGSEGSEGSEFDDSGV" ||
        mixed_fallback_candidates.front().consumed_input_bytes != mixed_fallback_input.size())
        return fail("long mixed fallback candidates failed");

    const owo::engine::MemoryLexicon phrase_abbreviation_lexicon({
        {{"bu", "gan", "dang"}, "BGD", 1000},
        {{"bu", "ge"}, "BG", 1000},
    });
    const owo::engine::CandidateGenerator phrase_abbreviation_generator(
        phrase_abbreviation_lexicon);
    const auto phrase_abbreviation =
        phrase_abbreviation_generator.generate(schema.parse("bugd"));
    if (phrase_abbreviation.empty() || phrase_abbreviation.front().text != "BGD" ||
        phrase_abbreviation.front().syllables !=
            std::vector<std::string>{"bu", "gan", "dang"} ||
        phrase_abbreviation.front().source_segments !=
            std::vector<std::string>{"bu", "g", "d"})
        return fail("lexicon-aware mixed abbreviation failed");

    const owo::engine::MemoryLexicon word_priority_lexicon({
        {{"shi"}, "中国", 10000}, {{"shi"}, "世界", 9000},
        {{"shi"}, "可以", 8000},  {{"shi"}, "我们", 7000},
        {{"shi"}, "你们", 6000},  {{"shi"}, "是", 1000000},
        {{"shi"}, "时", 900000},  {{"shi"}, "生僻", 1},
    });
    const owo::engine::CandidateGenerator word_priority_generator(word_priority_lexicon);
    const auto word_priority = word_priority_generator.generate(schema.parse("shi"));
    const std::vector<std::string> expected_priority{
        "中国", "世界", "可以", "我们", "你们", "是", "时", "生僻"};
    if (word_priority.size() != expected_priority.size() ||
        !std::equal(word_priority.begin(), word_priority.end(), expected_priority.begin(),
                    [](const auto& candidate, const auto& expected) {
                        return candidate.text == expected;
                    }))
        return fail("two-character word priority bands failed");
    return 0;
}

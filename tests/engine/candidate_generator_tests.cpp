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
        {{"lan"}, "LAN", 100},
        {{"la", "n"}, "LA-N", 100000},
        {{"duo"}, "DUO", 100},
        {{"du", "o"}, "DU-O", 100000},
        {{"lin"}, "LIN", 1},
        {{"ling"}, "LING", 1},
        {{"bin"}, "BIN", 1},
        {{"bing"}, "BING", 1},
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
    if (xian.size() != 2 || xian[0].text != "先" || xian[1].text != "线")
        return fail("unseparated xian did not prefer the whole syllable");

    const auto separated = generator.generate(schema.parse("xi'an"));
    if (separated.size() != 1 || separated[0].text != "西安")
        return fail("explicit syllable boundary candidate failed");

    const auto lan = generator.generate(schema.parse("lan"));
    if (lan.empty() || lan[0].text != "LAN" ||
        lan[0].source_segments != std::vector<std::string>{"lan"} ||
        std::any_of(lan.begin(), lan.end(), [](const auto& value) {
            return value.text == "LA-N";
        })) return fail("lan candidate used la+n segmentation");
    const auto duo = generator.generate(schema.parse("duo"));
    if (duo.empty() || duo[0].text != "DUO" ||
        duo[0].source_segments != std::vector<std::string>{"duo"} ||
        std::any_of(duo.begin(), duo.end(), [](const auto& value) {
            return value.text == "DU-O";
        })) return fail("duo candidate used du+o segmentation");
    const auto lin_candidates = generator.generate(schema.parse("lin"));
    if (lin_candidates.empty() || lin_candidates[0].text != "LIN" ||
        std::any_of(lin_candidates.begin(), lin_candidates.end(), [](const auto& value) {
            return value.text == "LING";
        })) return fail("lin candidates included ling completion");
    const auto bin_candidates = generator.generate(schema.parse("bin"));
    if (bin_candidates.empty() || bin_candidates[0].text != "BIN" ||
        std::any_of(bin_candidates.begin(), bin_candidates.end(), [](const auto& value) {
            return value.text == "BING";
        })) return fail("bin candidates included bing completion");

    const auto incomplete = generator.generate(schema.parse("zhongg"));
    if (incomplete.empty() || incomplete[0].text != "中国" ||
        incomplete[0].match_kind != owo::engine::InputMatchKind::incomplete_completion)
        return fail("incomplete suffix did not generate a candidate");

    const auto mixed_incomplete = generator.generate(schema.parse("nih"));
    if (mixed_incomplete.empty() || mixed_incomplete[0].text != "你好" ||
        mixed_incomplete[0].match_kind != owo::engine::InputMatchKind::incomplete_completion)
        return fail("complete and incomplete syllables did not generate a candidate");

    const owo::engine::MemoryLexicon evidence_ordered_completion_lexicon({
        {{"wei", "shen", "me"}, "为什么", 500000},
        {{"wei"}, "为", 3000000},
        {{"shen"}, "神", 2000000},
        {{"ma"}, "吗", 2500000},
    });
    const owo::engine::CandidateGenerator evidence_ordered_completion_generator(
        evidence_ordered_completion_lexicon);
    const auto evidence_ordered_completion =
        evidence_ordered_completion_generator.generate(schema.parse("weishenm", 8));
    if (evidence_ordered_completion.empty() ||
        evidence_ordered_completion.front().text != "为什么" ||
        evidence_ordered_completion.front().syllables !=
            std::vector<std::string>{"wei", "shen", "me"})
        return fail("dictionary-backed incomplete completion was starved");

    const auto single_initial = generator.generate(schema.parse("b"));
    if (single_initial.empty() || single_initial[0].text != "把")
        return fail("single initial did not generate prefix candidates");

    const owo::engine::MemoryLexicon varied_initial_lexicon({
        {{"da"}, "答", 100},   {{"dai"}, "带", 700},
        {{"dan"}, "但", 600},  {{"dao"}, "到", 650},
        {{"de"}, "的", 1000},  {{"deng"}, "等", 800},
        {{"di"}, "地", 500},   {{"dou"}, "都", 950},
        {{"dui"}, "对", 900},  {{"da"}, "生僻", 100000},
    });
    const owo::engine::CandidateGenerator varied_initial_generator(
        varied_initial_lexicon);
    const auto varied_initial = varied_initial_generator.generate(schema.parse("d"));
    const std::vector<std::string> expected_initial{
        "的", "都", "对", "等", "到", "但", "带", "地", "答"};
    if (varied_initial.size() != expected_initial.size() ||
        !std::equal(varied_initial.begin(), varied_initial.end(),
                    expected_initial.begin(),
                    [](const auto& candidate, const auto& expected) {
                        return candidate.text == expected;
                    }))
        return fail("single initial was grouped by parser completion order");

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
        {{"ni", "ma"}, "你吗", 2600},
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

    const owo::engine::MemoryLexicon ambiguous_boundary_lexicon({
        {{"wan", "an"}, "WAN_AN", 100},
        {{"wa", "nan"}, "WA_NAN", 1000000},
        {{"nan", "an"}, "NAN_AN", 100},
        {{"na", "nan"}, "NA_NAN", 1000000},
    });
    const owo::engine::CandidateGenerator ambiguous_boundary_generator(
        ambiguous_boundary_lexicon);
    const auto wanan_candidates =
        ambiguous_boundary_generator.generate(schema.parse("wanan"));
    const auto nanan_candidates =
        ambiguous_boundary_generator.generate(schema.parse("nanan"));
    if (wanan_candidates.empty() || wanan_candidates.front().text != "WAN_AN" ||
        std::any_of(wanan_candidates.begin(), wanan_candidates.end(), [](const auto& value) {
            return value.text == "WA_NAN";
        }) ||
        nanan_candidates.empty() || nanan_candidates.front().text != "NAN_AN" ||
        std::any_of(nanan_candidates.begin(), nanan_candidates.end(), [](const auto& value) {
            return value.text == "NA_NAN";
        }))
        return fail("candidate segmentation diverged from the pinyin preview");

    const owo::engine::MemoryLexicon double_initial_lexicon({
        {{"ga"}, "A", 1000}, {{"ge"}, "B", 900},
        {{"gou"}, "E", 800}, {{"gu"}, "F", 700},
        {{"da"}, "C", 1000}, {{"de"}, "D", 900},
        {{"ga", "da"}, "XY", 2000},
        {{"ge", "de"}, "UV", 1800},
        {{"gou", "dong"}, "MN", 1600},
        {{"gu", "dian"}, "OP", 1400},
    });
    const owo::engine::CandidateGenerator double_initial_generator(
        double_initial_lexicon);
    const auto double_initial_candidates =
        double_initial_generator.generate(schema.parse("gd"), 5);
    if (double_initial_candidates.size() != 5 ||
        double_initial_candidates[0].consumed_input_bytes != 2 ||
        double_initial_candidates[1].consumed_input_bytes != 1 ||
        double_initial_candidates[2].consumed_input_bytes != 2 ||
        double_initial_candidates[3].consumed_input_bytes != 1 ||
        double_initial_candidates[4].consumed_input_bytes != 2 ||
        std::any_of(double_initial_candidates.begin(),
                    double_initial_candidates.end(), [](const auto& value) {
            return value.text == "AC" || value.text == "AD" ||
                   value.text == "BC" || value.text == "BD";
        }))
        return fail("double-initial dictionary/prefix candidates were not interleaved");
    for (std::size_t dynamic_limit = 2; dynamic_limit <= 7; ++dynamic_limit) {
        const auto dynamic_candidates = double_initial_generator.generate(
            schema.parse("gd"), dynamic_limit);
        if (dynamic_candidates.size() != dynamic_limit)
            return fail("double-initial dynamic candidate limit was not filled");
        for (std::size_t index = 0; index < dynamic_candidates.size(); ++index) {
            const auto expected_consumed = index % 2 == 0 ? 2U : 1U;
            if (dynamic_candidates[index].consumed_input_bytes != expected_consumed)
                return fail("double-initial ratio depended on a fixed page size");
        }
    }

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

    const owo::engine::MemoryLexicon boundary_coherence_lexicon({
        {{"yi"}, "一", 1000000},
        {{"yi"}, "以", 100},
        {{"wo"}, "我", 1000},
        {{"de"}, "的", 1000},
        {{"shi", "jiao"}, "视角", 1000},
        {{"yi", "wo", "de"}, "以我的", 1000},
    });
    const owo::engine::CandidateGenerator boundary_coherence_generator(
        boundary_coherence_lexicon);
    const auto coherent = boundary_coherence_generator.generate(
        schema.parse("yiwodeshijiao"));
    const auto coherent_sentences = std::count_if(
        coherent.begin(), coherent.end(), [&coherent](const auto& candidate) {
            return candidate.consumed_input_bytes ==
                   coherent.front().consumed_input_bytes;
        });
    if (coherent.size() < 2 || coherent.front().text != "以我的视角" ||
        coherent[1].text != "一" || coherent_sentences != 1)
        return fail("cross-boundary dictionary coherence was ignored");

    const owo::engine::MemoryLexicon ambiguous_sentence_lexicon({
        {{"yi", "kuan"}, "一款", 5000},
        {{"gao", "du"}, "高度", 5000},
        {{"zi", "ding", "yi"}, "自定义", 5000},
        {{"yi"}, "一", 10000000},
        {{"kuan"}, "宽", 10000000},
        {{"kuang", "ao"}, "狂傲", 10000000},
        {{"du", "zi"}, "独自", 10000000},
        {{"ding", "yi"}, "定义", 10000000},
    });
    const owo::engine::CandidateGenerator ambiguous_sentence_generator(
        ambiguous_sentence_lexicon);
    const auto ambiguous_sentence_parse =
        schema.parse("yikuangaoduzidingyi", 8);
    const auto ambiguous_sentences =
        ambiguous_sentence_generator.generate(ambiguous_sentence_parse);
    const auto long_sentence_count = std::count_if(
        ambiguous_sentences.begin(), ambiguous_sentences.end(),
        [&ambiguous_sentence_parse](const auto& candidate) {
            return candidate.consumed_input_bytes ==
                   ambiguous_sentence_parse.normalized_input.size();
        });
    if (ambiguous_sentences.size() < 2 ||
        ambiguous_sentences[0].text != "一款高度自定义" ||
        ambiguous_sentences[1].text != "一狂傲独自定义" ||
        long_sentence_count != 2 ||
        ambiguous_sentences[0].source_segments ==
            ambiguous_sentences[1].source_segments)
        return fail("ambiguous long sentence alternatives were not bounded");

    const owo::engine::MemoryLexicon bridge_alternative_lexicon({
        {{"wo", "de"}, "AB", 5000},
        {{"kuang", "ao"}, "CD", 5000},
        {{"jie"}, "E", 5000},
        {{"jie"}, "F", 1000},
        {{"jie"}, "K", 500},
        {{"ming", "ji"}, "GH", 5000},
        {{"yu", "ci"}, "IJ", 5000},
    });
    const owo::engine::CandidateGenerator bridge_alternative_generator(
        bridge_alternative_lexicon);
    const auto bridge_alternatives = bridge_alternative_generator.generate(
        schema.parse("wodekuangaojiemingjiyuci", 16), 6, false, {}, nullptr, {},
        true);
    const auto first_model_only = std::find_if(
        bridge_alternatives.begin(), bridge_alternatives.end(),
        [](const auto& candidate) { return candidate.model_only; });
    if (first_model_only == bridge_alternatives.end() ||
        static_cast<std::size_t>(first_model_only - bridge_alternatives.begin()) > 6 ||
        std::none_of(first_model_only, bridge_alternatives.end(),
                     [](const auto& candidate) {
                         return candidate.text == "ABCDKGHIJ" &&
                                candidate.model_only;
                     }))
        return fail("model-only bridge alternatives were not retained");

    const owo::engine::MemoryLexicon stable_trailing_lexicon({
        {{"zhe", "kuan"}, "这款", 5000},
        {{"gao", "du"}, "高度", 5000},
        {{"zi", "ding"}, "自定", 5000},
        {{"zi", "ding"}, "自订", 4800},
        {{"zhe"}, "这", 10000000},
        {{"kuang", "ao"}, "狂傲", 10000000},
        {{"du", "zi"}, "独自", 10000000},
        {{"ding"}, "定", 10000000},
    });
    const owo::engine::CandidateGenerator stable_trailing_generator(
        stable_trailing_lexicon);
    const auto completed_trailing = stable_trailing_generator.generate(
        schema.parse("zhekuangaoduziding", 8));
    const auto incomplete_trailing = stable_trailing_generator.generate(
        schema.parse("zhekuangaoduzidin", 32));
    if (completed_trailing.size() < 2 || incomplete_trailing.size() < 2 ||
        completed_trailing[0].text != "这款高度自定" ||
        completed_trailing[1].text != "这款高度自订" ||
        incomplete_trailing[0].text != completed_trailing[0].text ||
        incomplete_trailing[1].text != completed_trailing[1].text)
        return fail("finishing a trailing syllable destabilized long candidates");

    const owo::engine::MemoryLexicon leading_prefix_lexicon({
        {{"zhe", "kuan"}, "这款", 5000},
        {{"gao", "du"}, "高度", 5000},
        {{"zi", "ding", "yi"}, "自定义", 5000},
        {{"zhe"}, "这", 1000},
        {{"zhe"}, "着", 100000},
        {{"zhe"}, "者", 90000},
    });
    const owo::engine::CandidateGenerator leading_prefix_generator(
        leading_prefix_lexicon);
    const auto leading_prefixes = leading_prefix_generator.generate(
        schema.parse("zhe'kuan'gao'du'zi'ding'yi"));
    if (leading_prefixes.size() < 4 ||
        leading_prefixes[0].text != "这款高度自定义" ||
        leading_prefixes[1].text != "这款" ||
        leading_prefixes[2].text != "这" ||
        leading_prefixes[3].text != "着")
        return fail("winning sentence prefixes were buried by alternatives");

    const owo::engine::MemoryLexicon leading_character_lexicon({
        {{"shi", "zhe", "ge"}, "ABC", 5000},
        {{"shi"}, "A", 1000},
        {{"shi"}, "B", 100000},
    });
    const owo::engine::CandidateGenerator leading_character_generator(
        leading_character_lexicon);
    const auto leading_characters = leading_character_generator.generate(
        schema.parse("shi'zhe'ge"));
    if (leading_characters.size() < 3 ||
        leading_characters[0].text != "ABC" ||
        leading_characters[1].text != "B" ||
        leading_characters[2].text != "A")
        return fail("winning sentence leading character was not retained");

    const owo::engine::MemoryLexicon compositional({
        {{"ni"}, "你", 1000}, {{"ni"}, "泥", 950},
        {{"hao"}, "好", 1000}, {{"hao"}, "号", 950},
    });
    owo::engine::MemoryBigramModel bigram;
    bigram.set("你", "好", 5000);
    bigram.set("泥", "号", -5000);
    const owo::engine::CandidateGenerator beam_generator(compositional, &bigram);
    const auto composed = beam_generator.generate(schema.parse("nihao"));
    if (composed.empty() || composed[0].text != "你好" ||
        composed[0].syllables != std::vector<std::string>{"ni", "hao"} ||
        std::count_if(composed.begin(), composed.end(), [](const auto& candidate) {
            return candidate.syllables.size() == 2 && candidate.segment_count > 1;
        }) > 1)
        return fail("beam search or bigram ranking failed");

    const owo::engine::MemoryLexicon sentence_alternative_lexicon({
        {{"jin"}, "进", 2706350},
        {{"jin"}, "仅", 466676},
        {{"bao", "liu"}, "保留", 500457},
    });
    const owo::engine::CandidateGenerator sentence_alternative_generator(
        sentence_alternative_lexicon);
    const auto sentence_alternatives = sentence_alternative_generator.generate(
        schema.parse("jinbaoliu"), 12, false, {}, nullptr, {}, true);
    const auto model_alternative = std::find_if(
        sentence_alternatives.begin(), sentence_alternatives.end(),
        [](const auto& candidate) {
            return candidate.text == "仅保留" && candidate.model_only;
        });
    if (sentence_alternatives.empty() ||
        sentence_alternatives.front().text != "进保留" ||
        model_alternative == sentence_alternatives.end())
        return fail("coherent short sentence alternative was not retained for the model");

    owo::engine::UserFrequencyStore user_frequency;
    user_frequency.record("泥号", 20);
    const owo::engine::CandidateGenerator personalized(compositional, nullptr, &user_frequency);
    const auto learned = personalized.generate(schema.parse("nihao"));
    if (learned.empty() || learned[0].text != "泥号")
        return fail("user frequency ranking failed");

    owo::engine::UserFrequencyStore language_frequency;
    language_frequency.set_sensitivity(10);
    language_frequency.record("我说", "nihao", "泥号");
    const owo::engine::CandidateGenerator language_personalized(
        compositional, nullptr, &language_frequency);
    const auto language_learned = language_personalized.generate(
        schema.parse("nihao"), 32, true, "我说");
    if (language_learned.empty() || language_learned[0].text != "泥号")
        return fail("language context ranking failed");

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

    const owo::engine::MemoryLexicon bounded_composition_lexicon({
        {{"chan"}, "产", 1000000}, {{"chan"}, "禅", 900000},
        {{"wu"}, "无", 1000000},   {{"wu"}, "五", 900000},
        {{"chan", "wu"}, "产物", 1000},
        {{"chan", "wu"}, "禅悟", 900},
        {{"chan", "wu"}, "产无", 100},
        {{"ni"}, "你", 1000000},  {{"ni"}, "拟", 900000},
        {{"jian"}, "见", 1000000}, {{"jian"}, "间", 900000},
        {{"ni", "jian"}, "拟建", 800},
    });
    const owo::engine::CandidateGenerator bounded_composition_generator(
        bounded_composition_lexicon);
    for (const auto input : {"chanwu", "nijian"}) {
        const auto bounded = bounded_composition_generator.generate(schema.parse(input), 16);
        const auto dictionary_words = std::count_if(
            bounded.begin(), bounded.end(), [](const auto& candidate) {
                return candidate.syllables.size() == 2 && candidate.segment_count == 1;
            });
        if (bounded.empty() || dictionary_words > 2 || std::any_of(
                bounded.begin(), bounded.end(), [](const auto& candidate) {
                    return candidate.syllables.size() == 2 &&
                           candidate.segment_count > 1;
                }))
            return fail("two-syllable permutations displaced dictionary words");
    }

    const owo::engine::MemoryLexicon fallback_composition_lexicon({
        {{"ni"}, "你", 1000}, {{"ni"}, "拟", 900},
        {{"jian"}, "见", 1000}, {{"jian"}, "间", 900},
    });
    const owo::engine::CandidateGenerator fallback_composition_generator(
        fallback_composition_lexicon);
    const auto fallback_compositions = fallback_composition_generator.generate(
        schema.parse("nijian"), 16);
    if (std::count_if(fallback_compositions.begin(), fallback_compositions.end(),
                      [](const auto& candidate) {
                          return candidate.syllables.size() == 2 &&
                                 candidate.segment_count > 1;
                      }) > 1)
        return fail("two-syllable fallback permutations were not bounded");

    const auto cancelled_generation = word_priority_generator.generate(
        schema.parse("shi"), 32, false, {}, nullptr, [] { return true; });
    if (!cancelled_generation.empty())
        return fail("cancelled candidate generation continued producing candidates");
    return 0;
}

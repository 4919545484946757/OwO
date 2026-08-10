#include "owo/engine/binary_lexicon.h"
#include "owo/engine/candidate_generator.h"
#include "owo/engine/full_pinyin_schema.h"

#ifdef _WIN32
#include <Windows.h>
#endif

#include <algorithm>
#include <array>
#include <chrono>
#include <filesystem>
#include <iostream>
#include <optional>
#include <string>
#include <vector>

namespace {

double percentile(const std::vector<double>& sorted, const double fraction) {
    const auto index = static_cast<std::size_t>(fraction * static_cast<double>(sorted.size() - 1));
    return sorted[index];
}

}  // namespace

int main(int argc, char** argv) {
    std::optional<std::filesystem::path> external_path;
    std::optional<std::string> custom_query;
    bool assisted = false;
    bool parse_only = false;
    for (int index = 1; index < argc; ++index) {
        if (std::string_view(argv[index]) == "--assisted") {
            if (assisted) return 2;
            assisted = true;
        } else if (std::string_view(argv[index]) == "--query") {
            if (custom_query.has_value() || index + 1 >= argc) return 2;
            custom_query = argv[++index];
        } else if (std::string_view(argv[index]) == "--parse-only") {
            parse_only = true;
        } else if (external_path.has_value()) {
            return 2;
        } else {
            external_path = std::filesystem::path(argv[index]);
        }
    }
    const bool external_lexicon = external_path.has_value();
    std::vector<owo::engine::LexiconEntry> entries{
        {{"ni", "hao"}, "你好", 1000}, {{"ni", "hao"}, "你号", 50},
        {{"xian"}, "先", 800}, {{"xian"}, "线", 700}, {{"xi", "an"}, "西安", 900},
        {{"ce", "shi"}, "测试", 600}};
    constexpr std::size_t noise_entries = 20'000;
    entries.reserve(entries.size() + noise_entries);
    for (std::size_t index = 0; index < noise_entries; ++index) {
        entries.push_back({{"synthetic", std::to_string(index)},
                           "synthetic-" + std::to_string(index), 1});
    }

    const auto path = external_lexicon
        ? *external_path
        : std::filesystem::temp_directory_path() / "owo-engine-benchmark.owolx";
    const auto written = external_lexicon
        ? owo::engine::LexiconIoResult{true, {}}
        : owo::engine::write_binary_lexicon(path, std::move(entries));
    owo::engine::BinaryLexicon lexicon;
    const auto load_started = std::chrono::steady_clock::now();
    const auto loaded = lexicon.load(path);
    const auto load_ms = std::chrono::duration<double, std::milli>(
        std::chrono::steady_clock::now() - load_started).count();
    if (!written.success || !loaded.success) return 2;

    const owo::engine::FullPinyinSchema schema;
    const owo::engine::CandidateGenerator generator(lexicon);
    constexpr std::array<std::string_view, 4> exact_queries{
        "nihao", "zhongguo", "shijie", "ceshi"};
    constexpr std::array<std::string_view, 4> assisted_queries{
        "b", "nih", "zhongg", "niaho"};
    const auto& queries = assisted ? assisted_queries : exact_queries;
    const auto query_at = [&](const std::size_t index) -> std::string_view {
        return custom_query ? std::string_view(*custom_query)
                            : queries[index % queries.size()];
    };
    if (parse_only) {
        if (!custom_query) return 2;
        owo::engine::FullPinyinParseMetrics metrics;
        const auto started = std::chrono::steady_clock::now();
        const auto parsed = schema.parse(*custom_query, 32, true, &metrics);
        const auto elapsed = std::chrono::duration<double, std::milli>(
            std::chrono::steady_clock::now() - started).count();
        std::cout << "{\"valid\":" << (parsed.valid ? "true" : "false")
                  << ",\"paths\":" << parsed.paths.size()
                  << ",\"elapsed_ms\":" << elapsed
                  << ",\"correction_us\":" << metrics.correction_us << ",\"readings\":[";
        for (std::size_t path_index = 0; path_index < parsed.paths.size(); ++path_index) {
            if (path_index != 0) std::cout << ',';
            std::cout << '\"';
            for (std::size_t index = 0; index < parsed.paths[path_index].syllables.size(); ++index) {
                if (index != 0) std::cout << '\'';
                std::cout << parsed.paths[path_index].syllables[index].text;
            }
            std::cout << '\"';
        }
        std::cout << "]}\n";
        return parsed.valid ? 0 : 3;
    }
    const std::size_t warmup_count = custom_query ? 0 : 100;
    const std::size_t sample_count = custom_query ? 1 : 1000;
    for (std::size_t index = 0; index < warmup_count; ++index)
        static_cast<void>(generator.generate(schema.parse(query_at(index)), 10));

    std::vector<double> samples;
    samples.reserve(sample_count);
    std::size_t total_candidates = 0;
    std::uint64_t total_parse_us = 0;
    std::uint64_t total_correction_us = 0;
    std::uint64_t total_lookup_us = 0;
    std::uint64_t total_sort_us = 0;
    for (std::size_t index = 0; index < sample_count; ++index) {
        const auto start = std::chrono::steady_clock::now();
        owo::engine::FullPinyinParseMetrics parse_metrics;
        const auto parsed = schema.parse(query_at(index), 32, true, &parse_metrics);
        owo::engine::CandidateGenerationMetrics generation_metrics;
        const auto candidates = generator.generate(parsed, 10, false, {}, &generation_metrics);
        const auto elapsed = std::chrono::steady_clock::now() - start;
        samples.push_back(std::chrono::duration<double, std::micro>(elapsed).count());
        total_candidates += candidates.size();
        total_parse_us += parse_metrics.normalization_us + parse_metrics.segmentation_us +
                          parse_metrics.correction_us;
        total_correction_us += parse_metrics.correction_us;
        total_lookup_us += generation_metrics.lexicon_lookup_us;
        total_sort_us += generation_metrics.sort_us;
    }
    std::sort(samples.begin(), samples.end());
    std::error_code ignored;
    if (!external_lexicon) std::filesystem::remove(path, ignored);

    unsigned processors = 0;
#ifdef _WIN32
    SYSTEM_INFO system{};
    GetSystemInfo(&system);
    processors = system.dwNumberOfProcessors;
#endif
    std::cout << "{\"benchmark\":\"owo.engine.candidate_generation\""
              << ",\"configuration\":\""
#ifdef NDEBUG
              << "Release"
#else
              << "Debug"
#endif
              << "\",\"samples\":" << sample_count << ",\"warmup\":" << warmup_count
              << ",\"lexicon_entries\":" << lexicon.size()
              << ",\"lexicon_load_ms\":" << load_ms
              << ",\"source\":\"" << (external_lexicon ? "external" : "synthetic") << "\""
              << ",\"query_set\":\""
              << (custom_query ? "custom" : assisted ? "assisted" : "exact") << "\""
              << ",\"logical_processors\":" << processors
              << ",\"total_candidates\":" << total_candidates
              << ",\"average_phase_us\":{\"parse\":" << total_parse_us / sample_count
              << ",\"correction\":" << total_correction_us / sample_count
              << ",\"lookup\":" << total_lookup_us / sample_count
              << ",\"sort\":" << total_sort_us / sample_count << "}"
              << ",\"latency_us\":{\"p50\":" << percentile(samples, 0.50)
              << ",\"p95\":" << percentile(samples, 0.95)
              << ",\"p99\":" << percentile(samples, 0.99)
              << ",\"max\":" << samples.back() << "}}\n";
    return total_candidates != 0 ? 0 : 3;
}

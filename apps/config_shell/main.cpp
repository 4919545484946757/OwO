#include "owo/config/config_monitor.h"
#include "owo/config/config_store.h"

#include <charconv>
#include <chrono>
#include <cstdint>
#include <iostream>
#include <string>
#include <string_view>

namespace {

bool parse_u32(const std::wstring_view text, std::uint32_t& output) {
    std::string ascii;
    ascii.reserve(text.size());
    for (const auto character : text) {
        if (character > 0x7f) return false;
        ascii.push_back(static_cast<char>(character));
    }
    const auto result = std::from_chars(ascii.data(), ascii.data() + ascii.size(), output);
    return result.ec == std::errc{} && result.ptr == ascii.data() + ascii.size();
}

bool parse_ascii(const std::wstring_view text, std::string& output) {
    output.clear();
    output.reserve(text.size());
    for (const auto character : text) {
        if (character > 0x7f) return false;
        output.push_back(static_cast<char>(character));
    }
    return !output.empty();
}

bool apply(owo::config::AppConfig& config, const std::wstring_view field,
           const std::wstring_view value) {
    if (field == L"candidate_page_size")
        return parse_u32(value, config.candidate_page_size);
    if (field == L"candidate_wrap_length")
        return parse_u32(value, config.candidate_wrap_length);
    if (field == L"user_learning_sensitivity")
        return parse_u32(value, config.user_learning_sensitivity);
    if (field == L"model_timeout_ms") return parse_u32(value, config.model_timeout_ms);
    if (field == L"correction_shortcut")
        return parse_ascii(value, config.correction_shortcut);
    if (field == L"language_shortcut")
        return parse_ascii(value, config.language_shortcut);
    if (field == L"raw_input_shortcut")
        return parse_ascii(value, config.raw_input_shortcut);
    bool parsed{};
    if (value == L"true") parsed = true;
    else if (value != L"false") return false;
    if (field == L"user_learning_enabled") config.user_learning_enabled = parsed;
    else if (field == L"model_ranking_enabled") config.model_ranking_enabled = parsed;
    else if (field == L"correction_shortcut_enabled")
        config.correction_shortcut_enabled = parsed;
    else if (field == L"language_shortcut_enabled")
        config.language_shortcut_enabled = parsed;
    else if (field == L"raw_input_shortcut_enabled")
        config.raw_input_shortcut_enabled = parsed;
    else return false;
    return true;
}

void usage() {
    std::cerr << "usage: owo_config_shell <path> show | repair | set <field> <value> | "
                 "set-all <page-size> <learning> <ranking> <timeout-ms> "
                 "[<correction-enabled> <correction-key> <language-enabled> <language-key> "
                 "<raw-enabled> <raw-key> [<candidate-wrap-length> "
                 "[<learning-sensitivity>]]] | watch <timeout-ms>\n";
}

}  // namespace

int wmain(const int argc, wchar_t** argv) {
    if (argc < 3) {
        usage();
        return 2;
    }
    const std::filesystem::path path(argv[1]);
    const std::wstring_view command(argv[2]);
    if (command == L"show" && argc == 3) {
        owo::config::ConfigStore store;
        const auto loaded = store.load(path);
        if (!loaded.success) {
            std::cerr << loaded.diagnostic << '\n';
            return 3;
        }
        std::cout << owo::config::serialize_config(store.snapshot());
        if (!loaded.diagnostic.empty()) std::cerr << loaded.diagnostic << '\n';
        return 0;
    }
    if (command == L"repair" && argc == 3) {
        owo::config::ConfigStore store;
        const auto loaded = store.load(path);
        if (!loaded.success) return 3;
        const auto saved = store.save(store.snapshot());
        if (!saved.success) {
            std::cerr << saved.diagnostic << '\n';
            return 5;
        }
        std::cout << (loaded.recovered_from_backup ? "repaired_from_backup" :
                      loaded.used_defaults ? "repaired_with_defaults" : "already_valid") << '\n';
        return 0;
    }
    if (command == L"set" && argc == 5) {
        owo::config::ConfigStore store;
        const auto loaded = store.load(path);
        if (!loaded.success) return 3;
        auto value = store.snapshot();
        if (!apply(value, argv[3], argv[4])) {
            std::cerr << "unknown field or invalid value type\n";
            return 4;
        }
        const auto saved = store.save(value);
        if (!saved.success) {
            std::cerr << saved.diagnostic << '\n';
            return 5;
        }
        std::cout << "saved generation=" << saved.generation << '\n';
        return 0;
    }
    if (command == L"set-all" && (argc == 7 || argc == 13 || argc == 14 || argc == 15)) {
        owo::config::ConfigStore store;
        const auto loaded = store.load(path);
        if (!loaded.success) return 3;
        auto value = store.snapshot();
        if (!apply(value, L"candidate_page_size", argv[3]) ||
            !apply(value, L"user_learning_enabled", argv[4]) ||
            !apply(value, L"model_ranking_enabled", argv[5]) ||
            !apply(value, L"model_timeout_ms", argv[6]) ||
            (argc >= 13 &&
             (!apply(value, L"correction_shortcut_enabled", argv[7]) ||
              !apply(value, L"correction_shortcut", argv[8]) ||
              !apply(value, L"language_shortcut_enabled", argv[9]) ||
              !apply(value, L"language_shortcut", argv[10]) ||
              !apply(value, L"raw_input_shortcut_enabled", argv[11]) ||
              !apply(value, L"raw_input_shortcut", argv[12]))) ||
            (argc >= 14 && !apply(value, L"candidate_wrap_length", argv[13])) ||
            (argc == 15 && !apply(value, L"user_learning_sensitivity", argv[14]))) {
            std::cerr << "invalid configuration value type\n";
            return 4;
        }
        const auto saved = store.save(value);
        if (!saved.success) {
            std::cerr << saved.diagnostic << '\n';
            return 5;
        }
        std::cout << "saved generation=" << saved.generation << '\n';
        return 0;
    }
    if (command == L"watch" && argc == 4) {
        std::uint32_t timeout_ms{};
        if (!parse_u32(argv[3], timeout_ms) || timeout_ms < 10 || timeout_ms > 60'000) return 4;
        owo::config::ConfigMonitor monitor;
        const auto started = monitor.start(path, std::chrono::milliseconds(20));
        if (!started.success) return 3;
        const auto initial_generation = monitor.generation();
        std::cout << "ready generation=" << initial_generation << '\n' << std::flush;
        if (!monitor.wait_for_generation(initial_generation, std::chrono::milliseconds(timeout_ms))) {
            std::cerr << "watch timeout\n";
            return 6;
        }
        std::cout << "changed generation=" << monitor.generation() << '\n'
                  << owo::config::serialize_config(*monitor.snapshot());
        return 0;
    }
    usage();
    return 2;
}

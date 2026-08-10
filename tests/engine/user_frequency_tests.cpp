#include "owo/engine/user_frequency.h"

#include <filesystem>
#include <fstream>
#include <iostream>
#include <span>
#include <vector>

namespace {

void append_u32(std::vector<unsigned char>& bytes, const std::uint32_t value) {
    for (unsigned shift = 0; shift < 32; shift += 8)
        bytes.push_back(static_cast<unsigned char>(value >> shift));
}

void append_u64(std::vector<unsigned char>& bytes, const std::uint64_t value) {
    for (unsigned shift = 0; shift < 64; shift += 8)
        bytes.push_back(static_cast<unsigned char>(value >> shift));
}

std::uint64_t checksum(const std::span<const unsigned char> bytes) {
    std::uint64_t value = 14695981039346656037ULL;
    for (const auto byte : bytes) { value ^= byte; value *= 1099511628211ULL; }
    return value;
}

void write_v1(const std::filesystem::path& path) {
    constexpr std::string_view text = "legacy";
    std::vector<unsigned char> payload;
    append_u32(payload, static_cast<std::uint32_t>(text.size()));
    payload.insert(payload.end(), text.begin(), text.end());
    append_u32(payload, 4);
    std::vector<unsigned char> bytes{'O', 'W', 'U', 'F'};
    append_u32(bytes, 1);
    append_u32(bytes, 1);
    append_u64(bytes, checksum(payload));
    bytes.insert(bytes.end(), payload.begin(), payload.end());
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(reinterpret_cast<const char*>(bytes.data()),
                 static_cast<std::streamsize>(bytes.size()));
}

void write_v2(const std::filesystem::path& path) {
    constexpr std::string_view input = "nihao";
    constexpr std::string_view text = "legacy-context";
    std::vector<unsigned char> payload;
    append_u32(payload, static_cast<std::uint32_t>(input.size()));
    append_u32(payload, static_cast<std::uint32_t>(text.size()));
    payload.insert(payload.end(), input.begin(), input.end());
    payload.insert(payload.end(), text.begin(), text.end());
    append_u32(payload, 3);
    std::vector<unsigned char> bytes{'O', 'W', 'U', 'F'};
    append_u32(bytes, 2);
    append_u32(bytes, 1);
    append_u64(bytes, checksum(payload));
    bytes.insert(bytes.end(), payload.begin(), payload.end());
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(reinterpret_cast<const char*>(bytes.data()),
                 static_cast<std::streamsize>(bytes.size()));
}

}  // namespace

int main() {
    auto path = std::filesystem::temp_directory_path() / "owo-user-frequency-test.bin";
    std::error_code ignored;
    std::filesystem::remove(path, ignored);
    std::filesystem::remove(path.wstring() + L".bak", ignored);

    owo::engine::UserFrequencyStore store;
    if (!store.load(path).success) return 1;
    store.record("你好", 2);
    store.record("nihao", "你好", 2);
    if (!store.flush().success) return 1;
    store.record("你好", 3);
    store.record("西安");
    store.record("我喜欢", "pingguo", "苹果", 2);
    if (!store.flush().success) return 1;

    owo::engine::UserFrequencyStore loaded;
    if (!loaded.load(path).success || loaded.count("你好") != 7 ||
        loaded.count("西安") != 1 || loaded.score("你好") <= loaded.score("西安"))
        return 1;
    if (loaded.contextual_count("nihao", "你好") != 2 ||
        loaded.contextual_count("xian", "你好") != 0) return 2;
    if (loaded.language_context_score("我喜欢", "pingguo", "苹果") <= 0 ||
        loaded.language_context_score("他喜欢", "pingguo", "苹果") != 0) return 5;
    loaded.set_sensitivity(10);
    const auto sensitive = loaded.score("你好");
    loaded.set_sensitivity(1);
    if (loaded.score("你好") >= sensitive ||
        loaded.contextual_score("nihao", "你好") <= 0) return 3;

    { std::ofstream corrupt(path, std::ios::binary | std::ios::trunc); corrupt << "broken"; }
    owo::engine::UserFrequencyStore recovered;
    const auto recovery = recovered.load(path);
    if (!recovery.success || !recovery.recovered_from_backup ||
        recovered.count("你好") != 4 ||
        recovered.contextual_count("nihao", "你好") != 2) {
        std::cerr << "backup recovery failed\n";
        return 1;
    }
    const auto legacy_path = path.wstring() + L".v1";
    write_v1(legacy_path);
    owo::engine::UserFrequencyStore legacy;
    if (!legacy.load(legacy_path).success || legacy.count("legacy") != 4 ||
        legacy.contextual_count("legacy", "legacy") != 0) return 4;
    std::filesystem::remove(legacy_path, ignored);
    const auto version_two_path = path.wstring() + L".v2";
    write_v2(version_two_path);
    owo::engine::UserFrequencyStore version_two;
    if (!version_two.load(version_two_path).success ||
        version_two.contextual_count("nihao", "legacy-context") != 3 ||
        version_two.language_context_score("context", "nihao", "legacy-context") != 0)
        return 6;
    std::filesystem::remove(version_two_path, ignored);
    std::filesystem::remove(path, ignored);
    std::filesystem::remove(path.wstring() + L".bak", ignored);
    return 0;
}

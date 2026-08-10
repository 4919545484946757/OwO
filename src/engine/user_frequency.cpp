#include "owo/engine/user_frequency.h"

#ifdef _WIN32
#include <Windows.h>
#endif

#include <algorithm>
#include <array>
#include <fstream>
#include <iterator>
#include <limits>
#include <span>
#include <vector>

namespace owo::engine {
namespace {

constexpr std::array<unsigned char, 4> kMagic{'O', 'W', 'U', 'F'};
constexpr std::uint32_t kVersion = 3;
constexpr std::size_t kHeaderSize = 20;
constexpr std::size_t kMaximumFileBytes = 16U * 1024U * 1024U;

void append_u32(std::vector<unsigned char>& out, const std::uint32_t value) {
    for (unsigned shift = 0; shift < 32; shift += 8) out.push_back(static_cast<unsigned char>(value >> shift));
}
void append_u64(std::vector<unsigned char>& out, const std::uint64_t value) {
    for (unsigned shift = 0; shift < 64; shift += 8) out.push_back(static_cast<unsigned char>(value >> shift));
}
std::uint64_t checksum(const std::span<const unsigned char> bytes) {
    std::uint64_t value = 14695981039346656037ULL;
    for (const auto byte : bytes) { value ^= byte; value *= 1099511628211ULL; }
    return value;
}
bool read_u32(const std::span<const unsigned char> bytes, std::size_t& offset, std::uint32_t& value) {
    if (offset + 4 > bytes.size()) return false;
    value = 0;
    for (unsigned index = 0; index < 4; ++index) value |= static_cast<std::uint32_t>(bytes[offset + index]) << (index * 8U);
    offset += 4;
    return true;
}
bool read_u64(const std::span<const unsigned char> bytes, std::size_t& offset, std::uint64_t& value) {
    if (offset + 8 > bytes.size()) return false;
    value = 0;
    for (unsigned index = 0; index < 8; ++index) value |= static_cast<std::uint64_t>(bytes[offset + index]) << (index * 8U);
    offset += 8;
    return true;
}

bool decode(const std::filesystem::path& path,
            std::unordered_map<std::string, std::uint32_t>& counts,
            std::unordered_map<std::string, std::uint32_t>& contextual_counts,
            std::unordered_map<std::string, std::uint32_t>& language_context_counts) {
    std::ifstream input(path, std::ios::binary);
    if (!input) return false;
    std::vector<unsigned char> bytes((std::istreambuf_iterator<char>(input)), {});
    if (bytes.size() < kHeaderSize || bytes.size() > kMaximumFileBytes ||
        !std::equal(kMagic.begin(), kMagic.end(), bytes.begin())) return false;
    std::size_t offset = 4;
    std::uint32_t version{}, entry_count{};
    std::uint64_t expected{};
    if (!read_u32(bytes, offset, version) || version < 1 || version > kVersion ||
        !read_u32(bytes, offset, entry_count) || !read_u64(bytes, offset, expected) ||
        checksum(std::span(bytes).subspan(offset)) != expected) return false;
    std::unordered_map<std::string, std::uint32_t> parsed;
    std::unordered_map<std::string, std::uint32_t> parsed_contextual;
    std::unordered_map<std::string, std::uint32_t> parsed_language_context;
    for (std::uint32_t index = 0; index < entry_count; ++index) {
        std::uint32_t context_size{}, input_size{}, size{}, count{};
        if (version >= 3 && !read_u32(bytes, offset, context_size)) return false;
        if (version >= 2 && !read_u32(bytes, offset, input_size)) return false;
        if (!read_u32(bytes, offset, size) || size == 0 || offset + size > bytes.size()) return false;
        std::string context_text;
        if (context_size != 0) {
            if (offset + context_size + input_size + size > bytes.size()) return false;
            context_text.assign(reinterpret_cast<const char*>(bytes.data() + offset), context_size);
            offset += context_size;
        }
        std::string input_text;
        if (input_size != 0) {
            if (offset + input_size + size > bytes.size()) return false;
            input_text.assign(reinterpret_cast<const char*>(bytes.data() + offset), input_size);
            offset += input_size;
        }
        std::string text(reinterpret_cast<const char*>(bytes.data() + offset), size);
        offset += size;
        if (!read_u32(bytes, offset, count) || count == 0) return false;
        if (!context_text.empty() && !input_text.empty()) {
            context_text.push_back('\0');
            context_text += input_text;
            context_text.push_back('\0');
            context_text += text;
            if (!parsed_language_context.emplace(std::move(context_text), count).second)
                return false;
        } else if (input_text.empty()) {
            if (!parsed.emplace(std::move(text), count).second) return false;
        } else {
            input_text.push_back('\0');
            input_text += text;
            if (!parsed_contextual.emplace(std::move(input_text), count).second) return false;
        }
    }
    if (offset != bytes.size()) return false;
    counts = std::move(parsed);
    contextual_counts = std::move(parsed_contextual);
    language_context_counts = std::move(parsed_language_context);
    return true;
}

std::vector<unsigned char> encode(
    const std::unordered_map<std::string, std::uint32_t>& counts,
    const std::unordered_map<std::string, std::uint32_t>& contextual_counts,
    const std::unordered_map<std::string, std::uint32_t>& language_context_counts) {
    struct Entry { std::string context; std::string input; std::string text; std::uint32_t count{}; };
    std::vector<Entry> ordered;
    ordered.reserve(counts.size() + contextual_counts.size() + language_context_counts.size());
    for (const auto& [text, count] : counts) ordered.push_back({{}, {}, text, count});
    for (const auto& [key, count] : contextual_counts) {
        const auto separator = key.find('\0');
        if (separator == std::string::npos || separator == 0 || separator + 1 >= key.size()) continue;
        ordered.push_back({{}, key.substr(0, separator), key.substr(separator + 1), count});
    }
    for (const auto& [key, count] : language_context_counts) {
        const auto first = key.find('\0');
        const auto second = first == std::string::npos ? std::string::npos
                                                       : key.find('\0', first + 1);
        if (first == std::string::npos || second == std::string::npos || first == 0 ||
            second == first + 1 || second + 1 >= key.size()) continue;
        ordered.push_back({key.substr(0, first), key.substr(first + 1, second - first - 1),
                           key.substr(second + 1), count});
    }
    std::sort(ordered.begin(), ordered.end(), [](const Entry& left, const Entry& right) {
        if (left.context != right.context) return left.context < right.context;
        if (left.input != right.input) return left.input < right.input;
        return left.text < right.text;
    });
    std::vector<unsigned char> payload;
    for (const auto& entry : ordered) {
        append_u32(payload, static_cast<std::uint32_t>(entry.context.size()));
        append_u32(payload, static_cast<std::uint32_t>(entry.input.size()));
        append_u32(payload, static_cast<std::uint32_t>(entry.text.size()));
        payload.insert(payload.end(), entry.context.begin(), entry.context.end());
        payload.insert(payload.end(), entry.input.begin(), entry.input.end());
        payload.insert(payload.end(), entry.text.begin(), entry.text.end());
        append_u32(payload, entry.count);
    }
    std::vector<unsigned char> output(kMagic.begin(), kMagic.end());
    append_u32(output, kVersion);
    append_u32(output, static_cast<std::uint32_t>(ordered.size()));
    append_u64(output, checksum(payload));
    output.insert(output.end(), payload.begin(), payload.end());
    return output;
}

}  // namespace

UserFrequencyIoResult UserFrequencyStore::load(const std::filesystem::path& path) {
    path_ = path;
    counts_.clear();
    contextual_counts_.clear();
    language_context_counts_.clear();
    if (!std::filesystem::exists(path)) return {true, false, {}};
    if (decode(path, counts_, contextual_counts_, language_context_counts_)) return {true, false, {}};
    auto backup = path; backup += L".bak";
    if (decode(backup, counts_, contextual_counts_, language_context_counts_)) return {true, true, {}};
    return {false, false, "user frequency and backup are invalid"};
}

void UserFrequencyStore::record(const std::string_view input, const std::string_view text,
                                const std::uint32_t amount) {
    record(text, amount);
    if (input.empty() || text.empty() || amount == 0) return;
    std::string key(input);
    key.push_back('\0');
    key += text;
    auto& value = contextual_counts_[std::move(key)];
    value = (std::numeric_limits<std::uint32_t>::max)() - value < amount
                ? (std::numeric_limits<std::uint32_t>::max)() : value + amount;
}

void UserFrequencyStore::record(const std::string_view context, const std::string_view input,
                                const std::string_view text, const std::uint32_t amount) {
    record(input, text, amount);
    if (context.empty() || input.empty() || text.empty() || amount == 0) return;
    std::string key(context);
    key.push_back('\0');
    key += input;
    key.push_back('\0');
    key += text;
    auto& value = language_context_counts_[std::move(key)];
    value = (std::numeric_limits<std::uint32_t>::max)() - value < amount
                ? (std::numeric_limits<std::uint32_t>::max)() : value + amount;
}

void UserFrequencyStore::set_sensitivity(const std::uint32_t sensitivity) noexcept {
    sensitivity_ = std::clamp(sensitivity, 1U, 10U);
}

void UserFrequencyStore::record(const std::string_view text, const std::uint32_t amount) {
    if (text.empty() || amount == 0) return;
    auto& value = counts_[std::string(text)];
    value = (std::numeric_limits<std::uint32_t>::max)() - value < amount
                ? (std::numeric_limits<std::uint32_t>::max)() : value + amount;
}

std::uint32_t UserFrequencyStore::count(const std::string_view text) const {
    const auto found = counts_.find(std::string(text));
    return found == counts_.end() ? 0 : found->second;
}

std::uint32_t UserFrequencyStore::contextual_count(const std::string_view input,
                                                   const std::string_view text) const {
    std::string key(input);
    key.push_back('\0');
    key += text;
    const auto found = contextual_counts_.find(key);
    return found == contextual_counts_.end() ? 0 : found->second;
}

std::int64_t UserFrequencyStore::score(const std::string_view text) const {
    const auto gain = 250LL + static_cast<std::int64_t>(sensitivity_ - 1U) * 250LL;
    return static_cast<std::int64_t>(count(text)) * gain;
}

std::int64_t UserFrequencyStore::contextual_score(const std::string_view input,
                                                  const std::string_view text) const {
    const auto gain = 500LL + static_cast<std::int64_t>(sensitivity_ - 1U) * 500LL;
    return static_cast<std::int64_t>(contextual_count(input, text)) * gain;
}

std::int64_t UserFrequencyStore::language_context_score(
    const std::string_view context, const std::string_view input,
    const std::string_view text) const {
    std::string key(context);
    key.push_back('\0');
    key += input;
    key.push_back('\0');
    key += text;
    const auto found = language_context_counts_.find(key);
    if (found == language_context_counts_.end()) return 0;
    const auto gain = 1'000LL + static_cast<std::int64_t>(sensitivity_ - 1U) * 1'000LL;
    return static_cast<std::int64_t>(found->second) * gain;
}

UserFrequencyIoResult UserFrequencyStore::flush() const {
    if (path_.empty()) return {false, false, "user frequency path is not set"};
    const auto bytes = encode(counts_, contextual_counts_, language_context_counts_);
    if (bytes.size() > kMaximumFileBytes) return {false, false, "user frequency exceeds size limit"};
    auto temporary = path_; temporary += L".tmp";
    auto backup = path_; backup += L".bak";
    { std::ofstream output(temporary, std::ios::binary | std::ios::trunc);
      if (!output) return {false, false, "cannot open temporary file"};
      output.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
      output.flush();
      if (!output) return {false, false, "cannot write temporary file"}; }
#ifdef _WIN32
    if (std::filesystem::exists(path_) && !CopyFileW(path_.c_str(), backup.c_str(), FALSE))
        return {false, false, "cannot update backup"};
    if (!MoveFileExW(temporary.c_str(), path_.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH))
        return {false, false, "cannot atomically replace user frequency"};
#else
    std::error_code error;
    if (std::filesystem::exists(path_)) std::filesystem::copy_file(path_, backup, std::filesystem::copy_options::overwrite_existing, error);
    std::filesystem::rename(temporary, path_, error);
    if (error) return {false, false, "cannot replace user frequency"};
#endif
    return {true, false, {}};
}

}  // namespace owo::engine

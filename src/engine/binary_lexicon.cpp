#include "owo/engine/binary_lexicon.h"

#include "lexicon_match.h"

#include <algorithm>
#include <array>
#include <charconv>
#include <cstring>
#include <fstream>
#include <iterator>
#include <limits>
#include <map>
#include <optional>
#include <set>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <Windows.h>
#endif

namespace owo::engine {
namespace {

constexpr std::array<unsigned char, 4> kMagic{'O', 'W', 'L', 'X'};
constexpr std::size_t kV1HeaderSize = 20;
constexpr std::size_t kV2HeaderSize = 96;
constexpr std::size_t kMaximumFileBytes = 256U * 1024U * 1024U;
constexpr std::size_t kMaximumEntries = 4U * 1024U * 1024U;

enum V2HeaderField : std::size_t {
    version_field = 4,
    header_size_field = 8,
    file_size_field = 12,
    entry_count_field = 16,
    syllable_count_field = 20,
    syllable_id_count_field = 24,
    syllable_pool_size_field = 28,
    text_pool_size_field = 32,
    initial_index_count_field = 36,
    mixed_bucket_count_field = 40,
    mixed_index_count_field = 44,
    maximum_reading_length_field = 48,
    entries_offset_field = 52,
    syllable_ids_offset_field = 56,
    syllable_records_offset_field = 60,
    syllable_pool_offset_field = 64,
    text_pool_offset_field = 68,
    initial_ranges_offset_field = 72,
    initial_indices_offset_field = 76,
    mixed_buckets_offset_field = 80,
    mixed_indices_offset_field = 84,
    payload_checksum_low_field = 88,
    payload_checksum_high_field = 92,
};

struct DiskEntry {
    std::uint32_t syllable_offset{};
    std::uint32_t text_offset{};
    std::uint32_t frequency{};
    std::uint16_t syllable_count{};
    std::uint16_t text_size{};
};

struct DiskSyllableRecord {
    std::uint32_t offset{};
    std::uint16_t size{};
    std::uint16_t reserved{};
};

struct DiskRange {
    std::uint32_t offset{};
    std::uint32_t count{};
};

struct DiskMixedBucket {
    std::uint32_t key{};
    std::uint32_t offset{};
    std::uint32_t count{};
};

static_assert(sizeof(DiskEntry) == 16);
static_assert(sizeof(DiskSyllableRecord) == 8);
static_assert(sizeof(DiskRange) == 8);
static_assert(sizeof(DiskMixedBucket) == 12);

void append_u16(std::vector<unsigned char>& out, const std::uint16_t value) {
    out.push_back(static_cast<unsigned char>(value));
    out.push_back(static_cast<unsigned char>(value >> 8U));
}

void append_u32(std::vector<unsigned char>& out, const std::uint32_t value) {
    for (unsigned shift = 0; shift < 32; shift += 8) out.push_back(static_cast<unsigned char>(value >> shift));
}

void append_u64(std::vector<unsigned char>& out, const std::uint64_t value) {
    for (unsigned shift = 0; shift < 64; shift += 8) out.push_back(static_cast<unsigned char>(value >> shift));
}

void patch_u32(std::vector<unsigned char>& out, const std::size_t offset,
               const std::uint32_t value) {
    for (unsigned shift = 0; shift < 32; shift += 8)
        out[offset + shift / 8] = static_cast<unsigned char>(value >> shift);
}

void align_u32(std::vector<unsigned char>& out) {
    while (out.size() % alignof(std::uint32_t) != 0) out.push_back(0);
}

template <typename Value>
void append_pod_vector(std::vector<unsigned char>& out, const std::vector<Value>& values) {
    if (values.empty()) return;
    const auto* begin = reinterpret_cast<const unsigned char*>(values.data());
    out.insert(out.end(), begin, begin + values.size() * sizeof(Value));
}

std::uint64_t checksum(const std::span<const unsigned char> bytes) {
    std::uint64_t value = 14695981039346656037ULL;
    for (const auto byte : bytes) {
        value ^= byte;
        value *= 1099511628211ULL;
    }
    return value;
}

#ifdef _WIN32
bool validation_cache_matches(const std::filesystem::path& path, const HANDLE file,
                              const std::uint64_t file_size,
                              const std::uint32_t version) {
    auto validation_path = path;
    validation_path += L".validation";
    std::ifstream stream(validation_path, std::ios::binary);
    if (!stream) return false;
    const std::string content((std::istreambuf_iterator<char>(stream)), {});
    if (!content.starts_with("OWO_LEXICON_VALIDATION_V1")) return false;
    const auto value = [&](const std::string_view key) -> std::optional<std::string_view> {
        const auto marker = std::string(key) + '=';
        const auto begin = content.find(marker);
        if (begin == std::string::npos ||
            (begin != 0 && content[begin - 1] != '\n')) return std::nullopt;
        const auto value_begin = begin + marker.size();
        const auto end = content.find_first_of("\r\n", value_begin);
        return std::string_view(content).substr(value_begin, end - value_begin);
    };
    const auto parse_u64 = [&](const std::string_view key) -> std::optional<std::uint64_t> {
        const auto text = value(key);
        if (!text) return std::nullopt;
        std::uint64_t parsed{};
        const auto result = std::from_chars(text->data(), text->data() + text->size(), parsed);
        if (result.ec != std::errc{} || result.ptr != text->data() + text->size())
            return std::nullopt;
        return parsed;
    };
    const auto cached_version = parse_u64("version");
    const auto cached_size = parse_u64("size");
    const auto cached_write_time = parse_u64("write_time_utc_ticks");
    const auto cached_sha256 = value("sha256");
    FILETIME write_time{};
    if (!cached_version || !cached_size || !cached_write_time || !cached_sha256 ||
        cached_sha256->size() != 64 || !GetFileTime(file, nullptr, nullptr, &write_time))
        return false;
    const auto windows_ticks = (static_cast<std::uint64_t>(write_time.dwHighDateTime) << 32U) |
                               write_time.dwLowDateTime;
    constexpr std::uint64_t dotnet_epoch_ticks = 504911232000000000ULL;
    return *cached_version == version && *cached_size == file_size &&
           *cached_write_time == windows_ticks + dotnet_epoch_ticks;
}
#endif

template <typename Integer>
bool read_integer(const std::span<const unsigned char> bytes, std::size_t& offset, Integer& value) {
    if (offset + sizeof(Integer) > bytes.size()) return false;
    value = 0;
    for (std::size_t index = 0; index < sizeof(Integer); ++index)
        value |= static_cast<Integer>(bytes[offset + index]) << (index * 8U);
    offset += sizeof(Integer);
    return true;
}

bool entry_less(const LexiconEntry& left, const LexiconEntry& right) {
    if (left.syllables != right.syllables) return left.syllables < right.syllables;
    if (left.text != right.text) return left.text < right.text;
    return left.frequency > right.frequency;
}

}  // namespace

LexiconIoResult write_binary_lexicon(const std::filesystem::path& path,
                                     std::vector<LexiconEntry> entries) {
    if (entries.size() > kMaximumEntries) return {false, "too many entries"};
    std::sort(entries.begin(), entries.end(), entry_less);

    std::set<std::string> unique_syllables;
    for (const auto& entry : entries) {
        if (entry.text.empty() || entry.text.size() > std::numeric_limits<std::uint16_t>::max() ||
            entry.syllables.empty() || entry.syllables.size() > std::numeric_limits<std::uint16_t>::max())
            return {false, "entry field is empty or too large"};
        for (const auto& syllable : entry.syllables) {
            if (syllable.empty() || syllable.size() > 255) return {false, "invalid syllable length"};
            unique_syllables.insert(syllable);
        }
    }
    if (unique_syllables.size() > std::numeric_limits<std::uint16_t>::max())
        return {false, "too many unique syllables"};

    std::vector<std::string> syllables(unique_syllables.begin(), unique_syllables.end());
    std::unordered_map<std::string, std::uint16_t> syllable_to_id;
    syllable_to_id.reserve(syllables.size());
    for (std::size_t index = 0; index < syllables.size(); ++index)
        syllable_to_id.emplace(syllables[index], static_cast<std::uint16_t>(index));

    std::vector<DiskEntry> disk_entries;
    std::vector<std::uint16_t> syllable_ids;
    std::string text_pool;
    disk_entries.reserve(entries.size());
    std::size_t maximum_reading_length = 0;
    for (const auto& entry : entries) {
        if (syllable_ids.size() > std::numeric_limits<std::uint32_t>::max() ||
            text_pool.size() > std::numeric_limits<std::uint32_t>::max())
            return {false, "lexicon section exceeds 32-bit format limit"};
        DiskEntry disk_entry;
        disk_entry.syllable_offset = static_cast<std::uint32_t>(syllable_ids.size());
        disk_entry.text_offset = static_cast<std::uint32_t>(text_pool.size());
        disk_entry.frequency = entry.frequency;
        disk_entry.syllable_count = static_cast<std::uint16_t>(entry.syllables.size());
        disk_entry.text_size = static_cast<std::uint16_t>(entry.text.size());
        for (const auto& syllable : entry.syllables)
            syllable_ids.push_back(syllable_to_id.at(syllable));
        text_pool.append(entry.text);
        maximum_reading_length = std::max(maximum_reading_length, entry.syllables.size());
        disk_entries.push_back(disk_entry);
    }

    std::vector<DiskSyllableRecord> syllable_records;
    std::string syllable_pool;
    syllable_records.reserve(syllables.size());
    for (const auto& syllable : syllables) {
        if (syllable_pool.size() > std::numeric_limits<std::uint32_t>::max())
            return {false, "syllable pool exceeds 32-bit format limit"};
        syllable_records.push_back({static_cast<std::uint32_t>(syllable_pool.size()),
                                    static_cast<std::uint16_t>(syllable.size()), 0});
        syllable_pool.append(syllable);
    }

    std::array<std::vector<std::uint32_t>, 26> initial_entries;
    std::map<std::uint32_t, std::vector<std::uint32_t>> mixed_entries;
    for (std::uint32_t index = 0; index < disk_entries.size(); ++index) {
        const auto& entry = disk_entries[index];
        const auto first_id = syllable_ids[entry.syllable_offset];
        if (entry.syllable_count == 1) {
            const auto initial = syllables[first_id].front();
            if (initial >= 'a' && initial <= 'z') initial_entries[initial - 'a'].push_back(index);
            continue;
        }
        const auto second_id = syllable_ids[entry.syllable_offset + 1];
        const auto initial = syllables[second_id].front();
        if (initial < 'a' || initial > 'z') continue;
        const auto key = (static_cast<std::uint32_t>(first_id) << 8U) |
                         static_cast<unsigned char>(initial);
        mixed_entries[key].push_back(index);
    }
    const auto initial_less = [&](const std::uint32_t left_index,
                                  const std::uint32_t right_index) {
        const auto& left = disk_entries[left_index];
        const auto& right = disk_entries[right_index];
        if (left.frequency != right.frequency) return left.frequency > right.frequency;
        const auto& left_syllable = syllables[syllable_ids[left.syllable_offset]];
        const auto& right_syllable = syllables[syllable_ids[right.syllable_offset]];
        if (left_syllable.size() != right_syllable.size())
            return left_syllable.size() < right_syllable.size();
        return std::string_view(text_pool).substr(left.text_offset, left.text_size) <
               std::string_view(text_pool).substr(right.text_offset, right.text_size);
    };
    for (auto& bucket : initial_entries) std::sort(bucket.begin(), bucket.end(), initial_less);

    std::vector<DiskRange> initial_ranges;
    std::vector<std::uint32_t> initial_indices;
    initial_ranges.reserve(26);
    for (const auto& bucket : initial_entries) {
        initial_ranges.push_back({static_cast<std::uint32_t>(initial_indices.size()),
                                  static_cast<std::uint32_t>(bucket.size())});
        initial_indices.insert(initial_indices.end(), bucket.begin(), bucket.end());
    }
    std::vector<DiskMixedBucket> mixed_buckets;
    std::vector<std::uint32_t> mixed_indices;
    mixed_buckets.reserve(mixed_entries.size());
    for (const auto& [key, bucket] : mixed_entries) {
        mixed_buckets.push_back({key, static_cast<std::uint32_t>(mixed_indices.size()),
                                 static_cast<std::uint32_t>(bucket.size())});
        mixed_indices.insert(mixed_indices.end(), bucket.begin(), bucket.end());
    }

    std::vector<unsigned char> output(kV2HeaderSize, 0);
    std::copy(kMagic.begin(), kMagic.end(), output.begin());
    patch_u32(output, version_field, kBinaryLexiconVersion);
    patch_u32(output, header_size_field, static_cast<std::uint32_t>(kV2HeaderSize));
    patch_u32(output, entry_count_field, static_cast<std::uint32_t>(disk_entries.size()));
    patch_u32(output, syllable_count_field, static_cast<std::uint32_t>(syllable_records.size()));
    patch_u32(output, syllable_id_count_field, static_cast<std::uint32_t>(syllable_ids.size()));
    patch_u32(output, syllable_pool_size_field, static_cast<std::uint32_t>(syllable_pool.size()));
    patch_u32(output, text_pool_size_field, static_cast<std::uint32_t>(text_pool.size()));
    patch_u32(output, initial_index_count_field, static_cast<std::uint32_t>(initial_indices.size()));
    patch_u32(output, mixed_bucket_count_field, static_cast<std::uint32_t>(mixed_buckets.size()));
    patch_u32(output, mixed_index_count_field, static_cast<std::uint32_t>(mixed_indices.size()));
    patch_u32(output, maximum_reading_length_field,
              static_cast<std::uint32_t>(maximum_reading_length));
    const auto append_section = [&](const std::size_t field, const auto& values) {
        align_u32(output);
        patch_u32(output, field, static_cast<std::uint32_t>(output.size()));
        append_pod_vector(output, values);
    };
    append_section(entries_offset_field, disk_entries);
    append_section(syllable_ids_offset_field, syllable_ids);
    append_section(syllable_records_offset_field, syllable_records);
    align_u32(output);
    patch_u32(output, syllable_pool_offset_field, static_cast<std::uint32_t>(output.size()));
    output.insert(output.end(), syllable_pool.begin(), syllable_pool.end());
    align_u32(output);
    patch_u32(output, text_pool_offset_field, static_cast<std::uint32_t>(output.size()));
    output.insert(output.end(), text_pool.begin(), text_pool.end());
    append_section(initial_ranges_offset_field, initial_ranges);
    append_section(initial_indices_offset_field, initial_indices);
    append_section(mixed_buckets_offset_field, mixed_buckets);
    append_section(mixed_indices_offset_field, mixed_indices);
    if (output.size() > kMaximumFileBytes) return {false, "lexicon exceeds size limit"};
    patch_u32(output, file_size_field, static_cast<std::uint32_t>(output.size()));
    const auto payload_checksum = checksum(std::span(output).subspan(kV2HeaderSize));
    patch_u32(output, payload_checksum_low_field,
              static_cast<std::uint32_t>(payload_checksum));
    patch_u32(output, payload_checksum_high_field,
              static_cast<std::uint32_t>(payload_checksum >> 32U));
    std::ofstream stream(path, std::ios::binary | std::ios::trunc);
    if (!stream) return {false, "cannot open output"};
    stream.write(reinterpret_cast<const char*>(output.data()), static_cast<std::streamsize>(output.size()));
    if (!stream) return {false, "cannot write output"};
    return {true, {}};
}

BinaryLexicon::~BinaryLexicon() { reset_mapping(); }

void BinaryLexicon::reset_mapping() noexcept {
#ifdef _WIN32
    if (mapped_bytes_ != nullptr) UnmapViewOfFile(mapped_bytes_);
    if (mapped_mapping_handle_ != nullptr)
        CloseHandle(static_cast<HANDLE>(mapped_mapping_handle_));
    if (mapped_file_handle_ != INVALID_HANDLE_VALUE)
        CloseHandle(static_cast<HANDLE>(mapped_file_handle_));
#endif
    mapped_file_handle_ = reinterpret_cast<void*>(-1);
    mapped_mapping_handle_ = nullptr;
    mapped_bytes_ = nullptr;
    mapped_size_ = 0;
    mapped_entries_ = nullptr;
    mapped_entry_count_ = 0;
    mapped_syllable_ids_ = nullptr;
    mapped_syllable_id_count_ = 0;
    mapped_syllable_records_ = nullptr;
    mapped_syllable_count_ = 0;
    mapped_syllable_pool_ = nullptr;
    mapped_syllable_pool_size_ = 0;
    mapped_text_pool_ = nullptr;
    mapped_text_pool_size_ = 0;
    mapped_initial_ranges_ = nullptr;
    mapped_initial_indices_ = nullptr;
    mapped_initial_index_count_ = 0;
    mapped_mixed_buckets_ = nullptr;
    mapped_mixed_bucket_count_ = 0;
    mapped_mixed_indices_ = nullptr;
    mapped_mixed_index_count_ = 0;
}

LexiconIoResult BinaryLexicon::load(const std::filesystem::path& path) {
    reset_mapping();
    entries_.clear();
    syllable_ids_.clear();
    syllables_.clear();
    syllable_to_id_.clear();
    text_pool_.clear();
    for (auto& values : initial_entries_) values.clear();
    mixed_entries_.clear();
    maximum_reading_length_ = 0;

    std::array<unsigned char, 12> prefix{};
    {
        std::ifstream prefix_stream(path, std::ios::binary);
        if (!prefix_stream) return {false, "cannot open lexicon"};
        prefix_stream.read(reinterpret_cast<char*>(prefix.data()),
                           static_cast<std::streamsize>(prefix.size()));
        if (prefix_stream.gcount() != static_cast<std::streamsize>(prefix.size()) ||
            !std::equal(kMagic.begin(), kMagic.end(), prefix.begin()))
            return {false, "invalid lexicon header"};
    }
    std::size_t prefix_offset = 4;
    std::uint32_t detected_version{};
    if (!read_integer(std::span(prefix), prefix_offset, detected_version))
        return {false, "invalid lexicon header"};

    if (detected_version == kBinaryLexiconVersion) {
#ifdef _WIN32
        static_assert(sizeof(CompactEntry) == sizeof(DiskEntry));
        static_assert(sizeof(SyllableRecord) == sizeof(DiskSyllableRecord));
        static_assert(sizeof(IndexRange) == sizeof(DiskRange));
        static_assert(sizeof(MixedBucket) == sizeof(DiskMixedBucket));

        const HANDLE file = CreateFileW(path.c_str(), GENERIC_READ,
                                        FILE_SHARE_READ | FILE_SHARE_DELETE, nullptr,
                                        OPEN_EXISTING,
                                        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_RANDOM_ACCESS,
                                        nullptr);
        if (file == INVALID_HANDLE_VALUE) return {false, "cannot map lexicon file"};
        LARGE_INTEGER size{};
        if (!GetFileSizeEx(file, &size) || size.QuadPart < static_cast<LONGLONG>(kV2HeaderSize) ||
            size.QuadPart > static_cast<LONGLONG>(kMaximumFileBytes)) {
            CloseHandle(file);
            return {false, "invalid lexicon size"};
        }
        const HANDLE mapping = CreateFileMappingW(file, nullptr, PAGE_READONLY, 0, 0, nullptr);
        if (mapping == nullptr) {
            CloseHandle(file);
            return {false, "cannot create lexicon mapping"};
        }
        const auto* bytes = static_cast<const unsigned char*>(
            MapViewOfFile(mapping, FILE_MAP_READ, 0, 0, 0));
        if (bytes == nullptr) {
            CloseHandle(mapping);
            CloseHandle(file);
            return {false, "cannot map lexicon view"};
        }
        const auto fail = [&](const char* error) {
            UnmapViewOfFile(bytes);
            CloseHandle(mapping);
            CloseHandle(file);
            return LexiconIoResult{false, error};
        };
        const auto u32 = [&](const std::size_t field) {
            return static_cast<std::uint32_t>(bytes[field]) |
                   (static_cast<std::uint32_t>(bytes[field + 1]) << 8U) |
                   (static_cast<std::uint32_t>(bytes[field + 2]) << 16U) |
                   (static_cast<std::uint32_t>(bytes[field + 3]) << 24U);
        };
        if (!std::equal(kMagic.begin(), kMagic.end(), bytes) ||
            u32(version_field) != kBinaryLexiconVersion ||
            u32(header_size_field) != kV2HeaderSize ||
            u32(file_size_field) != static_cast<std::uint32_t>(size.QuadPart))
            return fail("invalid v2 lexicon header");
        const auto expected_payload_checksum =
            static_cast<std::uint64_t>(u32(payload_checksum_low_field)) |
            (static_cast<std::uint64_t>(u32(payload_checksum_high_field)) << 32U);
        if (!validation_cache_matches(path, file, static_cast<std::uint64_t>(size.QuadPart),
                                      kBinaryLexiconVersion) &&
            checksum(std::span(bytes, static_cast<std::size_t>(size.QuadPart))
                         .subspan(kV2HeaderSize)) != expected_payload_checksum)
            return fail("v2 lexicon checksum mismatch");
        const auto valid_section = [&](const std::uint32_t offset,
                                       const std::uint64_t count,
                                       const std::uint64_t element_size) {
            const auto section_size = count * element_size;
            return offset >= kV2HeaderSize && offset % alignof(std::uint32_t) == 0 &&
                   section_size <= static_cast<std::uint64_t>(size.QuadPart) &&
                   offset <= static_cast<std::uint64_t>(size.QuadPart) - section_size;
        };
        const auto entry_count = u32(entry_count_field);
        const auto syllable_count = u32(syllable_count_field);
        const auto syllable_id_count = u32(syllable_id_count_field);
        const auto syllable_pool_size = u32(syllable_pool_size_field);
        const auto text_pool_size = u32(text_pool_size_field);
        const auto initial_index_count = u32(initial_index_count_field);
        const auto mixed_bucket_count = u32(mixed_bucket_count_field);
        const auto mixed_index_count = u32(mixed_index_count_field);
        if (entry_count > kMaximumEntries || syllable_count == 0 ||
            syllable_count > std::numeric_limits<std::uint16_t>::max() ||
            !valid_section(u32(entries_offset_field), entry_count, sizeof(CompactEntry)) ||
            !valid_section(u32(syllable_ids_offset_field), syllable_id_count,
                           sizeof(std::uint16_t)) ||
            !valid_section(u32(syllable_records_offset_field), syllable_count,
                           sizeof(SyllableRecord)) ||
            !valid_section(u32(syllable_pool_offset_field), syllable_pool_size, 1) ||
            !valid_section(u32(text_pool_offset_field), text_pool_size, 1) ||
            !valid_section(u32(initial_ranges_offset_field), 26, sizeof(IndexRange)) ||
            !valid_section(u32(initial_indices_offset_field), initial_index_count,
                           sizeof(std::uint32_t)) ||
            !valid_section(u32(mixed_buckets_offset_field), mixed_bucket_count,
                           sizeof(MixedBucket)) ||
            !valid_section(u32(mixed_indices_offset_field), mixed_index_count,
                           sizeof(std::uint32_t)))
            return fail("invalid v2 lexicon section bounds");

        const auto* records = reinterpret_cast<const SyllableRecord*>(
            bytes + u32(syllable_records_offset_field));
        std::string_view previous;
        for (std::size_t index = 0; index < syllable_count; ++index) {
            if (records[index].size == 0 || records[index].size > syllable_pool_size ||
                records[index].offset > syllable_pool_size - records[index].size)
                return fail("invalid v2 syllable record");
            const std::string_view current(
                reinterpret_cast<const char*>(bytes + u32(syllable_pool_offset_field) +
                                              records[index].offset),
                records[index].size);
            if (index != 0 && !(previous < current))
                return fail("v2 syllable table is not sorted");
            previous = current;
        }
        const auto* initial_ranges = reinterpret_cast<const IndexRange*>(
            bytes + u32(initial_ranges_offset_field));
        for (std::size_t index = 0; index < 26; ++index)
            if (initial_ranges[index].offset > initial_index_count ||
                initial_ranges[index].count >
                    initial_index_count - initial_ranges[index].offset)
                return fail("invalid v2 initial index range");
        const auto* mixed_buckets = reinterpret_cast<const MixedBucket*>(
            bytes + u32(mixed_buckets_offset_field));
        std::uint32_t previous_key = 0;
        for (std::size_t index = 0; index < mixed_bucket_count; ++index) {
            if ((index != 0 && mixed_buckets[index].key <= previous_key) ||
                mixed_buckets[index].offset > mixed_index_count ||
                mixed_buckets[index].count > mixed_index_count - mixed_buckets[index].offset)
                return fail("invalid v2 mixed index range");
            previous_key = mixed_buckets[index].key;
        }

        mapped_file_handle_ = file;
        mapped_mapping_handle_ = mapping;
        mapped_bytes_ = bytes;
        mapped_size_ = static_cast<std::size_t>(size.QuadPart);
        mapped_entries_ = reinterpret_cast<const CompactEntry*>(bytes + u32(entries_offset_field));
        mapped_entry_count_ = entry_count;
        mapped_syllable_ids_ = reinterpret_cast<const std::uint16_t*>(
            bytes + u32(syllable_ids_offset_field));
        mapped_syllable_id_count_ = syllable_id_count;
        mapped_syllable_records_ = records;
        mapped_syllable_count_ = syllable_count;
        mapped_syllable_pool_ = reinterpret_cast<const char*>(
            bytes + u32(syllable_pool_offset_field));
        mapped_syllable_pool_size_ = syllable_pool_size;
        mapped_text_pool_ = reinterpret_cast<const char*>(bytes + u32(text_pool_offset_field));
        mapped_text_pool_size_ = text_pool_size;
        mapped_initial_ranges_ = initial_ranges;
        mapped_initial_indices_ = reinterpret_cast<const std::uint32_t*>(
            bytes + u32(initial_indices_offset_field));
        mapped_initial_index_count_ = initial_index_count;
        mapped_mixed_buckets_ = mixed_buckets;
        mapped_mixed_bucket_count_ = mixed_bucket_count;
        mapped_mixed_indices_ = reinterpret_cast<const std::uint32_t*>(
            bytes + u32(mixed_indices_offset_field));
        mapped_mixed_index_count_ = mixed_index_count;
        maximum_reading_length_ = u32(maximum_reading_length_field);
        return {true, {}};
#else
        return {false, "v2 memory-mapped lexicons require Windows"};
#endif
    }
    if (detected_version != 1) return {false, "unsupported lexicon version"};

    std::ifstream stream(path, std::ios::binary);
    if (!stream) return {false, "cannot open lexicon"};
    std::vector<unsigned char> bytes((std::istreambuf_iterator<char>(stream)), {});
    if (bytes.size() < kV1HeaderSize || bytes.size() > kMaximumFileBytes)
        return {false, "invalid lexicon size"};
    if (!std::equal(kMagic.begin(), kMagic.end(), bytes.begin())) return {false, "invalid magic"};
    std::size_t offset = kMagic.size();
    std::uint32_t version{};
    std::uint32_t count{};
    std::uint64_t expected_checksum{};
    if (!read_integer(std::span(bytes), offset, version) || version != 1)
        return {false, "unsupported lexicon version"};
    if (!read_integer(std::span(bytes), offset, count) || count > kMaximumEntries ||
        !read_integer(std::span(bytes), offset, expected_checksum)) return {false, "invalid header"};
    if (checksum(std::span(bytes).subspan(offset)) != expected_checksum) return {false, "checksum mismatch"};

    // The on-disk v1 format repeats syllable strings for compatibility.  Build
    // one canonical, lexicographically ordered syllable table first, then keep
    // only 16-bit IDs in the live lexicon.  This avoids millions of individual
    // std::string allocations while preserving the file format.
    const auto payload_offset = offset;
    std::set<std::string> unique_syllables;
    for (std::uint32_t index = 0; index < count; ++index) {
        std::uint32_t frequency{};
        std::uint16_t syllable_count{};
        std::uint16_t text_size{};
        if (!read_integer(std::span(bytes), offset, frequency) ||
            !read_integer(std::span(bytes), offset, syllable_count) || syllable_count == 0 ||
            !read_integer(std::span(bytes), offset, text_size) || text_size == 0)
            return {false, "truncated entry header"};
        for (std::uint16_t syllable_index = 0; syllable_index < syllable_count; ++syllable_index) {
            if (offset >= bytes.size()) return {false, "truncated syllable"};
            const auto size = bytes[offset++];
            if (size == 0 || offset + size > bytes.size()) return {false, "invalid syllable"};
            unique_syllables.emplace(reinterpret_cast<const char*>(bytes.data() + offset), size);
            offset += size;
        }
        if (offset + text_size > bytes.size()) return {false, "truncated text"};
        offset += text_size;
    }
    if (offset != bytes.size()) return {false, "trailing data"};
    if (unique_syllables.size() > std::numeric_limits<std::uint16_t>::max())
        return {false, "too many unique syllables"};

    std::vector<std::string> syllables(unique_syllables.begin(), unique_syllables.end());
    std::unordered_map<std::string, std::uint16_t> syllable_to_id;
    syllable_to_id.reserve(syllables.size());
    for (std::size_t index = 0; index < syllables.size(); ++index)
        syllable_to_id.emplace(syllables[index], static_cast<std::uint16_t>(index));

    std::vector<CompactEntry> parsed;
    std::vector<std::uint16_t> syllable_ids;
    std::string text_pool;
    parsed.reserve(count);
    // A conservative reserve prevents repeated growth without retaining the
    // complete input buffer after loading.
    syllable_ids.reserve(count * 2ULL);
    text_pool.reserve(bytes.size() / 3);
    offset = payload_offset;
    std::size_t maximum_reading_length = 0;
    for (std::uint32_t index = 0; index < count; ++index) {
        CompactEntry entry;
        if (!read_integer(std::span(bytes), offset, entry.frequency) ||
            !read_integer(std::span(bytes), offset, entry.syllable_count) ||
            !read_integer(std::span(bytes), offset, entry.text_size))
            return {false, "truncated entry header"};
        entry.syllable_offset = static_cast<std::uint32_t>(syllable_ids.size());
        entry.text_offset = static_cast<std::uint32_t>(text_pool.size());
        for (std::uint16_t syllable_index = 0; syllable_index < entry.syllable_count;
             ++syllable_index) {
            const auto size = bytes[offset++];
            const std::string key(reinterpret_cast<const char*>(bytes.data() + offset), size);
            const auto found = syllable_to_id.find(key);
            if (found == syllable_to_id.end()) return {false, "invalid syllable table"};
            syllable_ids.push_back(found->second);
            offset += size;
        }
        text_pool.append(reinterpret_cast<const char*>(bytes.data() + offset), entry.text_size);
        offset += entry.text_size;
        maximum_reading_length = std::max<std::size_t>(maximum_reading_length,
                                                       entry.syllable_count);
        parsed.push_back(entry);
    }

    const auto compact_less = [&](const CompactEntry& left, const CompactEntry& right) {
        const auto left_ids = std::span(syllable_ids).subspan(left.syllable_offset,
                                                              left.syllable_count);
        const auto right_ids = std::span(syllable_ids).subspan(right.syllable_offset,
                                                               right.syllable_count);
        if (!std::equal(left_ids.begin(), left_ids.end(), right_ids.begin(), right_ids.end()))
            return std::lexicographical_compare(left_ids.begin(), left_ids.end(),
                                                right_ids.begin(), right_ids.end());
        const auto left_text = std::string_view(text_pool).substr(left.text_offset, left.text_size);
        const auto right_text = std::string_view(text_pool).substr(right.text_offset, right.text_size);
        if (left_text != right_text) return left_text < right_text;
        return left.frequency > right.frequency;
    };
    if (!std::is_sorted(parsed.begin(), parsed.end(), compact_less))
        return {false, "entries are not sorted"};

    std::array<std::vector<std::uint32_t>, 26> initial_entries;
    std::unordered_map<std::uint32_t, std::vector<std::uint32_t>> mixed_entries;
    for (std::uint32_t index = 0; index < parsed.size(); ++index) {
        const auto& entry = parsed[index];
        if (entry.syllable_count != 1) continue;
        const auto& syllable = syllables[syllable_ids[entry.syllable_offset]];
        if (!syllable.empty() && syllable.front() >= 'a' && syllable.front() <= 'z')
            initial_entries[syllable.front() - 'a'].push_back(index);
    }
    for (std::uint32_t index = 0; index < parsed.size(); ++index) {
        const auto& entry = parsed[index];
        if (entry.syllable_count < 2) continue;
        const auto first_id = syllable_ids[entry.syllable_offset];
        const auto& second = syllables[syllable_ids[entry.syllable_offset + 1]];
        if (second.empty() || second.front() < 'a' || second.front() > 'z') continue;
        const auto key = (static_cast<std::uint32_t>(first_id) << 8U) |
                         static_cast<unsigned char>(second.front());
        mixed_entries[key].push_back(index);
    }
    const auto initial_less = [&](const std::uint32_t left_index,
                                  const std::uint32_t right_index) {
        const auto& left = parsed[left_index];
        const auto& right = parsed[right_index];
        if (left.frequency != right.frequency) return left.frequency > right.frequency;
        const auto& left_syllable = syllables[syllable_ids[left.syllable_offset]];
        const auto& right_syllable = syllables[syllable_ids[right.syllable_offset]];
        if (left_syllable.size() != right_syllable.size())
            return left_syllable.size() < right_syllable.size();
        return std::string_view(text_pool).substr(left.text_offset, left.text_size) <
               std::string_view(text_pool).substr(right.text_offset, right.text_size);
    };
    for (auto& initial : initial_entries)
        std::sort(initial.begin(), initial.end(), initial_less);

    entries_ = std::move(parsed);
    syllable_ids_ = std::move(syllable_ids);
    syllables_ = std::move(syllables);
    syllable_to_id_ = std::move(syllable_to_id);
    text_pool_ = std::move(text_pool);
    initial_entries_ = std::move(initial_entries);
    mixed_entries_ = std::move(mixed_entries);
    maximum_reading_length_ = maximum_reading_length;
    return {true, {}};
}

std::vector<LexiconEntry> BinaryLexicon::lookup(const std::span<const std::string_view> syllables) const {
    std::vector<std::uint16_t> reading;
    reading.reserve(syllables.size());
    for (const auto syllable : syllables) {
        if (mapped_entries_ != nullptr) {
            std::size_t first = 0;
            std::size_t last = mapped_syllable_count_;
            while (first < last) {
                const auto middle = first + (last - first) / 2;
                if (syllable_at(static_cast<std::uint16_t>(middle)) < syllable)
                    first = middle + 1;
                else
                    last = middle;
            }
            if (first == mapped_syllable_count_ ||
                syllable_at(static_cast<std::uint16_t>(first)) != syllable)
                return {};
            reading.push_back(static_cast<std::uint16_t>(first));
        } else {
            const auto found = syllable_to_id_.find(std::string(syllable));
            if (found == syllable_to_id_.end()) return {};
            reading.push_back(found->second);
        }
    }
    const auto entries = mapped_entries_ != nullptr
                             ? std::span<const CompactEntry>(mapped_entries_, mapped_entry_count_)
                             : std::span<const CompactEntry>(entries_);
    const auto ids = mapped_entries_ != nullptr
                         ? std::span<const std::uint16_t>(mapped_syllable_ids_,
                                                         mapped_syllable_id_count_)
                         : std::span<const std::uint16_t>(syllable_ids_);
    const auto compare = [&](const CompactEntry& entry,
                             const std::span<const std::uint16_t> right) {
        const auto left = ids.subspan(entry.syllable_offset, entry.syllable_count);
        const auto shared = std::min(left.size(), right.size());
        for (std::size_t index = 0; index < shared; ++index) {
            if (left[index] < right[index]) return -1;
            if (left[index] > right[index]) return 1;
        }
        if (left.size() < right.size()) return -1;
        if (left.size() > right.size()) return 1;
        return 0;
    };
    const auto first = std::lower_bound(
        entries.begin(), entries.end(), std::span<const std::uint16_t>(reading),
        [&](const CompactEntry& entry, const std::span<const std::uint16_t> value) {
            return compare(entry, value) < 0;
        });
    const auto last = std::upper_bound(
        first, entries.end(), std::span<const std::uint16_t>(reading),
        [&](const std::span<const std::uint16_t> value, const CompactEntry& entry) {
            return compare(entry, value) > 0;
        });
    std::vector<LexiconEntry> matches;
    matches.reserve(static_cast<std::size_t>(last - first));
    for (auto entry = first; entry != last; ++entry) matches.push_back(materialize(*entry));
    return matches;
}

std::vector<LexiconEntry> BinaryLexicon::lookup_initial(const char initial,
                                                        const std::size_t limit) const {
    if (initial < 'a' || initial > 'z' || limit == 0) return {};
    if (mapped_entries_ != nullptr) {
        const auto range = mapped_initial_ranges_[initial - 'a'];
        std::vector<LexiconEntry> matches;
        const auto count = std::min<std::size_t>(limit, range.count);
        matches.reserve(count);
        for (std::size_t index = 0; index < count; ++index) {
            const auto entry_index = mapped_initial_indices_[range.offset + index];
            if (entry_index >= mapped_entry_count_) return {};
            matches.push_back(materialize(mapped_entries_[entry_index]));
        }
        return matches;
    }
    const auto& indices = initial_entries_[initial - 'a'];
    std::vector<LexiconEntry> matches;
    const auto count = std::min(limit, indices.size());
    matches.reserve(count);
    for (std::size_t index = 0; index < count; ++index)
        matches.push_back(materialize(entries_[indices[index]]));
    return matches;
}

std::vector<AbbreviatedLexiconMatch> BinaryLexicon::lookup_mixed_abbreviation(
    const std::string_view input, const std::size_t limit) const {
    if (limit == 0 || input.size() < 2 || input.find('\'') != std::string_view::npos)
        return {};
    std::vector<AbbreviatedLexiconMatch> matches;
    const auto prune_threshold = limit > (std::numeric_limits<std::size_t>::max)() / 4
                                     ? (std::numeric_limits<std::size_t>::max)()
                                     : limit * 4;
    if (input.size() == 2) {
        const auto syllable_count = mapped_entries_ != nullptr
                                        ? mapped_syllable_count_
                                        : syllables_.size();
        for (std::size_t first_id = 0; first_id < syllable_count; ++first_id) {
            const auto first_syllable = syllable_at(static_cast<std::uint16_t>(first_id));
            if (first_syllable.empty() || first_syllable.front() != input[0]) continue;
            const auto key = (static_cast<std::uint32_t>(first_id) << 8U) |
                             static_cast<unsigned char>(input[1]);
            const std::uint32_t* bucket_begin = nullptr;
            const std::uint32_t* bucket_end = nullptr;
            if (mapped_entries_ != nullptr) {
                const auto* bucket = std::lower_bound(
                    mapped_mixed_buckets_,
                    mapped_mixed_buckets_ + mapped_mixed_bucket_count_, key,
                    [](const MixedBucket& value, const std::uint32_t expected) {
                        return value.key < expected;
                    });
                if (bucket == mapped_mixed_buckets_ + mapped_mixed_bucket_count_ ||
                    bucket->key != key)
                    continue;
                bucket_begin = mapped_mixed_indices_ + bucket->offset;
                bucket_end = bucket_begin + bucket->count;
            }
            const auto owned_bucket = mapped_entries_ == nullptr
                                          ? mixed_entries_.find(key)
                                          : mixed_entries_.end();
            if (mapped_entries_ == nullptr && owned_bucket == mixed_entries_.end())
                continue;
            const auto append_entry = [&](const std::uint32_t entry_index) {
                const auto& entry = mapped_entries_ != nullptr
                                        ? mapped_entries_[entry_index]
                                        : entries_[entry_index];
                if (entry.syllable_count != 2) return;
                matches.push_back({materialize(entry),
                                   {std::string(1, input[0]),
                                    std::string(1, input[1])}});
                if (matches.size() >= prune_threshold)
                    detail::retain_best_mixed_matches(matches, limit);
            };
            if (mapped_entries_ != nullptr) {
                for (auto index = bucket_begin; index != bucket_end; ++index) {
                    if (*index >= mapped_entry_count_) return {};
                    append_entry(*index);
                }
            } else {
                for (const auto entry_index : owned_bucket->second)
                    append_entry(entry_index);
            }
        }
        detail::retain_best_mixed_matches(matches, limit);
        return matches;
    }
    const auto maximum_first = std::min<std::size_t>(6, input.size() - 1);
    for (std::size_t first_size = 1; first_size <= maximum_first; ++first_size) {
        const auto first_syllable = input.substr(0, first_size);
        std::optional<std::uint16_t> first_id;
        if (mapped_entries_ != nullptr) {
            std::size_t first = 0;
            std::size_t last = mapped_syllable_count_;
            while (first < last) {
                const auto middle = first + (last - first) / 2;
                if (syllable_at(static_cast<std::uint16_t>(middle)) < first_syllable)
                    first = middle + 1;
                else
                    last = middle;
            }
            if (first != mapped_syllable_count_ &&
                syllable_at(static_cast<std::uint16_t>(first)) == first_syllable)
                first_id = static_cast<std::uint16_t>(first);
        } else {
            const auto found = syllable_to_id_.find(std::string(first_syllable));
            if (found != syllable_to_id_.end()) first_id = found->second;
        }
        if (!first_id) continue;
        const auto key = (static_cast<std::uint32_t>(*first_id) << 8U) |
                         static_cast<unsigned char>(input[first_size]);
        const std::uint32_t* bucket_begin = nullptr;
        const std::uint32_t* bucket_end = nullptr;
        if (mapped_entries_ != nullptr) {
            const auto* bucket = std::lower_bound(
                mapped_mixed_buckets_, mapped_mixed_buckets_ + mapped_mixed_bucket_count_, key,
                [](const MixedBucket& value, const std::uint32_t expected) {
                    return value.key < expected;
                });
            if (bucket == mapped_mixed_buckets_ + mapped_mixed_bucket_count_ ||
                bucket->key != key)
                continue;
            bucket_begin = mapped_mixed_indices_ + bucket->offset;
            bucket_end = bucket_begin + bucket->count;
        }
        const auto owned_bucket = mapped_entries_ == nullptr ? mixed_entries_.find(key)
                                                              : mixed_entries_.end();
        if (mapped_entries_ == nullptr && owned_bucket == mixed_entries_.end()) continue;
        const auto match_entry = [&](const std::uint32_t entry_index) {
            const auto& entry = mapped_entries_ != nullptr ? mapped_entries_[entry_index]
                                                            : entries_[entry_index];
            std::vector<std::string_view> reading;
            reading.reserve(entry.syllable_count);
            for (std::size_t index = 0; index < entry.syllable_count; ++index) {
                const auto id = mapped_entries_ != nullptr
                                    ? mapped_syllable_ids_[entry.syllable_offset + index]
                                    : syllable_ids_[entry.syllable_offset + index];
                reading.push_back(syllable_at(id));
            }
            const auto segments = detail::mixed_abbreviation_segments(
                std::span<const std::string_view>(reading), input);
            if (!segments) return;
            matches.push_back({materialize(entry), *segments});
            if (matches.size() >= prune_threshold) {
                detail::retain_best_mixed_matches(matches, limit);
            }
        };
        if (mapped_entries_ != nullptr) {
            for (auto index = bucket_begin; index != bucket_end; ++index) {
                if (*index >= mapped_entry_count_) return {};
                match_entry(*index);
            }
        } else {
            for (const auto entry_index : owned_bucket->second) match_entry(entry_index);
        }
    }
    detail::retain_best_mixed_matches(matches, limit);
    return matches;
}

LexiconEntry BinaryLexicon::materialize(const CompactEntry& entry) const {
    LexiconEntry result;
    result.frequency = entry.frequency;
    if (mapped_entries_ != nullptr)
        result.text.assign(mapped_text_pool_ + entry.text_offset, entry.text_size);
    else
        result.text.assign(text_pool_.data() + entry.text_offset, entry.text_size);
    result.syllables.reserve(entry.syllable_count);
    for (std::size_t index = 0; index < entry.syllable_count; ++index) {
        const auto id = mapped_entries_ != nullptr
                            ? mapped_syllable_ids_[entry.syllable_offset + index]
                            : syllable_ids_[entry.syllable_offset + index];
        result.syllables.emplace_back(syllable_at(id));
    }
    return result;
}

std::string_view BinaryLexicon::syllable_at(const std::uint16_t id) const {
    if (mapped_entries_ == nullptr) return syllables_[id];
    const auto& record = mapped_syllable_records_[id];
    return {mapped_syllable_pool_ + record.offset, record.size};
}

std::vector<LexiconEntry> BinaryLexicon::materialize_entries() const {
    std::vector<LexiconEntry> result;
    result.reserve(size());
    if (mapped_entries_ != nullptr) {
        for (std::size_t index = 0; index < mapped_entry_count_; ++index)
            result.push_back(materialize(mapped_entries_[index]));
    } else {
        for (const auto& entry : entries_) result.push_back(materialize(entry));
    }
    return result;
}

}  // namespace owo::engine

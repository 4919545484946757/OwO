#include "owo/plugin/package_archive.h"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#include <bcrypt.h>
#endif

#include <algorithm>
#include <array>
#include <cctype>
#include <fstream>
#include <limits>
#include <set>
#include <span>

namespace owo::plugin {
namespace {

constexpr std::uint64_t kMaximumPackageBytes = 64ULL * 1024ULL * 1024ULL;
constexpr std::uint64_t kMaximumFileBytes = 64ULL * 1024ULL * 1024ULL;
constexpr std::uint64_t kMaximumExpandedBytes = 256ULL * 1024ULL * 1024ULL;
constexpr std::uint16_t kMaximumEntries = 1024;
constexpr std::uint32_t kEocdSignature = 0x06054b50U;
constexpr std::uint32_t kCentralSignature = 0x02014b50U;
constexpr std::uint32_t kLocalSignature = 0x04034b50U;

std::uint16_t u16(const std::span<const unsigned char> bytes, const std::size_t offset) {
    return static_cast<std::uint16_t>(bytes[offset]) |
           (static_cast<std::uint16_t>(bytes[offset + 1]) << 8U);
}

std::uint32_t u32(const std::span<const unsigned char> bytes, const std::size_t offset) {
    return static_cast<std::uint32_t>(bytes[offset]) |
           (static_cast<std::uint32_t>(bytes[offset + 1]) << 8U) |
           (static_cast<std::uint32_t>(bytes[offset + 2]) << 16U) |
           (static_cast<std::uint32_t>(bytes[offset + 3]) << 24U);
}

bool valid_utf8(const std::string_view text) {
    std::size_t offset = 0;
    while (offset < text.size()) {
        const auto first = static_cast<unsigned char>(text[offset]);
        std::size_t count = 0;
        std::uint32_t scalar = 0;
        if (first <= 0x7fU) { count = 1; scalar = first; }
        else if (first >= 0xc2U && first <= 0xdfU) { count = 2; scalar = first & 0x1fU; }
        else if (first >= 0xe0U && first <= 0xefU) { count = 3; scalar = first & 0x0fU; }
        else if (first >= 0xf0U && first <= 0xf4U) { count = 4; scalar = first & 0x07U; }
        else return false;
        if (offset + count > text.size()) return false;
        for (std::size_t index = 1; index < count; ++index) {
            const auto next = static_cast<unsigned char>(text[offset + index]);
            if ((next & 0xc0U) != 0x80U) return false;
            scalar = (scalar << 6U) | (next & 0x3fU);
        }
        if ((count == 3 && scalar < 0x800U) || (count == 4 && scalar < 0x10000U) ||
            (scalar >= 0xd800U && scalar <= 0xdfffU) || scalar > 0x10ffffU) return false;
        offset += count;
    }
    return true;
}

std::string ascii_lower(std::string value) {
    std::transform(value.begin(), value.end(), value.begin(), [](const unsigned char byte) {
        return static_cast<char>(byte >= 'A' && byte <= 'Z' ? byte + ('a' - 'A') : byte);
    });
    return value;
}

bool reserved_windows_component(std::string component) {
    while (!component.empty() && (component.back() == '.' || component.back() == ' ')) component.pop_back();
    const auto dot = component.find('.');
    const auto stem = ascii_lower(component.substr(0, dot));
    if (stem == "con" || stem == "prn" || stem == "aux" || stem == "nul") return true;
    if (stem.size() == 4 && (stem.starts_with("com") || stem.starts_with("lpt")) &&
        stem[3] >= '1' && stem[3] <= '9') return true;
    return false;
}

bool safe_package_path(const std::string_view path) {
    if (path.empty() || path.size() > 512 || path.front() == '/' ||
        path.find('\\') != std::string_view::npos || path.find(':') != std::string_view::npos ||
        path.find('\0') != std::string_view::npos || !valid_utf8(path)) return false;
    std::size_t start = 0;
    while (start < path.size()) {
        const auto slash = path.find('/', start);
        const auto component = path.substr(start, slash == std::string_view::npos ? path.size() - start
                                                                                  : slash - start);
        if (component.empty() || component == "." || component == ".." ||
            component.back() == '.' || component.back() == ' ' ||
            reserved_windows_component(std::string(component))) return false;
        if (slash == std::string_view::npos) break;
        start = slash + 1;
        if (start == path.size()) return true;  // directory entry
    }
    return true;
}

PackageInspection failure(std::string diagnostic) {
    return {false, {}, 0, {}, {}, {}, std::move(diagnostic)};
}

std::string sha256(const std::span<const unsigned char> data) {
#ifdef _WIN32
    BCRYPT_ALG_HANDLE algorithm = nullptr;
    BCRYPT_HASH_HANDLE hash = nullptr;
    DWORD digest_size = 0;
    DWORD result_size = 0;
    if (data.size() > std::numeric_limits<ULONG>::max() ||
        BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) < 0 ||
        BCryptGetProperty(algorithm, BCRYPT_HASH_LENGTH, reinterpret_cast<PUCHAR>(&digest_size),
                          sizeof(digest_size), &result_size, 0) < 0 ||
        BCryptCreateHash(algorithm, &hash, nullptr, 0, nullptr, 0, 0) < 0) {
        if (hash != nullptr) BCryptDestroyHash(hash);
        if (algorithm != nullptr) BCryptCloseAlgorithmProvider(algorithm, 0);
        return {};
    }
    bool valid = BCryptHashData(hash, const_cast<PUCHAR>(data.data()),
                                static_cast<ULONG>(data.size()), 0) >= 0;
    std::vector<unsigned char> digest(digest_size);
    if (!valid || BCryptFinishHash(hash, digest.data(), digest_size, 0) < 0) valid = false;
    BCryptDestroyHash(hash);
    BCryptCloseAlgorithmProvider(algorithm, 0);
    if (!valid) return {};
    constexpr char hex[] = "0123456789abcdef";
    std::string output;
    output.reserve(digest.size() * 2U);
    for (const auto byte : digest) {
        output.push_back(hex[byte >> 4U]);
        output.push_back(hex[byte & 0x0fU]);
    }
    return output;
#else
    static_cast<void>(data);
    return {};
#endif
}

void append_u16(std::vector<unsigned char>& output, const std::uint16_t value) {
    output.push_back(static_cast<unsigned char>(value));
    output.push_back(static_cast<unsigned char>(value >> 8U));
}

void append_u32(std::vector<unsigned char>& output, const std::uint32_t value) {
    append_u16(output, static_cast<std::uint16_t>(value));
    append_u16(output, static_cast<std::uint16_t>(value >> 16U));
}

void append_u64(std::vector<unsigned char>& output, const std::uint64_t value) {
    append_u32(output, static_cast<std::uint32_t>(value));
    append_u32(output, static_cast<std::uint32_t>(value >> 32U));
}

std::uint32_t crc32(const std::span<const unsigned char> data) {
    std::uint32_t crc = 0xffffffffU;
    for (const auto byte : data) {
        crc ^= byte;
        for (unsigned bit = 0; bit < 8; ++bit)
            crc = (crc >> 1U) ^ (0xedb88320U & (0U - (crc & 1U)));
    }
    return ~crc;
}

bool finalize_inventory(PackageInspection& result) {
    std::vector<const PackageEntry*> canonical_entries;
    for (const auto& entry : result.entries) {
        if (entry.path != "signature.json") canonical_entries.push_back(&entry);
    }
    std::sort(canonical_entries.begin(), canonical_entries.end(), [](const auto* left,
                                                                      const auto* right) {
        return left->path < right->path;
    });
    std::vector<unsigned char> canonical;
    constexpr std::array<unsigned char, 22> domain{'O','w','O','P','a','c','k','a','g','e','I','n','v','e','n','t','o','r','y','V','1',0};
    canonical.insert(canonical.end(), domain.begin(), domain.end());
    append_u32(canonical, static_cast<std::uint32_t>(canonical_entries.size()));
    for (const auto* entry : canonical_entries) {
        append_u32(canonical, static_cast<std::uint32_t>(entry->path.size()));
        canonical.insert(canonical.end(), entry->path.begin(), entry->path.end());
        append_u16(canonical, entry->compression_method);
        append_u32(canonical, entry->crc32);
        append_u64(canonical, entry->compressed_size);
        append_u64(canonical, entry->uncompressed_size);
        canonical.insert(canonical.end(), entry->compressed_sha256.begin(),
                         entry->compressed_sha256.end());
    }
    result.inventory_sha256 = sha256(canonical);
    return !result.inventory_sha256.empty();
}

std::string generic_utf8(const std::filesystem::path& path) {
    const auto encoded = path.generic_u8string();
    return {reinterpret_cast<const char*>(encoded.data()), encoded.size()};
}

PackageInspection inspect_directory(const std::filesystem::path& source_path) {
    std::error_code error;
    const auto source = std::filesystem::absolute(source_path, error).lexically_normal();
    if (error || source.empty() || !std::filesystem::is_directory(source, error) || error)
        return failure("plugin folder does not exist or is not a directory");
#ifdef _WIN32
    const auto root_attributes = GetFileAttributesW(source.c_str());
    if (root_attributes == INVALID_FILE_ATTRIBUTES ||
        (root_attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
        return failure("plugin folder root must not be a reparse point");
#endif

    std::vector<std::pair<std::string, std::filesystem::path>> files;
    std::set<std::string> normalized_paths;
    std::filesystem::recursive_directory_iterator iterator(
        source, std::filesystem::directory_options::none, error);
    const std::filesystem::recursive_directory_iterator end;
    while (!error && iterator != end) {
        const auto path = iterator->path();
#ifdef _WIN32
        const auto attributes = GetFileAttributesW(path.c_str());
        if (attributes == INVALID_FILE_ATTRIBUTES)
            return failure("cannot inspect plugin folder entry");
        if ((attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0)
            return failure("plugin folders must not contain reparse points or symbolic links");
#endif
        if (iterator->is_directory(error)) {
            if (error) return failure("cannot inspect plugin folder directory");
        } else if (iterator->is_regular_file(error)) {
            if (error) return failure("cannot inspect plugin folder file");
            const auto relative = path.lexically_relative(source);
            const auto package_path = generic_utf8(relative);
            if (!safe_package_path(package_path))
                return failure("unsafe plugin folder path: " + package_path);
            const auto normalized = ascii_lower(package_path);
            if (!normalized_paths.insert(normalized).second)
                return failure("duplicate Windows-normalized plugin folder path: " + package_path);
            files.emplace_back(package_path, path);
            if (files.size() > kMaximumEntries)
                return failure("plugin folder contains too many files");
        } else {
            return failure("plugin folders may contain only regular files and directories");
        }
        iterator.increment(error);
    }
    if (error) return failure("cannot enumerate plugin folder exactly");
    if (files.empty()) return failure("plugin folder is empty");
    std::sort(files.begin(), files.end(), [](const auto& left, const auto& right) {
        return left.first < right.first;
    });

    auto snapshot = std::make_shared<std::vector<unsigned char>>();
    PackageInspection result{true, {}, 0, {}, {}, snapshot, {}};
    result.entries.reserve(files.size());
    bool has_manifest = false;
    for (const auto& [package_path, path] : files) {
        const auto file_size = std::filesystem::file_size(path, error);
        if (error || file_size > kMaximumFileBytes ||
            result.total_uncompressed_size > kMaximumExpandedBytes - file_size)
            return failure("plugin folder expanded size limit exceeded");
        if (file_size > std::numeric_limits<std::size_t>::max())
            return failure("plugin folder file is too large for this process");
        const auto data_offset = snapshot->size();
        snapshot->resize(data_offset + static_cast<std::size_t>(file_size));
        std::ifstream input(path, std::ios::binary);
        if (!input) return failure("cannot open plugin folder file: " + package_path);
        input.read(reinterpret_cast<char*>(snapshot->data() + data_offset),
                   static_cast<std::streamsize>(file_size));
        if (!input || input.peek() != std::char_traits<char>::eof())
            return failure("cannot snapshot plugin folder file exactly: " + package_path);
        const auto data = std::span<const unsigned char>(*snapshot).subspan(
            data_offset, static_cast<std::size_t>(file_size));
        const auto digest = sha256(data);
        if (digest.empty()) return failure("SHA-256 is unavailable");
        result.entries.push_back({package_path, 0, crc32(data), file_size, file_size,
                                  digest, data_offset});
        result.total_uncompressed_size += file_size;
        if (package_path == "manifest.json") has_manifest = true;
        if (package_path == "signature.json") {
            if (file_size == 0 || file_size > 32U * 1024U)
                return failure("signature.json must be non-empty and no larger than 32768 bytes");
            result.embedded_signature_json.assign(
                reinterpret_cast<const char*>(data.data()), data.size());
        }
    }
    if (!has_manifest) return failure("root manifest.json is required");
    if (!finalize_inventory(result)) return failure("cannot hash canonical plugin folder inventory");
    return result;
}

}  // namespace

PackageInspection inspect_package(const std::filesystem::path& package_path) {
    std::error_code source_error;
    if (std::filesystem::is_directory(package_path, source_error) && !source_error)
        return inspect_directory(package_path);
    std::error_code error;
    const auto file_size = std::filesystem::file_size(package_path, error);
    if (error || file_size < 22 || file_size > kMaximumPackageBytes)
        return failure("package size is outside [22, 67108864]");
    std::ifstream input(package_path, std::ios::binary);
    if (!input) return failure("cannot open package");
    auto snapshot = std::make_shared<std::vector<unsigned char>>(static_cast<std::size_t>(file_size));
    auto& bytes = *snapshot;
    input.read(reinterpret_cast<char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    if (!input || input.peek() != std::char_traits<char>::eof()) return failure("cannot read package exactly");
    const std::span<const unsigned char> view(bytes);

    const auto search_start = bytes.size() > 65557 ? bytes.size() - 65557 : 0;
    std::size_t eocd = std::numeric_limits<std::size_t>::max();
    for (std::size_t cursor = bytes.size() - 22;; --cursor) {
        if (u32(view, cursor) == kEocdSignature && cursor + 22U + u16(view, cursor + 20) == bytes.size()) {
            eocd = cursor;
            break;
        }
        if (cursor == search_start) break;
    }
    if (eocd == std::numeric_limits<std::size_t>::max()) return failure("valid end-of-central-directory record not found");
    if (u16(view, eocd + 20) != 0) return failure("package comments are not supported");
    if (u16(view, eocd + 4) != 0 || u16(view, eocd + 6) != 0) return failure("multi-disk packages are not supported");
    const auto entries_on_disk = u16(view, eocd + 8);
    const auto entry_count = u16(view, eocd + 10);
    if (entry_count == 0 || entry_count != entries_on_disk || entry_count > kMaximumEntries || entry_count == 0xffffU)
        return failure("invalid or excessive central-directory entry count");
    const auto central_size = u32(view, eocd + 12);
    const auto central_offset = u32(view, eocd + 16);
    if (central_size == 0xffffffffU || central_offset == 0xffffffffU ||
        static_cast<std::uint64_t>(central_offset) + central_size != eocd)
        return failure("ZIP64 or inconsistent central directory is not supported");

    PackageInspection result{true, {}, 0, {}, {}, snapshot, {}};
    result.entries.reserve(entry_count);
    std::set<std::string> normalized_paths;
    std::vector<std::pair<std::uint64_t, std::uint64_t>> local_ranges;
    bool has_manifest = false;
    std::size_t cursor = central_offset;
    for (std::uint16_t index = 0; index < entry_count; ++index) {
        if (cursor + 46 > eocd || u32(view, cursor) != kCentralSignature)
            return failure("invalid central-directory entry");
        const auto version_made_by = u16(view, cursor + 4);
        const auto flags = u16(view, cursor + 8);
        const auto method = u16(view, cursor + 10);
        const auto crc32 = u32(view, cursor + 16);
        const auto compressed_size = u32(view, cursor + 20);
        const auto uncompressed_size = u32(view, cursor + 24);
        const auto name_length = u16(view, cursor + 28);
        const auto extra_length = u16(view, cursor + 30);
        const auto comment_length = u16(view, cursor + 32);
        const auto disk = u16(view, cursor + 34);
        const auto external_attributes = u32(view, cursor + 38);
        const auto local_offset = u32(view, cursor + 42);
        const std::uint64_t record_end = static_cast<std::uint64_t>(cursor) + 46U + name_length + extra_length + comment_length;
        if (record_end > eocd || name_length == 0 || extra_length != 0 || comment_length != 0 || disk != 0 ||
            compressed_size == 0xffffffffU || uncompressed_size == 0xffffffffU || local_offset == 0xffffffffU)
            return failure("invalid or ZIP64 central-directory entry");
        if ((flags & 0x0001U) != 0) return failure("encrypted entries are not supported");
        if ((flags & ~0x0806U) != 0 || (flags & 0x0006U) == 0x0006U)
            return failure("unsupported ZIP general-purpose flags");
        if (method != 0 && method != 8) return failure("only stored and deflated entries are supported");
        if (method == 0 && compressed_size != uncompressed_size)
            return failure("stored entry sizes must match");
        if (uncompressed_size > kMaximumFileBytes || result.total_uncompressed_size > kMaximumExpandedBytes - uncompressed_size)
            return failure("expanded package size limit exceeded");
        if (uncompressed_size > 1024U * 1024U &&
            (compressed_size == 0 || uncompressed_size / compressed_size > 200U))
            return failure("suspicious compression ratio");
        const std::string path(reinterpret_cast<const char*>(bytes.data() + cursor + 46), name_length);
        if (!safe_package_path(path)) return failure("unsafe package path: " + path);
        if (path.ends_with('/') && (compressed_size != 0 || uncompressed_size != 0))
            return failure("directory entries must be empty");
        if ((flags & 0x0800U) == 0 &&
            std::any_of(path.begin(), path.end(), [](const unsigned char byte) { return byte >= 0x80U; }))
            return failure("non-ASCII paths must set the ZIP UTF-8 flag");
        const auto normalized = ascii_lower(path);
        if (!normalized_paths.insert(normalized).second) return failure("duplicate Windows-normalized package path: " + path);
        const auto creator = static_cast<unsigned char>(version_made_by >> 8U);
        const auto unix_mode = static_cast<std::uint16_t>(external_attributes >> 16U);
        if (creator == 3U && (unix_mode & 0170000U) == 0120000U) return failure("symbolic links are not supported");

        if (static_cast<std::uint64_t>(local_offset) + 30U > central_offset || u32(view, local_offset) != kLocalSignature)
            return failure("invalid local entry header");
        const auto local_flags = u16(view, local_offset + 6);
        const auto local_method = u16(view, local_offset + 8);
        const auto local_crc32 = u32(view, local_offset + 14);
        const auto local_compressed_size = u32(view, local_offset + 18);
        const auto local_uncompressed_size = u32(view, local_offset + 22);
        const auto local_name_length = u16(view, local_offset + 26);
        const auto local_extra_length = u16(view, local_offset + 28);
        const std::uint64_t data_start = static_cast<std::uint64_t>(local_offset) + 30U + local_name_length + local_extra_length;
        const std::uint64_t data_end = data_start + compressed_size;
        if (local_flags != flags || local_method != method || local_crc32 != crc32 ||
            local_extra_length != 0 ||
            local_compressed_size != compressed_size || local_uncompressed_size != uncompressed_size ||
            data_end > central_offset ||
            local_name_length != name_length ||
            !std::equal(path.begin(), path.end(), bytes.begin() + local_offset + 30))
            return failure("local header does not match central directory");
        for (const auto& [begin, end] : local_ranges) {
            if (static_cast<std::uint64_t>(local_offset) < end && data_end > begin)
                return failure("overlapping local entries are not supported");
        }
        local_ranges.emplace_back(local_offset, data_end);
        result.total_uncompressed_size += uncompressed_size;
        const auto payload_digest = sha256(view.subspan(static_cast<std::size_t>(data_start), compressed_size));
        if (payload_digest.empty()) return failure("SHA-256 is unavailable");
        result.entries.push_back({path, method, crc32, compressed_size, uncompressed_size,
                                  payload_digest, data_start});
        if (path == "manifest.json") has_manifest = true;
        if (path == "signature.json") {
            if (method != 0 || uncompressed_size == 0 || uncompressed_size > 32U * 1024U)
                return failure("signature.json must be a non-empty stored entry no larger than 32768 bytes");
            result.embedded_signature_json.assign(
                reinterpret_cast<const char*>(bytes.data() + static_cast<std::size_t>(data_start)),
                static_cast<std::size_t>(compressed_size));
        }
        cursor = static_cast<std::size_t>(record_end);
    }
    if (cursor != eocd) return failure("central-directory size does not match entries");
    if (!has_manifest) return failure("root manifest.json is required");
    if (!finalize_inventory(result)) return failure("cannot hash canonical package inventory");
    return result;
}

}  // namespace owo::plugin

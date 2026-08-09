#include "owo/plugin/package_extraction.h"

#ifdef OWO_HAS_ZLIB
#include <zlib.h>
#endif

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#endif

#include <algorithm>
#include <limits>
#include <span>
#include <utility>

namespace owo::plugin {
namespace {

std::uint32_t crc32(const std::span<const unsigned char> data) {
    std::uint32_t crc = 0xffffffffU;
    for (const auto byte : data) {
        crc ^= byte;
        for (unsigned bit = 0; bit < 8; ++bit)
            crc = (crc >> 1U) ^ (0xedb88320U & (0U - (crc & 1U)));
    }
    return ~crc;
}

#ifdef _WIN32
std::filesystem::path utf8_path(const std::string_view text) {
    if (text.empty() || text.size() > static_cast<std::size_t>(std::numeric_limits<int>::max())) return {};
    const auto size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.data(),
                                          static_cast<int>(text.size()), nullptr, 0);
    if (size <= 0) return {};
    std::wstring wide(static_cast<std::size_t>(size), L'\0');
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text.data(),
                            static_cast<int>(text.size()), wide.data(), size) != size) return {};
    return std::filesystem::path(wide);
}

bool safe_existing_directory(const std::filesystem::path& path) {
    const auto attributes = GetFileAttributesW(path.c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0 &&
           (attributes & FILE_ATTRIBUTE_REPARSE_POINT) == 0;
}

bool safe_local_parent(const std::filesystem::path& parent) {
    if (!parent.is_absolute() || parent.root_name().native().size() != 2 ||
        parent.root_name().native()[1] != L':') return false;
    auto current = parent.root_path();
    for (const auto& component : parent.relative_path()) {
        current /= component;
        if (!safe_existing_directory(current)) return false;
    }
    return true;
}

bool ensure_directory(const std::filesystem::path& path) {
    if (CreateDirectoryW(path.c_str(), nullptr) == FALSE && GetLastError() != ERROR_ALREADY_EXISTS)
        return false;
    return safe_existing_directory(path);
}

std::string error_text(const char* prefix) {
    return std::string(prefix) + " (Windows error " + std::to_string(GetLastError()) + ")";
}
#endif

}  // namespace

bool deflate_extraction_available() noexcept {
#ifdef OWO_HAS_ZLIB
    return true;
#else
    return false;
#endif
}

PackageEntryReadResult read_package_entry(
    const PackageInspection& package, const std::string_view entry_path,
    const std::size_t maximum_bytes) {
    if (!package.ok || package.snapshot_bytes == nullptr || entry_path.empty())
        return {false, {}, "a successful immutable package preflight is required"};
    const auto found = std::find_if(package.entries.begin(), package.entries.end(),
        [entry_path](const PackageEntry& entry) { return entry.path == entry_path; });
    if (found == package.entries.end() || found->path.ends_with('/'))
        return {false, {}, "package entry is missing"};
    if (found->uncompressed_size > maximum_bytes ||
        found->uncompressed_size > std::numeric_limits<std::size_t>::max() ||
        found->data_offset > package.snapshot_bytes->size() ||
        found->compressed_size > package.snapshot_bytes->size() - found->data_offset)
        return {false, {}, "package entry is oversized or outside the immutable snapshot"};
    const auto compressed = std::span<const unsigned char>(*package.snapshot_bytes).subspan(
        static_cast<std::size_t>(found->data_offset),
        static_cast<std::size_t>(found->compressed_size));
    std::string output;
    if (found->compression_method == 0) {
        output.assign(reinterpret_cast<const char*>(compressed.data()), compressed.size());
    } else if (found->compression_method == 8) {
#ifdef OWO_HAS_ZLIB
        if (found->compressed_size > std::numeric_limits<uInt>::max() ||
            found->uncompressed_size > std::numeric_limits<uInt>::max())
            return {false, {}, "Deflate entry exceeds zlib chunk limits"};
        output.resize(static_cast<std::size_t>(found->uncompressed_size));
        unsigned char empty_output{};
        z_stream stream{};
        stream.next_in = const_cast<Bytef*>(compressed.data());
        stream.avail_in = static_cast<uInt>(compressed.size());
        stream.next_out = output.empty() ? &empty_output
                                         : reinterpret_cast<Bytef*>(output.data());
        stream.avail_out = output.empty() ? 1U : static_cast<uInt>(output.size());
        if (inflateInit2(&stream, -MAX_WBITS) != Z_OK)
            return {false, {}, "cannot initialize raw Deflate decoder"};
        const auto result = inflate(&stream, Z_FINISH);
        const auto consumed = stream.total_in;
        const auto produced = stream.total_out;
        inflateEnd(&stream);
        if (result != Z_STREAM_END || consumed != compressed.size() ||
            produced != found->uncompressed_size)
            return {false, {}, "Deflate stream does not match declared package sizes"};
#else
        return {false, {}, "Deflate extraction is unavailable"};
#endif
    } else {
        return {false, {}, "unsupported extraction method"};
    }
    const auto data = std::span<const unsigned char>(
        reinterpret_cast<const unsigned char*>(output.data()), output.size());
    if (crc32(data) != found->crc32)
        return {false, {}, "package entry CRC-32 mismatch"};
    return {true, std::move(output), {}};
}

ExtractionResult extract_package_to_staging(
    const PackageInspection& package, const std::filesystem::path& staging_directory) {
    if (!package.ok || package.snapshot_bytes == nullptr)
        return {false, 0, 0, "a successful immutable package preflight is required"};
    std::vector<std::vector<unsigned char>> expanded(package.entries.size());
    for (std::size_t index = 0; index < package.entries.size(); ++index) {
        const auto& entry = package.entries[index];
        if (entry.data_offset > package.snapshot_bytes->size() ||
            entry.compressed_size > package.snapshot_bytes->size() - entry.data_offset)
            return {false, 0, 0, "package snapshot entry range is invalid"};
        if (!entry.path.ends_with('/')) {
            const auto compressed = std::span<const unsigned char>(*package.snapshot_bytes).subspan(
                static_cast<std::size_t>(entry.data_offset),
                static_cast<std::size_t>(entry.compressed_size));
            std::span<const unsigned char> output = compressed;
            if (entry.compression_method == 8) {
#ifdef OWO_HAS_ZLIB
                if (entry.compressed_size > std::numeric_limits<uInt>::max() ||
                    entry.uncompressed_size > std::numeric_limits<uInt>::max())
                    return {false, 0, 0, "Deflate entry exceeds zlib chunk limits"};
                auto& buffer = expanded[index];
                buffer.resize(static_cast<std::size_t>(entry.uncompressed_size));
                unsigned char empty_output{};
                z_stream stream{};
                stream.next_in = const_cast<Bytef*>(compressed.data());
                stream.avail_in = static_cast<uInt>(compressed.size());
                stream.next_out = buffer.empty() ? &empty_output : buffer.data();
                stream.avail_out = buffer.empty() ? 1U : static_cast<uInt>(buffer.size());
                if (inflateInit2(&stream, -MAX_WBITS) != Z_OK)
                    return {false, 0, 0, "cannot initialize raw Deflate decoder"};
                const auto inflate_result = inflate(&stream, Z_FINISH);
                const auto consumed = stream.total_in;
                const auto produced = stream.total_out;
                inflateEnd(&stream);
                if (inflate_result != Z_STREAM_END || consumed != compressed.size() ||
                    produced != entry.uncompressed_size)
                    return {false, 0, 0, "Deflate stream does not match declared package sizes: " + entry.path};
                output = buffer;
#else
                return {false, 0, 0, "Deflate extraction is unavailable; no staging files were created"};
#endif
            } else if (entry.compression_method != 0) {
                return {false, 0, 0, "unsupported extraction method"};
            }
            if (crc32(output) != entry.crc32)
                return {false, 0, 0, "entry CRC-32 mismatch: " + entry.path};
        }
    }
#ifdef _WIN32
    if (!staging_directory.is_absolute())
        return {false, 0, 0, "staging directory must be an absolute path"};
    std::error_code path_error;
    const auto absolute = std::filesystem::absolute(staging_directory, path_error).lexically_normal();
    if (path_error || absolute.empty() || absolute == absolute.root_path())
        return {false, 0, 0, "staging directory must be a local absolute child path"};
    const auto parent = absolute.parent_path();
    if (!safe_local_parent(parent))
        return {false, 0, 0, "staging parent is missing, non-local, or contains a reparse point"};
    if (GetFileAttributesW(absolute.c_str()) != INVALID_FILE_ATTRIBUTES)
        return {false, 0, 0, "staging directory must not already exist"};
    const auto missing_error = GetLastError();
    if (missing_error != ERROR_FILE_NOT_FOUND && missing_error != ERROR_PATH_NOT_FOUND)
        return {false, 0, 0, error_text("cannot inspect staging directory")};
    if (!ensure_directory(absolute)) return {false, 0, 0, error_text("cannot create safe staging directory")};

    ExtractionResult result{true, 0, 0, {}};
    for (std::size_t index = 0; index < package.entries.size(); ++index) {
        const auto& entry = package.entries[index];
        const auto relative = utf8_path(entry.path);
        if (relative.empty()) return {false, result.files_written, result.bytes_written,
                                     "cannot convert package path to Windows UTF-16"};
        auto output = absolute;
        const auto directory_relative = entry.path.ends_with('/') ? relative : relative.parent_path();
        for (const auto& component : directory_relative) {
            output /= component;
            if (!ensure_directory(output))
                return {false, result.files_written, result.bytes_written,
                        error_text("cannot create safe staging subdirectory")};
        }
        if (entry.path.ends_with('/')) continue;
        output = absolute / relative;
        const auto data = entry.compression_method == 8
            ? std::span<const unsigned char>(expanded[index])
            : std::span<const unsigned char>(*package.snapshot_bytes).subspan(
                  static_cast<std::size_t>(entry.data_offset),
                  static_cast<std::size_t>(entry.compressed_size));
        HANDLE file = CreateFileW(output.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW,
                                  FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH, nullptr);
        if (file == INVALID_HANDLE_VALUE)
            return {false, result.files_written, result.bytes_written,
                    error_text("cannot create staging file")};
        DWORD written = 0;
        FILE_ATTRIBUTE_TAG_INFO tag{};
        LARGE_INTEGER actual_size{};
        const bool write_ok = WriteFile(file, data.data(), static_cast<DWORD>(data.size()),
                                        &written, nullptr) != FALSE && written == data.size() &&
                              FlushFileBuffers(file) != FALSE &&
                              GetFileInformationByHandleEx(file, FileAttributeTagInfo,
                                                           &tag, sizeof(tag)) != FALSE &&
                              (tag.FileAttributes & (FILE_ATTRIBUTE_DIRECTORY |
                                                     FILE_ATTRIBUTE_REPARSE_POINT)) == 0 &&
                              GetFileSizeEx(file, &actual_size) != FALSE &&
                              actual_size.QuadPart == static_cast<LONGLONG>(data.size());
        const auto write_error = write_ok ? ERROR_SUCCESS : GetLastError();
        CloseHandle(file);
        if (!write_ok) {
            SetLastError(write_error);
            return {false, result.files_written, result.bytes_written,
                    error_text("cannot write staging file exactly")};
        }
        ++result.files_written;
        result.bytes_written += data.size();
    }
    return result;
#else
    static_cast<void>(staging_directory);
    return {false, 0, 0, "safe staging extraction is currently available on Windows only"};
#endif
}

}  // namespace owo::plugin

#pragma once

#include <cstdint>
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

namespace owo::plugin {

struct PackageEntry {
    std::string path;
    std::uint16_t compression_method{};
    std::uint32_t crc32{};
    std::uint64_t compressed_size{};
    std::uint64_t uncompressed_size{};
    std::string compressed_sha256;
    std::uint64_t data_offset{};
};

struct PackageInspection {
    bool ok{};
    std::vector<PackageEntry> entries;
    std::uint64_t total_uncompressed_size{};
    /// SHA-256 of the canonical inventory excluding signature.json.
    std::string inventory_sha256;
    /// Exact stored signature.json bytes captured from the same package snapshot.
    std::string embedded_signature_json;
    /// Immutable bytes validated by this inspection; used to avoid reopening the package.
    std::shared_ptr<const std::vector<unsigned char>> snapshot_bytes;
    std::string diagnostic;
};

/// Snapshots and preflights a bounded ZIP-compatible archive or a local directory without
/// executing its contents. Directory reparse points and symbolic links are rejected.
[[nodiscard]] PackageInspection inspect_package(const std::filesystem::path& package_path);

}  // namespace owo::plugin

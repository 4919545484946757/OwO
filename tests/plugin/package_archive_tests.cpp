#include "owo/plugin/package_archive.h"
#include "owo/plugin/package_extraction.h"
#include "owo/plugin/package_signature.h"

#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

namespace {

struct Entry {
    std::string path;
    std::string local_path;
    std::string data;
    std::uint16_t flags{0x0800U};
    std::uint16_t method{};
    std::uint32_t crc32{};
    std::uint32_t declared_uncompressed{};
    std::uint16_t version_made_by{20U};
    std::uint32_t external_attributes{};
};

void put16(std::vector<unsigned char>& out, const std::uint16_t value) {
    out.push_back(static_cast<unsigned char>(value));
    out.push_back(static_cast<unsigned char>(value >> 8U));
}

void put32(std::vector<unsigned char>& out, const std::uint32_t value) {
    put16(out, static_cast<std::uint16_t>(value));
    put16(out, static_cast<std::uint16_t>(value >> 16U));
}

void text(std::vector<unsigned char>& out, const std::string& value) {
    out.insert(out.end(), value.begin(), value.end());
}

std::vector<unsigned char> package(const std::vector<Entry>& entries) {
    std::vector<unsigned char> output;
    std::vector<std::uint32_t> offsets;
    for (const auto& entry : entries) {
        offsets.push_back(static_cast<std::uint32_t>(output.size()));
        const auto& local_name = entry.local_path.empty() ? entry.path : entry.local_path;
        put32(output, 0x04034b50U); put16(output, 20); put16(output, entry.flags);
        put16(output, entry.method); put16(output, 0); put16(output, 0); put32(output, entry.crc32);
        put32(output, static_cast<std::uint32_t>(entry.data.size()));
        put32(output, entry.declared_uncompressed == 0 ? static_cast<std::uint32_t>(entry.data.size())
                                                       : entry.declared_uncompressed);
        put16(output, static_cast<std::uint16_t>(local_name.size())); put16(output, 0);
        text(output, local_name); text(output, entry.data);
    }
    const auto central_offset = static_cast<std::uint32_t>(output.size());
    for (std::size_t index = 0; index < entries.size(); ++index) {
        const auto& entry = entries[index];
        put32(output, 0x02014b50U); put16(output, entry.version_made_by); put16(output, 20);
        put16(output, entry.flags); put16(output, entry.method); put16(output, 0); put16(output, 0);
        put32(output, entry.crc32); put32(output, static_cast<std::uint32_t>(entry.data.size()));
        put32(output, entry.declared_uncompressed == 0 ? static_cast<std::uint32_t>(entry.data.size())
                                                       : entry.declared_uncompressed);
        put16(output, static_cast<std::uint16_t>(entry.path.size())); put16(output, 0); put16(output, 0);
        put16(output, 0); put16(output, 0); put32(output, entry.external_attributes); put32(output, offsets[index]);
        text(output, entry.path);
    }
    const auto central_size = static_cast<std::uint32_t>(output.size()) - central_offset;
    put32(output, 0x06054b50U); put16(output, 0); put16(output, 0);
    put16(output, static_cast<std::uint16_t>(entries.size()));
    put16(output, static_cast<std::uint16_t>(entries.size()));
    put32(output, central_size); put32(output, central_offset); put16(output, 0);
    return output;
}

bool inspect(const std::filesystem::path& path, const std::vector<Entry>& entries) {
    const auto bytes = package(entries);
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    output.close();
    return owo::plugin::inspect_package(path).ok;
}

owo::plugin::PackageInspection inspection(const std::filesystem::path& path,
                                          const std::vector<Entry>& entries) {
    const auto bytes = package(entries);
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    output.close();
    return owo::plugin::inspect_package(path);
}

std::string signature_json(const std::string& digest) {
    return "{\"schema_version\":1,\"inventory_sha256\":\"" + digest +
           "\",\"format\":\"cms-detached-sha256\",\"signature_base64\":\"MAMCAQE=\"}";
}

std::uint32_t entry_crc32(const std::string& data) {
    std::uint32_t crc = 0xffffffffU;
    for (const auto character : data) {
        crc ^= static_cast<unsigned char>(character);
        for (unsigned bit = 0; bit < 8; ++bit)
            crc = (crc >> 1U) ^ (0xedb88320U & (0U - (crc & 1U)));
    }
    return ~crc;
}

}  // namespace

int main(const int argc, char** argv) {
    if (argc != 3) return 1;
    const std::filesystem::path path(argv[1]);
    const std::filesystem::path staging(argv[2]);
    const std::vector<Entry> valid{{"manifest.json", {}, "{}"}, {"bin/example.exe", {}, "MZ"}};
    const auto baseline = inspection(path, valid);
    if (!baseline.ok || baseline.inventory_sha256.size() != 64 ||
        baseline.entries.size() != 2 || baseline.entries[1].compressed_sha256.size() != 64) return 2;
    if (inspect(path, {{"../manifest.json", {}, "{}"}})) return 3;
    if (inspect(path, {{"manifest.json", {}, "{}"}, {"MANIFEST.JSON", {}, "{}"}})) return 4;
    if (inspect(path, {{"bin/example.exe", {}, "MZ"}})) return 5;
    auto encrypted = valid; encrypted[1].flags |= 1U;
    if (inspect(path, encrypted)) return 6;
    auto symlink = valid; symlink[1].version_made_by = static_cast<std::uint16_t>((3U << 8U) | 20U);
    symlink[1].external_attributes = static_cast<std::uint32_t>(0120000U) << 16U;
    if (inspect(path, symlink)) return 7;
    auto mismatch = valid; mismatch[1].local_path = "bin/other.exe";
    if (inspect(path, mismatch)) return 8;
    auto bomb = valid; bomb[1].method = 8; bomb[1].data = "x"; bomb[1].declared_uncompressed = 32U * 1024U * 1024U;
    if (inspect(path, bomb)) return 9;
    if (inspect(path, {{"manifest.json", {}, "{}"}, {"bin/CON.exe", {}, "x"}})) return 10;
    const std::vector<Entry> reordered(valid.rbegin(), valid.rend());
    if (inspection(path, reordered).inventory_sha256 != baseline.inventory_sha256) return 11;
    auto changed = valid; changed[1].data = "NZ";
    if (inspection(path, changed).inventory_sha256 == baseline.inventory_sha256) return 12;
    auto crc_changed = valid; crc_changed[1].crc32 = 1;
    if (inspection(path, crc_changed).inventory_sha256 == baseline.inventory_sha256) return 13;
    auto signed_one = valid; signed_one.push_back({"signature.json", {}, "first"});
    const auto signed_digest = inspection(path, signed_one).inventory_sha256;
    signed_one.back().data = "second";
    if (signed_digest != baseline.inventory_sha256 ||
        inspection(path, signed_one).inventory_sha256 != baseline.inventory_sha256) return 14;
    signed_one.back().data = signature_json(baseline.inventory_sha256);
    const auto signed_snapshot = inspection(path, signed_one);
    inspect(path, valid);
    const auto signed_metadata = owo::plugin::inspect_signed_package_metadata(signed_snapshot);
    if (!signed_metadata.ok || signed_metadata.inventory_sha256 != baseline.inventory_sha256 ||
        signed_metadata.signature.cms_der.size() != 5) return 15;
    signed_one[1].data = "changed";
    inspect(path, signed_one);
    if (owo::plugin::inspect_signed_package_metadata(path).ok) return 16;
    signed_one = valid;
    signed_one.push_back({"signature.json", {}, signature_json(baseline.inventory_sha256)});
    signed_one.back().method = 8;
    if (inspect(path, signed_one)) return 17;
    signed_one.back().method = 0;
    signed_one.back().data.assign(32769, 'A');
    if (inspect(path, signed_one)) return 18;
    inspect(path, valid);
    if (owo::plugin::inspect_signed_package_metadata(path).ok) return 19;
    std::vector<Entry> extractable = valid;
    for (auto& entry : extractable) entry.crc32 = entry_crc32(entry.data);
    const auto extractable_snapshot = inspection(path, extractable);
    std::error_code error;
    std::filesystem::remove_all(staging, error);
    if (error) return 20;
    const auto extracted = owo::plugin::extract_package_to_staging(
        extractable_snapshot, staging);
    if (!extracted.ok || extracted.files_written != 2 || extracted.bytes_written != 4 ||
        !std::filesystem::is_regular_file(staging / "manifest.json") ||
        !std::filesystem::is_regular_file(staging / "bin" / "example.exe")) return 21;
    std::ifstream extracted_exe(staging / "bin" / "example.exe", std::ios::binary);
    if (std::string(std::istreambuf_iterator<char>(extracted_exe),
                    std::istreambuf_iterator<char>()) != "MZ") return 27;
    extracted_exe.close();
    if (owo::plugin::extract_package_to_staging(extractable_snapshot, staging).ok) return 22;
    std::filesystem::remove_all(staging, error);
    if (error) return 23;
    auto deflated = extractable;
    deflated[1].method = 8;
    deflated[1].data = std::string("\xf3\x8d\x02\x00", 4);
    deflated[1].declared_uncompressed = 2;
    deflated[1].crc32 = 0x8fb09b5dU;
    const auto deflated_snapshot = inspection(path, deflated);
    const auto deflated_result = owo::plugin::extract_package_to_staging(
        deflated_snapshot, staging);
    if (owo::plugin::deflate_extraction_available()) {
        if (!deflated_result.ok || deflated_result.files_written != 2 ||
            deflated_result.bytes_written != 4) return 24;
        std::filesystem::remove_all(staging, error);
        if (error) return 28;
        auto trailing = deflated;
        trailing[1].data.push_back('\0');
        const auto trailing_snapshot = inspection(path, trailing);
        if (owo::plugin::extract_package_to_staging(trailing_snapshot, staging).ok ||
            std::filesystem::exists(staging)) return 29;
        auto truncated = deflated;
        truncated[1].data.pop_back();
        const auto truncated_snapshot = inspection(path, truncated);
        if (owo::plugin::extract_package_to_staging(truncated_snapshot, staging).ok ||
            std::filesystem::exists(staging)) return 30;
        auto wrong_size = deflated;
        wrong_size[1].declared_uncompressed = 3;
        const auto wrong_size_snapshot = inspection(path, wrong_size);
        if (owo::plugin::extract_package_to_staging(wrong_size_snapshot, staging).ok ||
            std::filesystem::exists(staging)) return 31;
        auto wrong_crc = deflated;
        wrong_crc[1].crc32 ^= 1U;
        const auto wrong_crc_snapshot = inspection(path, wrong_crc);
        if (owo::plugin::extract_package_to_staging(wrong_crc_snapshot, staging).ok ||
            std::filesystem::exists(staging)) return 32;
    } else if (deflated_result.ok || std::filesystem::exists(staging)) {
        return 24;
    }
    auto corrupt = extractable; corrupt.front().crc32 ^= 1U;
    const auto corrupt_snapshot = inspection(path, corrupt);
    if (owo::plugin::extract_package_to_staging(corrupt_snapshot, staging).ok ||
        std::filesystem::exists(staging)) return 25;
    std::filesystem::remove_all(staging, error);
    if (error) return 26;
    const auto folder = path.wstring() + L".folder";
    std::filesystem::remove_all(folder, error);
    std::filesystem::create_directories(std::filesystem::path(folder) / "bin", error);
    if (error) return 33;
    {
        std::ofstream manifest(std::filesystem::path(folder) / "manifest.json",
                               std::ios::binary);
        std::ofstream executable(std::filesystem::path(folder) / "bin" / "example.exe",
                                 std::ios::binary);
        manifest << "{}";
        executable << "MZ";
    }
    const auto folder_snapshot = owo::plugin::inspect_package(folder);
    if (!folder_snapshot.ok || folder_snapshot.entries.size() != 2 ||
        folder_snapshot.inventory_sha256.size() != 64) return 34;
    {
        std::ofstream executable(std::filesystem::path(folder) / "bin" / "example.exe",
                                 std::ios::binary | std::ios::trunc);
        executable << "NZ";
    }
    const auto folder_staging = staging.wstring() + L"-folder";
    std::filesystem::remove_all(folder_staging, error);
    const auto folder_extracted = owo::plugin::extract_package_to_staging(
        folder_snapshot, folder_staging);
    std::ifstream folder_executable(
        std::filesystem::path(folder_staging) / "bin" / "example.exe", std::ios::binary);
    if (!folder_extracted.ok || folder_extracted.files_written != 2 ||
        std::string(std::istreambuf_iterator<char>(folder_executable),
                    std::istreambuf_iterator<char>()) != "MZ") return 35;
    folder_executable.close();
    std::filesystem::remove(std::filesystem::path(folder) / "manifest.json", error);
    if (owo::plugin::inspect_package(folder).ok) return 36;
    std::filesystem::remove_all(folder, error);
    std::filesystem::remove_all(folder_staging, error);
    std::filesystem::remove(path, error);
    return 0;
}

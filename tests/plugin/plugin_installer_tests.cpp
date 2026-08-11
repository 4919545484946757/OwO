#include "owo/plugin/package_archive.h"
#include "owo/plugin/package_signature.h"
#include "owo/plugin/plugin_installer.h"
#include "owo/plugin/plugin_authorization_store.h"
#include "owo/plugin/plugin_store.h"

#include <cstdint>
#include <filesystem>
#include <fstream>
#include <string>
#include <vector>

namespace {

struct Entry {
    std::string path;
    std::string data;
};

void put16(std::vector<unsigned char>& output, const std::uint16_t value) {
    output.push_back(static_cast<unsigned char>(value));
    output.push_back(static_cast<unsigned char>(value >> 8U));
}

void put32(std::vector<unsigned char>& output, const std::uint32_t value) {
    put16(output, static_cast<std::uint16_t>(value));
    put16(output, static_cast<std::uint16_t>(value >> 16U));
}

std::uint32_t crc32(const std::string_view data) {
    std::uint32_t crc = 0xffffffffU;
    for (const unsigned char byte : data) {
        crc ^= byte;
        for (unsigned bit = 0; bit < 8; ++bit)
            crc = (crc >> 1U) ^ (0xedb88320U & (0U - (crc & 1U)));
    }
    return ~crc;
}

std::vector<unsigned char> package(const std::vector<Entry>& entries) {
    std::vector<unsigned char> output;
    std::vector<std::uint32_t> offsets;
    for (const auto& entry : entries) {
        offsets.push_back(static_cast<std::uint32_t>(output.size()));
        put32(output, 0x04034b50U); put16(output, 20); put16(output, 0x0800U);
        put16(output, 0); put16(output, 0); put16(output, 0); put32(output, crc32(entry.data));
        put32(output, static_cast<std::uint32_t>(entry.data.size()));
        put32(output, static_cast<std::uint32_t>(entry.data.size()));
        put16(output, static_cast<std::uint16_t>(entry.path.size())); put16(output, 0);
        output.insert(output.end(), entry.path.begin(), entry.path.end());
        output.insert(output.end(), entry.data.begin(), entry.data.end());
    }
    const auto central_offset = static_cast<std::uint32_t>(output.size());
    for (std::size_t index = 0; index < entries.size(); ++index) {
        const auto& entry = entries[index];
        put32(output, 0x02014b50U); put16(output, 20); put16(output, 20);
        put16(output, 0x0800U); put16(output, 0); put16(output, 0); put16(output, 0);
        put32(output, crc32(entry.data)); put32(output, static_cast<std::uint32_t>(entry.data.size()));
        put32(output, static_cast<std::uint32_t>(entry.data.size()));
        put16(output, static_cast<std::uint16_t>(entry.path.size()));
        put16(output, 0); put16(output, 0); put16(output, 0); put16(output, 0);
        put32(output, 0); put32(output, offsets[index]);
        output.insert(output.end(), entry.path.begin(), entry.path.end());
    }
    const auto central_size = static_cast<std::uint32_t>(output.size()) - central_offset;
    put32(output, 0x06054b50U); put16(output, 0); put16(output, 0);
    put16(output, static_cast<std::uint16_t>(entries.size()));
    put16(output, static_cast<std::uint16_t>(entries.size()));
    put32(output, central_size); put32(output, central_offset); put16(output, 0);
    return output;
}

bool write_package(const std::filesystem::path& path, const std::vector<Entry>& entries) {
    const auto bytes = package(entries);
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(reinterpret_cast<const char*>(bytes.data()),
                 static_cast<std::streamsize>(bytes.size()));
    return static_cast<bool>(output);
}

}  // namespace

int main(const int argc, char** argv) {
    if (argc != 3) return 1;
    const std::filesystem::path package_path(argv[1]);
    const std::filesystem::path store_root(argv[2]);
    std::error_code error;
    std::filesystem::remove_all(store_root, error);
    std::filesystem::remove(package_path, error);

    const auto missing = owo::plugin::install_plugin_package(package_path, store_root);
    if (missing.ok || missing.stage != owo::plugin::PluginInstallStage::package_inspection ||
        std::filesystem::exists(store_root)) return 2;

    const std::string manifest =
        "{\"id\":\"owo.plugin.transaction\",\"name\":\"Transaction\","
        "\"version\":\"1.0.0\",\"api_version\":1,\"runtime\":\"process\","
        "\"entry\":\"bin/plugin.exe\",\"permissions\":[\"system.full_trust\","
        "\"ui.desktop_pet\"],\"network\":false,"
        "\"config_schema\":\"config.schema.json\"}";
    std::vector<Entry> entries{{"manifest.json", manifest}, {"bin/plugin.exe", "MZ"},
                               {"config.schema.json", "{}"}};
    if (!write_package(package_path, entries)) return 3;
    const auto unsigned_snapshot = owo::plugin::inspect_package(package_path);
    if (!unsigned_snapshot.ok || unsigned_snapshot.inventory_sha256.size() != 64) return 4;
    entries.push_back({"signature.json",
        "{\"schema_version\":1,\"inventory_sha256\":\"" +
        unsigned_snapshot.inventory_sha256 +
        "\",\"format\":\"cms-detached-sha256\",\"signature_base64\":\"MAMCAQE=\"}"});
    if (!write_package(package_path, entries)) return 5;

    const auto captured = owo::plugin::inspect_package(package_path);
    if (!captured.ok) return 6;
    if (!write_package(package_path, {{"manifest.json", "{}"}})) return 7;
    if (!owo::plugin::inspect_signed_package_metadata(captured).ok) return 8;

    if (!write_package(package_path, entries)) return 9;
    const auto untrusted = owo::plugin::install_plugin_package(package_path, store_root);
    if (untrusted.ok || untrusted.stage != owo::plugin::PluginInstallStage::publisher_trust ||
        untrusted.version_published || untrusted.activated ||
        std::filesystem::exists(store_root)) return 10;

    const auto preview = owo::plugin::inspect_plugin_install(package_path);
    if (!preview.ok || preview.trust_tier != owo::plugin::PluginTrustTier::unverified_package ||
        preview.risk_level != owo::plugin::PluginInstallRiskLevel::critical ||
        !preview.requires_risk_consent || !preview.requires_full_trust) return 12;
    owo::plugin::PluginInstallConsent consent;
    consent.inventory_sha256.assign(64, 'f');
    consent.disclaimer_version = owo::plugin::kPluginRiskDisclaimerVersion;
    consent.accept_untrusted_publisher = true;
    consent.accept_full_trust = true;
    consent.granted_permissions = preview.manifest.permissions;
    const auto mismatched = owo::plugin::install_plugin_package(
        package_path, store_root, consent);
    if (mismatched.ok || mismatched.stage != owo::plugin::PluginInstallStage::risk_consent ||
        std::filesystem::exists(store_root)) return 13;
    consent.inventory_sha256 = preview.inventory_sha256;
    const auto installed = owo::plugin::install_plugin_package(
        package_path, store_root, consent);
    if (!installed.ok || installed.stage != owo::plugin::PluginInstallStage::completed ||
        !installed.version_published || installed.activated ||
        !installed.permissions_authorized) return 14;
    const auto installed_binding = owo::plugin::query_installed_plugin_version(
        store_root, preview.manifest.id, preview.manifest.version);
    if (!installed_binding.ok || installed_binding.trust_tier !=
            owo::plugin::PluginTrustTier::unverified_package) return 16;
    const auto authorization = owo::plugin::load_plugin_authorization(
        store_root, preview.manifest.id, preview.manifest.version);
    if (!authorization.ok || !authorization.value.context.informed_consent ||
        authorization.value.context.trust_tier !=
            owo::plugin::PluginTrustTier::unverified_package ||
        !owo::plugin::is_plugin_permission_granted(
            authorization.value, preview.manifest, preview.inventory_sha256,
            std::string(64, '0'), "system.full_trust")) return 15;

    const auto folder_path = package_path.wstring() + L".folder";
    const auto folder_store = store_root.wstring() + L"-folder";
    std::filesystem::remove_all(folder_path, error);
    std::filesystem::remove_all(folder_store, error);
    std::filesystem::create_directories(
        std::filesystem::path(folder_path) / "bin", error);
    if (error) return 17;
    auto folder_manifest = manifest;
    const auto version_position = folder_manifest.find("\"version\":\"1.0.0\"");
    folder_manifest.replace(version_position, std::string("\"version\":\"1.0.0\"").size(),
                            "\"version\":\"2.0.0\"");
    {
        std::ofstream manifest_file(std::filesystem::path(folder_path) / "manifest.json",
                                    std::ios::binary);
        std::ofstream executable(std::filesystem::path(folder_path) / "bin" / "plugin.exe",
                                 std::ios::binary);
        std::ofstream schema(std::filesystem::path(folder_path) / "config.schema.json",
                             std::ios::binary);
        manifest_file << folder_manifest;
        executable << "MZ";
        schema << "{}";
    }
    const auto folder_preview = owo::plugin::inspect_plugin_install(folder_path);
    if (!folder_preview.ok || folder_preview.manifest.version != "2.0.0" ||
        !folder_preview.requires_risk_consent) return 18;
    consent.inventory_sha256 = folder_preview.inventory_sha256;
    consent.granted_permissions = folder_preview.manifest.permissions;
    const auto folder_installed = owo::plugin::install_plugin_package(
        folder_path, folder_store, consent);
    if (!folder_installed.ok || folder_installed.activated ||
        !std::filesystem::is_regular_file(folder_installed.installed_path /
                                          "bin" / "plugin.exe")) return 19;
    std::filesystem::remove_all(folder_path, error);
    std::filesystem::remove_all(folder_store, error);

    std::filesystem::remove(package_path, error);
    if (error) return 11;
    return 0;
}

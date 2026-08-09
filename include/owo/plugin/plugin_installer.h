#pragma once

#include "owo/plugin/plugin_authorization.h"
#include "owo/plugin/plugin_manifest.h"
#include "owo/plugin/plugin_permissions.h"

#include <filesystem>
#include <cstdint>
#include <string>
#include <vector>

namespace owo::plugin {

enum class PluginInstallStage {
    none,
    package_inspection,
    publisher_trust,
    risk_consent,
    store_initialization,
    staging_extraction,
    version_publication,
    permission_authorization,
    completed,
};

enum class PluginInstallRiskLevel {
    low,
    elevated,
    high,
    critical,
};

struct PluginInstallPreview {
    bool ok{};
    PluginManifest manifest;
    PluginTrustTier trust_tier{PluginTrustTier::unverified_package};
    PluginInstallRiskLevel risk_level{PluginInstallRiskLevel::critical};
    bool requires_risk_consent{};
    bool requires_full_trust{};
    std::string inventory_sha256;
    std::string publisher_display_name;
    std::string publisher_certificate_sha256;
    std::string trust_diagnostic;
    std::string diagnostic;
};

struct PluginInstallConsent {
    std::string inventory_sha256;
    std::uint32_t disclaimer_version{};
    bool accept_untrusted_publisher{};
    bool accept_full_trust{};
    std::vector<std::string> granted_permissions;
};

struct PluginInstallResult {
    bool ok{};
    PluginInstallStage stage{PluginInstallStage::none};
    bool version_published{};
    bool activated{};
    PluginManifest manifest;
    std::filesystem::path installed_path;
    std::filesystem::path retained_staging_path;
    std::string previous_version;
    std::string inventory_sha256;
    std::string publisher_display_name;
    std::string publisher_certificate_sha256;
    PluginTrustTier trust_tier{PluginTrustTier::unverified_package};
    PluginInstallRiskLevel risk_level{PluginInstallRiskLevel::critical};
    bool permissions_authorized{};
    std::string diagnostic;
};

/// Inspects package identity, manifest, requested permissions and publisher status without writes.
[[nodiscard]] PluginInstallPreview inspect_plugin_install(
    const std::filesystem::path& package_path);

/// Installs a package through one immutable snapshot, Windows publisher trust, safe staging,
/// and versioned atomic publication. Untrusted packages never create store state.
[[nodiscard]] PluginInstallResult install_plugin_package(
    const std::filesystem::path& package_path, const std::filesystem::path& plugin_store_root);

/// Installs one exact previewed high-risk package inactive after informed per-version consent.
[[nodiscard]] PluginInstallResult install_plugin_package(
    const std::filesystem::path& package_path, const std::filesystem::path& plugin_store_root,
    const PluginInstallConsent& consent);

}  // namespace owo::plugin

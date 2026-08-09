#pragma once

#include "owo/plugin/plugin_authorization.h"
#include "owo/plugin/plugin_manifest.h"

#include <filesystem>
#include <string>
#include <vector>

namespace owo::plugin {

struct PluginStoreResult {
    bool ok{};
    PluginManifest manifest;
    std::filesystem::path installed_path;
    std::string previous_version;
    std::string diagnostic;
    bool version_published{};
    bool activated{};
};

struct InstalledPluginVersionResult {
    bool ok{};
    PluginManifest manifest;
    std::filesystem::path installed_path;
    std::string inventory_sha256;
    std::string publisher_certificate_sha256;
    std::string diagnostic;
    PluginTrustTier trust_tier{PluginTrustTier::trusted_publisher};
};

enum class PluginRecoveryKind {
    retained_staging,
    retained_uninstall,
    orphaned_version,
    orphaned_record,
    orphaned_authorization,
    inactive_version,
    invalid_active_record,
    unsafe_store_entry,
};

struct PluginRecoveryItem {
    PluginRecoveryKind kind{};
    std::filesystem::path path;
    std::string plugin_id;
    std::string version;
    std::string diagnostic;
};

struct PluginRecoveryScanResult {
    bool ok{};
    std::vector<PluginRecoveryItem> items;
    std::string diagnostic;
};

struct InstalledPluginState {
    PluginManifest manifest;
    std::filesystem::path installed_path;
    bool active{};
};

struct PluginStateListResult {
    bool ok{};
    std::vector<InstalledPluginState> versions;
    std::string diagnostic;
};

struct PluginManagementResult {
    bool ok{};
    std::string plugin_id;
    std::string version;
    std::filesystem::path affected_path;
    std::string diagnostic;
};

struct PluginUninstallResult {
    bool ok{};
    std::string plugin_id;
    std::string version;
    std::filesystem::path retained_uninstall_path;
    bool version_removed{};
    bool authorization_removed{};
    bool last_version{};
    bool sandbox_profile_removed{};
    bool data_preserved{true};
    std::string diagnostic;
};

/// Creates or validates the versioned plugin-store layout. The data directory is never replaced.
[[nodiscard]] PluginStoreResult initialize_plugin_store(const std::filesystem::path& root);

/// Publishes one prevalidated direct child of root/staging, then atomically activates it.
[[nodiscard]] PluginStoreResult publish_staged_plugin(
    const std::filesystem::path& root, const std::filesystem::path& staging_directory,
    std::string_view inventory_sha256, std::string_view publisher_certificate_sha256,
    bool activate = true,
    PluginTrustTier trust_tier = PluginTrustTier::trusted_publisher);

/// Atomically switches the active record to an already installed version.
[[nodiscard]] PluginStoreResult activate_installed_plugin_version(
    const std::filesystem::path& root, std::string_view plugin_id, std::string_view version);

/// Reads and cross-checks an installed manifest and immutable installation binding.
/// This is read-only and does not require the version to be active.
[[nodiscard]] InstalledPluginVersionResult query_installed_plugin_version(
    const std::filesystem::path& root, std::string_view plugin_id, std::string_view version);

/// Resolves the active record and cross-checks it against the installed version binding.
[[nodiscard]] InstalledPluginVersionResult query_active_plugin_version(
    const std::filesystem::path& root, std::string_view plugin_id);

/// Audits recoverable startup state without deleting, activating, or otherwise mutating it.
/// A missing store root is a valid empty state; an existing unsafe or incomplete layout fails.
[[nodiscard]] PluginRecoveryScanResult scan_plugin_store_recovery(
    const std::filesystem::path& root);

/// Lists every valid installed version and marks the single active binding per plugin.
/// Invalid entries remain visible through scan_plugin_store_recovery instead of being hidden.
[[nodiscard]] PluginStateListResult list_installed_plugins(
    const std::filesystem::path& root);

/// Removes only the exact expected active record. Installed versions, authorizations and data
/// are retained. A concurrently changed active version is never disabled accidentally.
[[nodiscard]] PluginManagementResult deactivate_plugin(
    const std::filesystem::path& root, std::string_view plugin_id,
    std::string_view expected_version);

/// Re-scans and exactly matches a previously reported recovery item before deleting it.
/// Inactive versions and unsafe entries are deliberately never deleted by this operation.
[[nodiscard]] PluginManagementResult cleanup_plugin_recovery_item(
    const std::filesystem::path& root, const PluginRecoveryItem& item);

/// Uninstalls one exact inactive version and its exact authorization record. Plugin data is
/// always retained. Removing the last version also removes the deterministic sandbox profile.
[[nodiscard]] PluginUninstallResult uninstall_plugin_version(
    const std::filesystem::path& root, std::string_view plugin_id,
    std::string_view version);

}  // namespace owo::plugin

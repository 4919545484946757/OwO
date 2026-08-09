#pragma once

#include "owo/plugin/plugin_manifest.h"

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>

namespace owo::plugin {

inline constexpr std::uint32_t kPluginAuthorizationSchemaVersion = 2;
inline constexpr std::uint32_t kPluginRiskDisclaimerVersion = 1;

enum class PluginTrustTier {
    trusted_publisher,
    third_party_signed,
    unverified_package,
};

struct PluginAuthorizationContext {
    PluginTrustTier trust_tier{PluginTrustTier::trusted_publisher};
    std::uint32_t disclaimer_version{};
    bool informed_consent{};
};

struct PluginAuthorization {
    std::uint32_t schema_version{kPluginAuthorizationSchemaVersion};
    std::string plugin_id;
    std::string version;
    std::string inventory_sha256;
    std::string publisher_certificate_sha256;
    std::vector<std::string> granted_permissions;
    PluginAuthorizationContext context;
};

struct PluginAuthorizationResult {
    bool ok{};
    PluginAuthorization value;
    std::string diagnostic;
};

/// Creates a version- and publisher-bound grant. Every grant must be declared by the manifest.
[[nodiscard]] PluginAuthorizationResult make_plugin_authorization(
    const PluginManifest& manifest, std::string_view inventory_sha256,
    std::string_view publisher_certificate_sha256,
    std::vector<std::string> granted_permissions);

/// Creates an authorization carrying a package-bound informed-consent receipt.
[[nodiscard]] PluginAuthorizationResult make_plugin_authorization(
    const PluginManifest& manifest, std::string_view inventory_sha256,
    std::string_view publisher_certificate_sha256,
    std::vector<std::string> granted_permissions,
    PluginAuthorizationContext context);

/// Stable ASCII value used by records and the settings-center JSON protocol.
[[nodiscard]] std::string_view plugin_trust_tier_name(PluginTrustTier tier) noexcept;

/// Serializes a strict canonical authorization record; invalid records return empty.
[[nodiscard]] std::string serialize_plugin_authorization(const PluginAuthorization& authorization);

/// Parses an exact canonical authorization record. Unknown, reordered, or duplicate fields fail.
[[nodiscard]] PluginAuthorizationResult parse_plugin_authorization(std::string_view record);

/// Returns true only when identity, version, inventory, publisher, and permission all match.
[[nodiscard]] bool is_plugin_permission_granted(
    const PluginAuthorization& authorization, const PluginManifest& installed_manifest,
    std::string_view inventory_sha256,
    std::string_view publisher_certificate_sha256, std::string_view permission);

}  // namespace owo::plugin

#include "owo/plugin/plugin_authorization.h"
#include "owo/plugin/plugin_permissions.h"

#include <algorithm>
#include <utility>

namespace owo::plugin {
namespace {

bool sha256_text(const std::string_view value) {
    return value.size() == 64 && std::all_of(value.begin(), value.end(), [](const unsigned char byte) {
        return (byte >= '0' && byte <= '9') || (byte >= 'a' && byte <= 'f');
    });
}

bool plugin_id_text(const std::string_view value) {
    if (value.size() < 3 || value.size() > 128 || value.front() == '.' ||
        value.back() == '.' || value.find('.') == std::string_view::npos) return false;
    bool previous_dot = false;
    for (const unsigned char byte : value) {
        const bool valid = (byte >= 'a' && byte <= 'z') || (byte >= '0' && byte <= '9') ||
                           byte == '-' || byte == '.';
        if (!valid || (byte == '.' && previous_dot)) return false;
        previous_dot = byte == '.';
    }
    return true;
}

bool version_text(const std::string_view value) {
    unsigned parts = 0;
    std::size_t start = 0;
    while (start < value.size()) {
        const auto end = value.find('.', start);
        const auto token = value.substr(start, end == std::string_view::npos
            ? value.size() - start : end - start);
        if (token.empty() || (token.size() > 1 && token.front() == '0') ||
            !std::all_of(token.begin(), token.end(), [](const unsigned char byte) {
                return byte >= '0' && byte <= '9';
            })) return false;
        ++parts;
        if (end == std::string_view::npos) break;
        start = end + 1;
    }
    return parts == 3;
}

bool valid_authorization(const PluginAuthorization& authorization) {
    const bool legacy = authorization.schema_version == 1;
    const bool current = authorization.schema_version == kPluginAuthorizationSchemaVersion;
    const bool consent_required = authorization.context.trust_tier !=
                                      PluginTrustTier::trusted_publisher ||
        std::find(authorization.granted_permissions.begin(),
                  authorization.granted_permissions.end(), "system.full_trust") !=
            authorization.granted_permissions.end();
    const bool informed_context = authorization.context.informed_consent &&
        authorization.context.disclaimer_version == kPluginRiskDisclaimerVersion;
    const bool empty_trusted_context = !authorization.context.informed_consent &&
        authorization.context.disclaimer_version == 0 &&
        authorization.context.trust_tier == PluginTrustTier::trusted_publisher;
    const bool context_valid = legacy
        ? authorization.context.trust_tier == PluginTrustTier::trusted_publisher &&
              authorization.context.disclaimer_version == 0 &&
              !authorization.context.informed_consent
        : current && (consent_required ? informed_context
                                       : (empty_trusted_context || informed_context));
    return (legacy || current) && context_valid &&
           plugin_id_text(authorization.plugin_id) && version_text(authorization.version) &&
           sha256_text(authorization.inventory_sha256) &&
           sha256_text(authorization.publisher_certificate_sha256) &&
           std::is_sorted(authorization.granted_permissions.begin(),
                          authorization.granted_permissions.end()) &&
           std::adjacent_find(authorization.granted_permissions.begin(),
                              authorization.granted_permissions.end()) ==
           authorization.granted_permissions.end() &&
           std::all_of(authorization.granted_permissions.begin(),
                       authorization.granted_permissions.end(), is_known_plugin_permission);
}

bool parse_trust_tier(const std::string_view value, PluginTrustTier& result) {
    if (value == "trusted_publisher") result = PluginTrustTier::trusted_publisher;
    else if (value == "third_party_signed") result = PluginTrustTier::third_party_signed;
    else if (value == "unverified_package") result = PluginTrustTier::unverified_package;
    else return false;
    return true;
}

bool parse_uint32(const std::string_view value, std::uint32_t& result) {
    if (value.empty()) return false;
    std::uint64_t parsed = 0;
    for (const unsigned char byte : value) {
        if (byte < '0' || byte > '9') return false;
        parsed = parsed * 10U + (byte - '0');
        if (parsed > std::numeric_limits<std::uint32_t>::max()) return false;
    }
    result = static_cast<std::uint32_t>(parsed);
    return true;
}

bool read_line(const std::string_view record, std::size_t& offset,
               const std::string_view prefix, std::string& value) {
    if (!record.substr(offset).starts_with(prefix)) return false;
    offset += prefix.size();
    const auto end = record.find('\n', offset);
    if (end == std::string_view::npos || end == offset) return false;
    value.assign(record.substr(offset, end - offset));
    offset = end + 1;
    return true;
}

}  // namespace

std::string_view plugin_trust_tier_name(const PluginTrustTier tier) noexcept {
    switch (tier) {
    case PluginTrustTier::trusted_publisher: return "trusted_publisher";
    case PluginTrustTier::third_party_signed: return "third_party_signed";
    case PluginTrustTier::unverified_package: return "unverified_package";
    }
    return "unknown";
}

PluginAuthorizationResult make_plugin_authorization(
    const PluginManifest& manifest, const std::string_view inventory_sha256,
    const std::string_view publisher_certificate_sha256,
    std::vector<std::string> granted_permissions) {
    return make_plugin_authorization(manifest, inventory_sha256,
                                     publisher_certificate_sha256,
                                     std::move(granted_permissions), {});
}

PluginAuthorizationResult make_plugin_authorization(
    const PluginManifest& manifest, const std::string_view inventory_sha256,
    const std::string_view publisher_certificate_sha256,
    std::vector<std::string> granted_permissions,
    const PluginAuthorizationContext context) {
    std::sort(granted_permissions.begin(), granted_permissions.end());
    if (std::adjacent_find(granted_permissions.begin(), granted_permissions.end()) !=
        granted_permissions.end()) return {false, {}, "duplicate permission grant"};
    for (const auto& permission : granted_permissions) {
        if (!is_known_plugin_permission(permission) ||
            std::find(manifest.permissions.begin(), manifest.permissions.end(), permission) ==
                manifest.permissions.end())
            return {false, {}, "permission was not declared by the installed manifest"};
    }
    PluginAuthorization authorization{kPluginAuthorizationSchemaVersion, manifest.id,
        manifest.version, std::string(inventory_sha256),
        std::string(publisher_certificate_sha256), std::move(granted_permissions), context};
    if (!valid_authorization(authorization))
        return {false, {}, "invalid plugin identity or installation binding"};
    return {true, std::move(authorization), {}};
}

std::string serialize_plugin_authorization(const PluginAuthorization& authorization) {
    if (!valid_authorization(authorization)) return {};
    std::string permissions;
    for (std::size_t index = 0; index < authorization.granted_permissions.size(); ++index) {
        if (index != 0) permissions.push_back(',');
        permissions += authorization.granted_permissions[index];
    }
    if (authorization.schema_version == 1) {
        return "schema_version=1\nplugin_id=" + authorization.plugin_id +
               "\nversion=" + authorization.version +
               "\ninventory_sha256=" + authorization.inventory_sha256 +
               "\npublisher_certificate_sha256=" + authorization.publisher_certificate_sha256 +
               "\ngranted_permissions=" + permissions + "\n";
    }
    return "schema_version=2\nplugin_id=" + authorization.plugin_id +
           "\nversion=" + authorization.version +
           "\ninventory_sha256=" + authorization.inventory_sha256 +
           "\npublisher_certificate_sha256=" + authorization.publisher_certificate_sha256 +
           "\ntrust_tier=" + std::string(plugin_trust_tier_name(authorization.context.trust_tier)) +
           "\ndisclaimer_version=" + std::to_string(authorization.context.disclaimer_version) +
           "\ninformed_consent=" + (authorization.context.informed_consent ? "1" : "0") +
           "\ngranted_permissions=" + permissions + "\n";
}

PluginAuthorizationResult parse_plugin_authorization(const std::string_view record) {
    if (record.empty() || record.size() > 4096 ||
        (!record.starts_with("schema_version=1\n") &&
         !record.starts_with("schema_version=2\n")))
        return {false, {}, "invalid authorization record header"};
    const bool legacy = record.starts_with("schema_version=1\n");
    std::size_t offset = std::string_view("schema_version=1\n").size();
    PluginAuthorization authorization;
    authorization.schema_version = legacy ? 1 : kPluginAuthorizationSchemaVersion;
    std::string permissions;
    if (!read_line(record, offset, "plugin_id=", authorization.plugin_id) ||
        !read_line(record, offset, "version=", authorization.version) ||
        !read_line(record, offset, "inventory_sha256=", authorization.inventory_sha256) ||
        !read_line(record, offset, "publisher_certificate_sha256=",
                   authorization.publisher_certificate_sha256))
        return {false, {}, "invalid authorization record fields"};
    if (!legacy) {
        std::string trust_tier;
        std::string disclaimer_version;
        std::string informed_consent;
        if (!read_line(record, offset, "trust_tier=", trust_tier) ||
            !read_line(record, offset, "disclaimer_version=", disclaimer_version) ||
            !read_line(record, offset, "informed_consent=", informed_consent) ||
            !parse_trust_tier(trust_tier, authorization.context.trust_tier) ||
            !parse_uint32(disclaimer_version, authorization.context.disclaimer_version) ||
            (informed_consent != "0" && informed_consent != "1"))
            return {false, {}, "invalid authorization consent fields"};
        authorization.context.informed_consent = informed_consent == "1";
    }
    if (!record.substr(offset).starts_with("granted_permissions="))
        return {false, {}, "invalid authorization permission field"};
    offset += std::string_view("granted_permissions=").size();
    const auto end = record.find('\n', offset);
    if (end == std::string_view::npos || end + 1 != record.size())
        return {false, {}, "invalid authorization permission field"};
    permissions.assign(record.substr(offset, end - offset));
    std::size_t permission_offset = 0;
    while (permission_offset < permissions.size()) {
        const auto separator = permissions.find(',', permission_offset);
        const auto length = separator == std::string::npos
            ? permissions.size() - permission_offset : separator - permission_offset;
        if (length == 0) return {false, {}, "empty authorization permission"};
        authorization.granted_permissions.push_back(permissions.substr(permission_offset, length));
        if (separator == std::string::npos) break;
        permission_offset = separator + 1;
    }
    if (!valid_authorization(authorization))
        return {false, {}, "invalid authorization record values"};
    return {true, std::move(authorization), {}};
}

bool is_plugin_permission_granted(
    const PluginAuthorization& authorization, const PluginManifest& installed_manifest,
    const std::string_view inventory_sha256,
    const std::string_view publisher_certificate_sha256, const std::string_view permission) {
    return valid_authorization(authorization) &&
           authorization.plugin_id == installed_manifest.id &&
           authorization.version == installed_manifest.version &&
           authorization.inventory_sha256 == inventory_sha256 &&
           authorization.publisher_certificate_sha256 == publisher_certificate_sha256 &&
           std::find(installed_manifest.permissions.begin(), installed_manifest.permissions.end(),
                     permission) != installed_manifest.permissions.end() &&
           std::binary_search(authorization.granted_permissions.begin(),
                              authorization.granted_permissions.end(), permission);
}

}  // namespace owo::plugin

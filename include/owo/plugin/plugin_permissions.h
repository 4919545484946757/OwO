#pragma once

#include <algorithm>
#include <array>
#include <string_view>
#include <vector>

namespace owo::plugin {

enum class PluginPermissionRisk {
    low,
    elevated,
    high,
    critical,
};

struct PluginPermissionDefinition {
    std::string_view name;
    PluginPermissionRisk risk;
    bool requires_full_trust;
};

inline constexpr std::array<PluginPermissionDefinition, 17> kKnownPluginPermissions{{
    {"clipboard.read", PluginPermissionRisk::elevated, false},
    {"clipboard.write", PluginPermissionRisk::high, false},
    {"filesystem.unrestricted", PluginPermissionRisk::critical, true},
    {"filesystem.user_selected", PluginPermissionRisk::elevated, false},
    {"input.commit", PluginPermissionRisk::high, false},
    {"input.context", PluginPermissionRisk::high, false},
    {"input.replace", PluginPermissionRisk::critical, false},
    {"microphone.capture", PluginPermissionRisk::high, true},
    {"network.client", PluginPermissionRisk::high, true},
    {"process.launch", PluginPermissionRisk::critical, true},
    {"resource.dictionary.install", PluginPermissionRisk::high, true},
    {"resource.material.install", PluginPermissionRisk::high, true},
    {"resource.theme.install", PluginPermissionRisk::high, true},
    {"screen.capture", PluginPermissionRisk::high, true},
    {"system.full_trust", PluginPermissionRisk::critical, false},
    {"ui.desktop_pet", PluginPermissionRisk::high, true},
    {"ui.overlay", PluginPermissionRisk::high, true},
}};

[[nodiscard]] inline bool is_known_plugin_permission(const std::string_view permission) noexcept {
    for (const auto known : kKnownPluginPermissions) {
        if (permission == known.name) return true;
    }
    return false;
}

[[nodiscard]] inline PluginPermissionRisk plugin_permission_risk(
    const std::string_view permission) noexcept {
    for (const auto& known : kKnownPluginPermissions) {
        if (permission == known.name) return known.risk;
    }
    return PluginPermissionRisk::critical;
}

[[nodiscard]] inline bool plugin_permission_requires_full_trust(
    const std::string_view permission) noexcept {
    for (const auto& known : kKnownPluginPermissions) {
        if (permission == known.name) return known.requires_full_trust;
    }
    return true;
}

[[nodiscard]] inline bool plugin_permissions_require_full_trust(
    const std::vector<std::string>& permissions) noexcept {
    return std::any_of(permissions.begin(), permissions.end(),
                       plugin_permission_requires_full_trust);
}

}  // namespace owo::plugin

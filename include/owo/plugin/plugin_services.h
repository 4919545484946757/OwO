#pragma once

#include <array>
#include <string_view>

namespace owo::plugin {

struct PluginServiceDefinition {
    std::string_view name;
    std::string_view required_permission;
    std::string_view summary;
};

// Standard Core-to-plugin services. Plugins may also expose vendor-prefixed *.v1 services.
// A non-empty permission is injected by PluginExecutor and is verified again by PluginHost.
inline constexpr std::array<PluginServiceDefinition, 17> kKnownPluginServices{{
    {"owo.health.check.v1", {}, "Check whether the plugin is ready."},
    {"owo.lifecycle.event.v1", {}, "Receive a bounded Core lifecycle event."},
    {"owo.command.execute.v1", {}, "Execute a plugin-defined command."},
    {"owo.dictionary.lookup.v1", {}, "Return dictionary results for an explicit query."},
    {"owo.candidate.transform.v1", "candidate.transform", "Transform an explicit candidate page."},
    {"owo.settings.schema.v1", "config.read", "Return the plugin settings schema."},
    {"owo.settings.read.v1", "config.read", "Read the plugin's own settings."},
    {"owo.settings.write.v1", "config.write", "Write the plugin's own settings."},
    {"owo.notification.show.v1", "notification.show", "Show a user notification."},
    {"owo.ui.settings-page.v1", "ui.settings_page", "Render a declarative settings page."},
    {"owo.ui.overlay.v1", "ui.overlay", "Control an authorized desktop overlay."},
    {"owo.ui.desktop-pet.v1", "ui.desktop_pet", "Control an authorized desktop pet."},
    {"owo.resource.dictionary.install.v1", "resource.dictionary.install", "Install a dictionary resource."},
    {"owo.resource.theme.install.v1", "resource.theme.install", "Install a theme resource."},
    {"owo.resource.material.install.v1", "resource.material.install", "Install a material resource."},
    {"owo.resource.model.install.v1", "resource.model.install", "Install a ranking model resource."},
    {"owo.resource.sound.install.v1", "resource.sound.install", "Install a sound resource."},
}};

[[nodiscard]] inline const PluginServiceDefinition* find_plugin_service(
    const std::string_view service) noexcept {
    for (const auto& known : kKnownPluginServices) {
        if (known.name == service) return &known;
    }
    return nullptr;
}

}  // namespace owo::plugin

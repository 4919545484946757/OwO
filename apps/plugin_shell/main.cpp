#include "owo/plugin/plugin_installer.h"
#include "owo/plugin/plugin_authorization_store.h"
#include "owo/plugin/plugin_permissions.h"
#include "owo/plugin/plugin_store.h"

#include <Windows.h>

#include <algorithm>
#include <charconv>
#include <iostream>
#include <string>
#include <vector>

namespace {

std::string utf8(const std::wstring_view value) {
    if (value.empty()) return {};
    const auto size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0,
                                          nullptr, nullptr);
    if (size <= 0) return {};
    std::string result(static_cast<std::size_t>(size), '\0');
    if (WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                            static_cast<int>(value.size()), result.data(), size,
                            nullptr, nullptr) != size) return {};
    return result;
}

std::string json_escape(const std::string_view value) {
    static constexpr char digits[] = "0123456789abcdef";
    std::string result;
    result.reserve(value.size() + 2);
    result.push_back('"');
    for (const unsigned char byte : value) {
        switch (byte) {
        case '"': result += "\\\""; break;
        case '\\': result += "\\\\"; break;
        case '\b': result += "\\b"; break;
        case '\f': result += "\\f"; break;
        case '\n': result += "\\n"; break;
        case '\r': result += "\\r"; break;
        case '\t': result += "\\t"; break;
        default:
            if (byte < 0x20) {
                result += "\\u00";
                result.push_back(digits[byte >> 4]);
                result.push_back(digits[byte & 0x0f]);
            } else {
                result.push_back(static_cast<char>(byte));
            }
        }
    }
    result.push_back('"');
    return result;
}

const char* recovery_kind(const owo::plugin::PluginRecoveryKind kind) {
    using enum owo::plugin::PluginRecoveryKind;
    switch (kind) {
    case retained_staging: return "retained_staging";
    case retained_uninstall: return "retained_uninstall";
    case orphaned_version: return "orphaned_version";
    case orphaned_record: return "orphaned_record";
    case orphaned_authorization: return "orphaned_authorization";
    case inactive_version: return "inactive_version";
    case invalid_active_record: return "invalid_active_record";
    case unsafe_store_entry: return "unsafe_store_entry";
    }
    return "unknown";
}

const char* recovery_action(const owo::plugin::PluginRecoveryKind kind) {
    if (kind == owo::plugin::PluginRecoveryKind::inactive_version) return "activate";
    if (kind == owo::plugin::PluginRecoveryKind::unsafe_store_entry) return "manual";
    return "cleanup";
}

const char* install_stage(const owo::plugin::PluginInstallStage stage) {
    using enum owo::plugin::PluginInstallStage;
    switch (stage) {
    case none: return "none";
    case package_inspection: return "package_inspection";
    case publisher_trust: return "publisher_trust";
    case risk_consent: return "risk_consent";
    case store_initialization: return "store_initialization";
    case staging_extraction: return "staging_extraction";
    case version_publication: return "version_publication";
    case permission_authorization: return "permission_authorization";
    case completed: return "completed";
    }
    return "unknown";
}

const char* risk_level(const owo::plugin::PluginInstallRiskLevel risk) {
    using enum owo::plugin::PluginInstallRiskLevel;
    switch (risk) {
    case low: return "low";
    case elevated: return "elevated";
    case high: return "high";
    case critical: return "critical";
    }
    return "critical";
}

void print_permissions(const std::vector<std::string>& permissions) {
    std::cout << '[';
    for (std::size_t index = 0; index < permissions.size(); ++index) {
        if (index != 0) std::cout << ',';
        std::cout << json_escape(permissions[index]);
    }
    std::cout << ']';
}

owo::plugin::PluginInstallRiskLevel manifest_risk(
    const owo::plugin::PluginManifest& manifest,
    const owo::plugin::PluginTrustTier trust_tier) {
    auto risk = owo::plugin::PluginInstallRiskLevel::low;
    for (const auto& permission : manifest.permissions) {
        const auto permission_risk = owo::plugin::plugin_permission_risk(permission);
        owo::plugin::PluginInstallRiskLevel value{};
        switch (permission_risk) {
        case owo::plugin::PluginPermissionRisk::low:
            value = owo::plugin::PluginInstallRiskLevel::low;
            break;
        case owo::plugin::PluginPermissionRisk::elevated:
            value = owo::plugin::PluginInstallRiskLevel::elevated;
            break;
        case owo::plugin::PluginPermissionRisk::high:
            value = owo::plugin::PluginInstallRiskLevel::high;
            break;
        case owo::plugin::PluginPermissionRisk::critical:
            value = owo::plugin::PluginInstallRiskLevel::critical;
            break;
        }
        if (static_cast<int>(value) > static_cast<int>(risk)) risk = value;
    }
    if (trust_tier == owo::plugin::PluginTrustTier::third_party_signed &&
        static_cast<int>(risk) < static_cast<int>(owo::plugin::PluginInstallRiskLevel::high))
        risk = owo::plugin::PluginInstallRiskLevel::high;
    if (trust_tier == owo::plugin::PluginTrustTier::unverified_package)
        risk = owo::plugin::PluginInstallRiskLevel::critical;
    return risk;
}

void print_install_preview(const owo::plugin::PluginInstallPreview& preview) {
    std::cout << "{\"schema_version\":1"
              << ",\"ok\":" << (preview.ok ? "true" : "false")
              << ",\"plugin_id\":" << json_escape(preview.manifest.id)
              << ",\"name\":" << json_escape(preview.manifest.name)
              << ",\"version\":" << json_escape(preview.manifest.version)
              << ",\"inventory_sha256\":" << json_escape(preview.inventory_sha256)
              << ",\"trust_tier\":"
              << json_escape(owo::plugin::plugin_trust_tier_name(preview.trust_tier))
              << ",\"risk_level\":" << json_escape(risk_level(preview.risk_level))
              << ",\"requires_risk_consent\":"
              << (preview.requires_risk_consent ? "true" : "false")
              << ",\"requires_full_trust\":"
              << (preview.requires_full_trust ? "true" : "false")
              << ",\"network\":" << (preview.manifest.network ? "true" : "false")
              << ",\"permissions\":";
    print_permissions(preview.manifest.permissions);
    std::cout << ",\"publisher_display_name\":"
              << json_escape(preview.publisher_display_name)
              << ",\"publisher_certificate_sha256\":"
              << json_escape(preview.publisher_certificate_sha256)
              << ",\"trust_diagnostic\":" << json_escape(preview.trust_diagnostic)
              << ",\"diagnostic\":" << json_escape(preview.diagnostic) << "}\n";
}

void print_install_result(const owo::plugin::PluginInstallResult& result) {
    std::cout << "{\"schema_version\":1"
              << ",\"ok\":" << (result.ok ? "true" : "false")
              << ",\"stage\":" << json_escape(install_stage(result.stage))
              << ",\"version_published\":"
              << (result.version_published ? "true" : "false")
              << ",\"activated\":" << (result.activated ? "true" : "false")
              << ",\"plugin_id\":" << json_escape(result.manifest.id)
              << ",\"name\":" << json_escape(result.manifest.name)
              << ",\"version\":" << json_escape(result.manifest.version)
              << ",\"installed_path\":"
              << json_escape(utf8(result.installed_path.wstring()))
              << ",\"retained_staging_path\":"
              << json_escape(utf8(result.retained_staging_path.wstring()))
              << ",\"previous_version\":" << json_escape(result.previous_version)
              << ",\"inventory_sha256\":" << json_escape(result.inventory_sha256)
              << ",\"publisher_display_name\":"
              << json_escape(result.publisher_display_name)
              << ",\"publisher_certificate_sha256\":"
              << json_escape(result.publisher_certificate_sha256)
              << ",\"trust_tier\":"
              << json_escape(owo::plugin::plugin_trust_tier_name(result.trust_tier))
              << ",\"risk_level\":" << json_escape(risk_level(result.risk_level))
              << ",\"permissions_authorized\":"
              << (result.permissions_authorized ? "true" : "false")
              << ",\"diagnostic\":" << json_escape(result.diagnostic) << "}\n";
}

void print_management_result(const owo::plugin::PluginManagementResult& result) {
    std::cout << "{\"ok\":" << (result.ok ? "true" : "false")
              << ",\"plugin_id\":" << json_escape(result.plugin_id)
              << ",\"version\":" << json_escape(result.version)
              << ",\"path\":" << json_escape(utf8(result.affected_path.wstring()))
              << ",\"diagnostic\":" << json_escape(result.diagnostic) << "}\n";
}

void print_uninstall_result(const owo::plugin::PluginUninstallResult& result) {
    std::cout << "{\"ok\":" << (result.ok ? "true" : "false")
              << ",\"plugin_id\":" << json_escape(result.plugin_id)
              << ",\"version\":" << json_escape(result.version)
              << ",\"version_removed\":" << (result.version_removed ? "true" : "false")
              << ",\"authorization_removed\":"
              << (result.authorization_removed ? "true" : "false")
              << ",\"last_version\":" << (result.last_version ? "true" : "false")
              << ",\"sandbox_profile_removed\":"
              << (result.sandbox_profile_removed ? "true" : "false")
              << ",\"data_preserved\":" << (result.data_preserved ? "true" : "false")
              << ",\"retained_uninstall_path\":"
              << json_escape(utf8(result.retained_uninstall_path.wstring()))
              << ",\"diagnostic\":" << json_escape(result.diagnostic) << "}\n";
}

int list(const std::filesystem::path& root) {
    const auto plugins = owo::plugin::list_installed_plugins(root);
    if (!plugins.ok) {
        std::cerr << plugins.diagnostic << '\n';
        return 2;
    }
    const auto recovery = owo::plugin::scan_plugin_store_recovery(root);
    if (!recovery.ok) {
        std::cerr << recovery.diagnostic << '\n';
        return 2;
    }
    std::cout << "{\"schema_version\":1,\"plugins\":[";
    for (std::size_t index = 0; index < plugins.versions.size(); ++index) {
        const auto& plugin = plugins.versions[index];
        const auto installed = owo::plugin::query_installed_plugin_version(
            root, plugin.manifest.id, plugin.manifest.version);
        const auto authorization = owo::plugin::load_plugin_authorization(
            root, plugin.manifest.id, plugin.manifest.version);
        const auto trust_tier = installed.ok
            ? installed.trust_tier
            : owo::plugin::PluginTrustTier::unverified_package;
        if (index != 0) std::cout << ',';
        std::cout << "{\"id\":" << json_escape(plugin.manifest.id)
                  << ",\"name\":" << json_escape(plugin.manifest.name)
                  << ",\"version\":" << json_escape(plugin.manifest.version)
                  << ",\"active\":" << (plugin.active ? "true" : "false")
                  << ",\"trust_tier\":"
                  << json_escape(owo::plugin::plugin_trust_tier_name(trust_tier))
                  << ",\"risk_level\":"
                  << json_escape(risk_level(manifest_risk(plugin.manifest, trust_tier)))
                  << ",\"permissions_authorized\":"
                  << (authorization.ok && !authorization.value.granted_permissions.empty()
                          ? "true" : "false")
                  << ",\"permissions\":";
        print_permissions(plugin.manifest.permissions);
        std::cout << '}';
    }
    std::cout << "],\"recovery\":[";
    for (std::size_t index = 0; index < recovery.items.size(); ++index) {
        const auto& item = recovery.items[index];
        if (index != 0) std::cout << ',';
        std::cout << "{\"index\":" << index
                  << ",\"kind\":" << json_escape(recovery_kind(item.kind))
                  << ",\"action\":" << json_escape(recovery_action(item.kind))
                  << ",\"path\":" << json_escape(utf8(item.path.wstring()))
                  << ",\"plugin_id\":" << json_escape(item.plugin_id)
                  << ",\"version\":" << json_escape(item.version)
                  << ",\"diagnostic\":" << json_escape(item.diagnostic) << '}';
    }
    std::cout << "]}\n";
    return 0;
}

}  // namespace

int wmain(const int argc, wchar_t** argv) {
    if (argc < 3) {
        std::cerr << "usage: owo_plugin_shell <store-root> "
                     "<list|inspect-install|install|install-risk|activate|deactivate|"
                     "revoke|uninstall|cleanup> ...\n";
        return 1;
    }
    SetConsoleOutputCP(CP_UTF8);
    const std::filesystem::path root(argv[1]);
    const std::wstring_view command(argv[2]);
    if (command == L"list" && argc == 3) return list(root);
    if (command == L"inspect-install" && argc == 4) {
        const auto preview = owo::plugin::inspect_plugin_install(argv[3]);
        print_install_preview(preview);
        if (!preview.ok) {
            std::cerr << preview.diagnostic << '\n';
            return 2;
        }
        return 0;
    }
    if (command == L"install" && argc == 4) {
        const auto result = owo::plugin::install_plugin_package(argv[3], root);
        print_install_result(result);
        if (!result.ok) {
            std::cerr << result.diagnostic << '\n';
            return 2;
        }
        return 0;
    }
    if (command == L"install-risk" && argc == 7) {
        if (std::wstring_view(argv[5]) != L"1" ||
            std::wstring_view(argv[6]) != L"I_ACCEPT_PLUGIN_RISK_V1") {
            std::cerr << "risk disclaimer acknowledgement is invalid\n";
            return 2;
        }
        const auto preview = owo::plugin::inspect_plugin_install(argv[3]);
        if (!preview.ok) {
            print_install_preview(preview);
            std::cerr << preview.diagnostic << '\n';
            return 2;
        }
        owo::plugin::PluginInstallConsent consent;
        consent.inventory_sha256 = utf8(argv[4]);
        consent.disclaimer_version = owo::plugin::kPluginRiskDisclaimerVersion;
        consent.accept_untrusted_publisher = true;
        consent.accept_full_trust = true;
        consent.granted_permissions = preview.manifest.permissions;
        const auto result = owo::plugin::install_plugin_package(argv[3], root, consent);
        print_install_result(result);
        if (!result.ok) {
            std::cerr << result.diagnostic << '\n';
            return 2;
        }
        return 0;
    }
    if (command == L"activate" && argc == 5) {
        const auto id = utf8(argv[3]);
        const auto version = utf8(argv[4]);
        const auto installed = owo::plugin::query_installed_plugin_version(root, id, version);
        if (!installed.ok) {
            std::cerr << installed.diagnostic << '\n';
            return 2;
        }
        const bool full_trust = std::find(installed.manifest.permissions.begin(),
            installed.manifest.permissions.end(), "system.full_trust") !=
            installed.manifest.permissions.end();
        if (full_trust) {
            const auto authorization = owo::plugin::load_plugin_authorization(root, id, version);
            const bool complete = authorization.ok && std::all_of(
                installed.manifest.permissions.begin(), installed.manifest.permissions.end(),
                [&](const std::string& permission) {
                    return owo::plugin::is_plugin_permission_granted(
                        authorization.value, installed.manifest, installed.inventory_sha256,
                        installed.publisher_certificate_sha256, permission);
                });
            if (!complete) {
                std::cerr << "full-trust authorization is incomplete or revoked\n";
                return 2;
            }
        }
        const auto result = owo::plugin::activate_installed_plugin_version(
            root, id, version);
        if (!result.ok) {
            std::cerr << result.diagnostic << '\n';
            return 2;
        }
        print_management_result({true, result.manifest.id, result.manifest.version,
                                 result.installed_path, {}});
        return 0;
    }
    if (command == L"deactivate" && argc == 5) {
        const auto result = owo::plugin::deactivate_plugin(
            root, utf8(argv[3]), utf8(argv[4]));
        if (!result.ok) {
            std::cerr << result.diagnostic << '\n';
            return 2;
        }
        print_management_result(result);
        return 0;
    }
    if (command == L"revoke" && argc == 5) {
        const auto id = utf8(argv[3]);
        const auto version = utf8(argv[4]);
        const auto installed = owo::plugin::query_installed_plugin_version(root, id, version);
        const auto existing = owo::plugin::load_plugin_authorization(root, id, version);
        if (!installed.ok || !existing.ok) {
            std::cerr << (installed.ok ? existing.diagnostic : installed.diagnostic) << '\n';
            return 2;
        }
        const auto revoked = owo::plugin::make_plugin_authorization(
            installed.manifest, installed.inventory_sha256,
            installed.publisher_certificate_sha256, {}, existing.value.context);
        const auto saved = revoked.ok
            ? owo::plugin::save_plugin_authorization(root, revoked.value)
            : owo::plugin::PluginAuthorizationStoreResult{false, {}, {}, revoked.diagnostic};
        if (!saved.ok) {
            std::cerr << saved.diagnostic << '\n';
            return 2;
        }
        print_management_result({true, id, version, saved.record_path, {}});
        return 0;
    }
    if (command == L"uninstall" && argc == 5) {
        const auto result = owo::plugin::uninstall_plugin_version(
            root, utf8(argv[3]), utf8(argv[4]));
        print_uninstall_result(result);
        if (!result.ok) {
            std::cerr << result.diagnostic << '\n';
            return 2;
        }
        return 0;
    }
    if (command == L"cleanup" && argc == 8) {
        std::size_t index = 0;
        const auto text = utf8(argv[3]);
        const auto parsed = std::from_chars(text.data(), text.data() + text.size(), index);
        const auto scan = owo::plugin::scan_plugin_store_recovery(root);
        if (parsed.ec != std::errc{} || parsed.ptr != text.data() + text.size() ||
            !scan.ok || index >= scan.items.size()) {
            std::cerr << (scan.ok ? "recovery index is invalid" : scan.diagnostic) << '\n';
            return 2;
        }
        const auto& selected = scan.items[index];
        if (utf8(argv[4]) != recovery_kind(selected.kind) ||
            std::filesystem::path(argv[5]).lexically_normal() != selected.path ||
            utf8(argv[6]) != selected.plugin_id || utf8(argv[7]) != selected.version) {
            std::cerr << "recovery selection changed; refresh before applying cleanup\n";
            return 2;
        }
        const auto result = owo::plugin::cleanup_plugin_recovery_item(root, selected);
        if (!result.ok) {
            std::cerr << result.diagnostic << '\n';
            return 2;
        }
        print_management_result(result);
        return 0;
    }
    std::cerr << "invalid plugin management command or argument count\n";
    return 1;
}

#include "owo/plugin/plugin_installer.h"

#include "owo/plugin/package_archive.h"
#include "owo/plugin/package_extraction.h"
#include "owo/plugin/package_signature.h"
#include "owo/plugin/plugin_authorization_store.h"
#include "owo/plugin/plugin_store.h"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#endif

#include <algorithm>
#include <atomic>
#include <utility>

namespace owo::plugin {
namespace {

constexpr std::string_view kUnverifiedPublisherBinding =
    "0000000000000000000000000000000000000000000000000000000000000000";

PluginInstallResult failure(const PluginInstallStage stage, std::string diagnostic) {
    PluginInstallResult result;
    result.stage = stage;
    result.diagnostic = std::move(diagnostic);
    return result;
}

PluginInstallRiskLevel maximum_risk(const PluginInstallRiskLevel left,
                                    const PluginInstallRiskLevel right) {
    return static_cast<int>(left) >= static_cast<int>(right) ? left : right;
}

PluginInstallRiskLevel install_risk(const PluginPermissionRisk risk) {
    switch (risk) {
    case PluginPermissionRisk::low: return PluginInstallRiskLevel::low;
    case PluginPermissionRisk::elevated: return PluginInstallRiskLevel::elevated;
    case PluginPermissionRisk::high: return PluginInstallRiskLevel::high;
    case PluginPermissionRisk::critical: return PluginInstallRiskLevel::critical;
    }
    return PluginInstallRiskLevel::critical;
}

PluginInstallPreview preview(const PackageInspection& package) {
    PluginInstallPreview result;
    if (!package.ok) {
        result.diagnostic = package.diagnostic;
        return result;
    }
    const auto manifest_bytes = read_package_entry(package, "manifest.json", 64U * 1024U);
    if (!manifest_bytes.ok) {
        result.diagnostic = "cannot read manifest from package snapshot: " +
                            manifest_bytes.diagnostic;
        return result;
    }
    const auto manifest = parse_manifest(manifest_bytes.bytes);
    if (!manifest.ok) {
        result.diagnostic = "package manifest is invalid: " + manifest.diagnostic;
        return result;
    }
    const auto trust = verify_signed_package_trust(package);
    result.manifest = manifest.value;
    result.inventory_sha256 = package.inventory_sha256;
    result.publisher_display_name = trust.publisher_display_name;
    result.publisher_certificate_sha256 = trust.certificate_sha256;
    result.trust_diagnostic = trust.diagnostic;
    if (trust.ok) result.trust_tier = PluginTrustTier::trusted_publisher;
    else if (trust.cryptographic_signature_valid && !trust.certificate_sha256.empty())
        result.trust_tier = PluginTrustTier::third_party_signed;
    else result.trust_tier = PluginTrustTier::unverified_package;

    result.risk_level = PluginInstallRiskLevel::low;
    for (const auto& permission : result.manifest.permissions) {
        result.risk_level = maximum_risk(
            result.risk_level, install_risk(plugin_permission_risk(permission)));
    }
    if (result.trust_tier == PluginTrustTier::third_party_signed)
        result.risk_level = maximum_risk(result.risk_level, PluginInstallRiskLevel::high);
    if (result.trust_tier == PluginTrustTier::unverified_package)
        result.risk_level = PluginInstallRiskLevel::critical;
    result.requires_full_trust = std::find(result.manifest.permissions.begin(),
        result.manifest.permissions.end(), "system.full_trust") !=
        result.manifest.permissions.end();
    result.requires_risk_consent = result.trust_tier != PluginTrustTier::trusted_publisher ||
        static_cast<int>(result.risk_level) >= static_cast<int>(PluginInstallRiskLevel::high);
    result.ok = true;
    return result;
}

bool exact_permission_grant(const PluginManifest& manifest,
                            std::vector<std::string> permissions) {
    auto declared = manifest.permissions;
    std::sort(declared.begin(), declared.end());
    std::sort(permissions.begin(), permissions.end());
    return declared == permissions &&
        std::adjacent_find(permissions.begin(), permissions.end()) == permissions.end();
}

#ifdef _WIN32
std::filesystem::path transaction_staging_path(const std::filesystem::path& root) {
    static std::atomic<std::uint64_t> sequence{};
    const auto name = L".install-" + std::to_wstring(GetCurrentProcessId()) + L"-" +
                      std::to_wstring(GetTickCount64()) + L"-" +
                      std::to_wstring(sequence.fetch_add(1, std::memory_order_relaxed));
    return root / L"staging" / name;
}

bool remove_transaction_staging(const std::filesystem::path& root,
                                const std::filesystem::path& staging) {
    const auto normalized = staging.lexically_normal();
    const auto expected_parent = (root / L"staging").lexically_normal();
    if (normalized.parent_path() != expected_parent ||
        !normalized.filename().native().starts_with(L".install-")) return false;
    std::error_code error;
    std::filesystem::remove_all(normalized, error);
    return !error && !std::filesystem::exists(normalized, error) && !error;
}
#endif

PluginInstallResult install_snapshot(
    const PackageInspection& package, const PluginInstallPreview& inspected,
    const std::filesystem::path& plugin_store_root,
    const PluginInstallConsent* consent) {
    if (!inspected.ok) return failure(PluginInstallStage::package_inspection,
                                      inspected.diagnostic);
    if (consent == nullptr) {
        if (inspected.trust_tier != PluginTrustTier::trusted_publisher)
            return failure(PluginInstallStage::publisher_trust,
                           inspected.trust_diagnostic.empty()
                               ? "package publisher is not trusted"
                               : inspected.trust_diagnostic);
        if (inspected.requires_risk_consent)
            return failure(PluginInstallStage::risk_consent,
                           "requested capabilities require informed per-package consent");
    } else {
        if (consent->inventory_sha256 != inspected.inventory_sha256 ||
            consent->disclaimer_version != kPluginRiskDisclaimerVersion)
            return failure(PluginInstallStage::risk_consent,
                           "risk consent does not match this package or disclaimer version");
        if (inspected.trust_tier != PluginTrustTier::trusted_publisher &&
            !consent->accept_untrusted_publisher)
            return failure(PluginInstallStage::risk_consent,
                           "untrusted publisher risk was not accepted");
        if (inspected.requires_full_trust && !consent->accept_full_trust)
            return failure(PluginInstallStage::risk_consent,
                           "full-trust execution risk was not accepted");
        if (!exact_permission_grant(inspected.manifest, consent->granted_permissions))
            return failure(PluginInstallStage::risk_consent,
                           "permission consent must exactly match the package manifest");
    }

    const auto initialized = initialize_plugin_store(plugin_store_root);
    if (!initialized.ok)
        return failure(PluginInstallStage::store_initialization, initialized.diagnostic);

#ifdef _WIN32
    const auto staging = transaction_staging_path(plugin_store_root);
    const auto extracted = extract_package_to_staging(package, staging);
    if (!extracted.ok) {
        auto result = failure(PluginInstallStage::staging_extraction, extracted.diagnostic);
        std::error_code error;
        const bool staging_exists = std::filesystem::exists(staging, error);
        if (error || (staging_exists && !remove_transaction_staging(plugin_store_root, staging))) {
            result.retained_staging_path = staging;
            result.diagnostic += "; exact staging cleanup failed";
        }
        return result;
    }

    std::size_t expected_files = 0;
    std::uint64_t expected_bytes = 0;
    for (const auto& entry : package.entries) {
        if (!entry.path.ends_with('/')) {
            ++expected_files;
            expected_bytes += entry.uncompressed_size;
        }
    }
    if (extracted.files_written != expected_files || extracted.bytes_written != expected_bytes) {
        auto result = failure(PluginInstallStage::staging_extraction,
                              "staging output counters do not match the immutable package snapshot");
        if (!remove_transaction_staging(plugin_store_root, staging)) {
            result.retained_staging_path = staging;
            result.diagnostic += "; exact staging cleanup failed";
        }
        return result;
    }

    const std::string publisher_binding = inspected.publisher_certificate_sha256.empty()
        ? std::string(kUnverifiedPublisherBinding)
        : inspected.publisher_certificate_sha256;
    const bool activate = consent == nullptr;
    const auto published = publish_staged_plugin(plugin_store_root, staging,
        inspected.inventory_sha256, publisher_binding, activate, inspected.trust_tier);
    if (!published.ok) {
        auto result = failure(PluginInstallStage::version_publication, published.diagnostic);
        result.version_published = published.version_published;
        result.activated = published.activated;
        result.manifest = published.manifest;
        result.installed_path = published.installed_path;
        result.previous_version = published.previous_version;
        result.inventory_sha256 = inspected.inventory_sha256;
        result.publisher_display_name = inspected.publisher_display_name;
        result.publisher_certificate_sha256 = inspected.publisher_certificate_sha256;
        result.trust_tier = inspected.trust_tier;
        result.risk_level = inspected.risk_level;
        std::error_code error;
        const bool staging_exists = std::filesystem::exists(staging, error);
        if (!published.version_published &&
            (error || (staging_exists && !remove_transaction_staging(plugin_store_root, staging)))) {
            result.retained_staging_path = staging;
            result.diagnostic += "; exact staging cleanup failed";
        }
        return result;
    }

    if (consent != nullptr) {
        const PluginAuthorizationContext context{
            inspected.trust_tier, consent->disclaimer_version, true};
        const auto authorization = make_plugin_authorization(
            published.manifest, inspected.inventory_sha256, publisher_binding,
            consent->granted_permissions, context);
        const auto saved = authorization.ok
            ? save_plugin_authorization(plugin_store_root, authorization.value)
            : PluginAuthorizationStoreResult{false, {}, {}, authorization.diagnostic};
        if (!saved.ok) {
            auto result = failure(PluginInstallStage::permission_authorization,
                                  "plugin was installed inactive but authorization failed: " +
                                      saved.diagnostic);
            result.version_published = true;
            result.manifest = published.manifest;
            result.installed_path = published.installed_path;
            result.previous_version = published.previous_version;
            result.inventory_sha256 = inspected.inventory_sha256;
            result.publisher_display_name = inspected.publisher_display_name;
            result.publisher_certificate_sha256 = inspected.publisher_certificate_sha256;
            result.trust_tier = inspected.trust_tier;
            result.risk_level = inspected.risk_level;
            return result;
        }
    }

    PluginInstallResult result;
    result.ok = true;
    result.stage = PluginInstallStage::completed;
    result.version_published = true;
    result.activated = activate;
    result.manifest = published.manifest;
    result.installed_path = published.installed_path;
    result.previous_version = published.previous_version;
    result.inventory_sha256 = inspected.inventory_sha256;
    result.publisher_display_name = inspected.publisher_display_name;
    result.publisher_certificate_sha256 = inspected.publisher_certificate_sha256;
    result.trust_tier = inspected.trust_tier;
    result.risk_level = inspected.risk_level;
    result.permissions_authorized = consent != nullptr;
    return result;
#else
    static_cast<void>(package);
    static_cast<void>(plugin_store_root);
    return failure(PluginInstallStage::staging_extraction,
                   "plugin installation is currently available on Windows only");
#endif
}

}  // namespace

PluginInstallPreview inspect_plugin_install(const std::filesystem::path& package_path) {
    return preview(inspect_package(package_path));
}

PluginInstallResult install_plugin_package(
    const std::filesystem::path& package_path,
    const std::filesystem::path& plugin_store_root) {
    const auto package = inspect_package(package_path);
    return install_snapshot(package, preview(package), plugin_store_root, nullptr);
}

PluginInstallResult install_plugin_package(
    const std::filesystem::path& package_path,
    const std::filesystem::path& plugin_store_root,
    const PluginInstallConsent& consent) {
    const auto package = inspect_package(package_path);
    return install_snapshot(package, preview(package), plugin_store_root, &consent);
}

}  // namespace owo::plugin

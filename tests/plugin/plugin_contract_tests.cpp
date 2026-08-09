#include "owo/plugin/plugin_authorization.h"
#include "owo/plugin/plugin_protocol.h"

#include <algorithm>
#include <string>
#include <vector>

namespace {

bool same_message(const owo::plugin::PluginMessage& left,
                  const owo::plugin::PluginMessage& right) {
    return left.type == right.type && left.status == right.status &&
           left.request_id == right.request_id &&
           left.target_request_id == right.target_request_id &&
           left.timeout_ms == right.timeout_ms && left.plugin_id == right.plugin_id &&
           left.service == right.service && left.payload == right.payload &&
           left.diagnostic == right.diagnostic && left.capabilities == right.capabilities;
}

bool rejected_message(std::string bytes) {
    return !owo::plugin::decode_plugin_message(bytes).validation;
}

}  // namespace

int main() {
    owo::plugin::PluginMessage invoke;
    invoke.type = owo::plugin::PluginMessageType::invoke_request;
    invoke.status = owo::plugin::PluginStatus::success;
    invoke.request_id = 42;
    invoke.timeout_ms = 250;
    invoke.plugin_id = "owo.plugin.example";
    invoke.service = "text.transform";
    invoke.payload = "opaque-request";
    const auto encoded = owo::plugin::encode_plugin_message(invoke);
    const auto decoded = owo::plugin::decode_plugin_message(encoded);
    if (encoded.empty() || !decoded.validation || !same_message(decoded.message, invoke)) return 1;

    owo::plugin::PluginMessage hello;
    hello.type = owo::plugin::PluginMessageType::hello_response;
    hello.status = owo::plugin::PluginStatus::success;
    hello.request_id = 1;
    hello.plugin_id = "owo.plugin.example";
    hello.capabilities = {"cancel.v1", "invoke.v1"};
    const auto hello_decoded = owo::plugin::decode_plugin_message(
        owo::plugin::encode_plugin_message(hello));
    if (!hello_decoded.validation || hello_decoded.message.capabilities != hello.capabilities) return 2;

    owo::plugin::PluginMessage cancel;
    cancel.type = owo::plugin::PluginMessageType::cancel_request;
    cancel.status = owo::plugin::PluginStatus::success;
    cancel.request_id = 44;
    cancel.target_request_id = 42;
    cancel.plugin_id = "owo.plugin.example";
    if (!owo::plugin::decode_plugin_message(owo::plugin::encode_plugin_message(cancel)).validation)
        return 3;
    cancel.target_request_id = cancel.request_id;
    if (!owo::plugin::encode_plugin_message(cancel).empty()) return 4;

    auto malformed = encoded;
    malformed[4] = 2;
    const auto wrong_version = owo::plugin::decode_plugin_message(malformed);
    if (wrong_version.validation.error != owo::protocol::ErrorCode::unsupported_protocol) return 5;
    malformed = encoded; malformed[8] = static_cast<char>(99);
    if (!rejected_message(malformed)) return 6;
    malformed = encoded; malformed[10] = 1;
    if (!rejected_message(malformed)) return 7;
    malformed = encoded; malformed.append("trailing");
    if (!rejected_message(malformed)) return 8;
    malformed = encoded;
    for (std::size_t offset = 12; offset < 20; ++offset) malformed[offset] = 0;
    if (!rejected_message(malformed)) return 9;
    invoke.payload.assign(owo::plugin::kMaximumPluginPayloadBytes + 1, 'x');
    if (!owo::plugin::encode_plugin_message(invoke).empty()) return 10;
    hello.capabilities = {"invoke.v1", "cancel.v1"};
    if (!owo::plugin::encode_plugin_message(hello).empty()) return 11;
    owo::plugin::PluginMessage error_response;
    error_response.type = owo::plugin::PluginMessageType::error_response;
    error_response.status = owo::plugin::PluginStatus::permission_denied;
    error_response.request_id = 45;
    error_response.plugin_id = "owo.plugin.example";
    error_response.diagnostic = "permission denied";
    if (!owo::plugin::decode_plugin_message(
            owo::plugin::encode_plugin_message(error_response)).validation) return 22;
    error_response.diagnostic = std::string("\xc0\x80", 2);
    if (!owo::plugin::encode_plugin_message(error_response).empty()) return 23;

    owo::plugin::PluginManifest manifest;
    manifest.id = "owo.plugin.example";
    manifest.version = "1.0.0";
    manifest.permissions = {"input.context", "clipboard.read"};
    const std::string inventory(64, 'a');
    const std::string certificate(64, 'b');
    const auto authorization = owo::plugin::make_plugin_authorization(
        manifest, inventory, certificate, {"input.context", "clipboard.read"});
    if (!authorization.ok || authorization.value.granted_permissions !=
            std::vector<std::string>({"clipboard.read", "input.context"})) return 12;
    const auto record = owo::plugin::serialize_plugin_authorization(authorization.value);
    const auto parsed = owo::plugin::parse_plugin_authorization(record);
    if (!parsed.ok || owo::plugin::serialize_plugin_authorization(parsed.value) != record) return 13;
    if (!owo::plugin::is_plugin_permission_granted(
            parsed.value, manifest, inventory, certificate, "input.context"))
        return 14;
    auto changed_manifest = manifest;
    changed_manifest.version = "2.0.0";
    if (owo::plugin::is_plugin_permission_granted(
            parsed.value, changed_manifest, inventory, certificate, "input.context") ||
        owo::plugin::is_plugin_permission_granted(
            parsed.value, manifest, std::string(64, 'c'), certificate,
            "input.context") ||
        owo::plugin::is_plugin_permission_granted(
            parsed.value, manifest, inventory, certificate, "input.commit"))
        return 15;
    if (owo::plugin::make_plugin_authorization(
            manifest, inventory, certificate, {"input.commit"}).ok ||
        owo::plugin::make_plugin_authorization(
            manifest, inventory, certificate, {"input.context", "input.context"}).ok)
        return 16;

    auto tampered = record;
    const auto permission_position = tampered.find("clipboard.read,input.context");
    tampered.replace(permission_position, std::string("clipboard.read,input.context").size(),
                     "input.context,clipboard.read");
    if (owo::plugin::parse_plugin_authorization(tampered).ok) return 17;
    tampered = record;
    tampered.replace(tampered.find("input.context"), std::string("input.context").size(), "network");
    if (owo::plugin::parse_plugin_authorization(tampered).ok) return 18;
    tampered = record;
    const auto version_line = tampered.find("version=1.0.0\n");
    tampered.replace(version_line, std::string("version=1.0.0\n").size(),
                     "version=1.0.0\nunknown=true\n");
    if (owo::plugin::parse_plugin_authorization(tampered).ok) return 19;

    const auto deny_all = owo::plugin::make_plugin_authorization(
        manifest, inventory, certificate, {});
    if (!deny_all.ok || !owo::plugin::parse_plugin_authorization(
            owo::plugin::serialize_plugin_authorization(deny_all.value)).ok ||
        owo::plugin::is_plugin_permission_granted(
            deny_all.value, manifest, inventory, certificate, "clipboard.read")) return 20;
    auto forged = parsed.value;
    forged.granted_permissions.push_back("input.replace");
    std::sort(forged.granted_permissions.begin(), forged.granted_permissions.end());
    if (owo::plugin::is_plugin_permission_granted(
            forged, manifest, inventory, certificate, "input.replace")) return 21;

    auto full_trust_manifest = manifest;
    full_trust_manifest.permissions = {"system.full_trust", "ui.desktop_pet"};
    if (owo::plugin::make_plugin_authorization(
            full_trust_manifest, inventory, certificate,
            full_trust_manifest.permissions).ok) return 24;
    const owo::plugin::PluginAuthorizationContext consent{
        owo::plugin::PluginTrustTier::unverified_package,
        owo::plugin::kPluginRiskDisclaimerVersion, true};
    const auto high_risk = owo::plugin::make_plugin_authorization(
        full_trust_manifest, inventory, std::string(64, '0'),
        full_trust_manifest.permissions, consent);
    const auto high_risk_record = high_risk.ok
        ? owo::plugin::serialize_plugin_authorization(high_risk.value) : std::string{};
    const auto parsed_high_risk = owo::plugin::parse_plugin_authorization(high_risk_record);
    if (!parsed_high_risk.ok || parsed_high_risk.value.schema_version != 2 ||
        !parsed_high_risk.value.context.informed_consent ||
        parsed_high_risk.value.context.trust_tier !=
            owo::plugin::PluginTrustTier::unverified_package ||
        !owo::plugin::is_plugin_permission_granted(
            parsed_high_risk.value, full_trust_manifest, inventory,
            std::string(64, '0'), "system.full_trust")) return 25;
    auto stale_disclaimer = high_risk_record;
    stale_disclaimer.replace(stale_disclaimer.find("disclaimer_version=1"),
                             std::string("disclaimer_version=1").size(),
                             "disclaimer_version=0");
    if (owo::plugin::parse_plugin_authorization(stale_disclaimer).ok) return 26;
    const std::string legacy = "schema_version=1\nplugin_id=owo.plugin.example\n"
        "version=1.0.0\ninventory_sha256=" + inventory +
        "\npublisher_certificate_sha256=" + certificate +
        "\ngranted_permissions=clipboard.read\n";
    const auto parsed_legacy = owo::plugin::parse_plugin_authorization(legacy);
    if (!parsed_legacy.ok || parsed_legacy.value.schema_version != 1 ||
        owo::plugin::serialize_plugin_authorization(parsed_legacy.value) != legacy) return 27;
    return 0;
}

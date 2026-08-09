#include "owo/plugin/plugin_authorization_store.h"
#include "owo/plugin/plugin_host.h"
#include "owo/plugin/plugin_store.h"

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

namespace {

bool write_file(const std::filesystem::path& path, const std::string_view contents) {
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(contents.data(), static_cast<std::streamsize>(contents.size()));
    return static_cast<bool>(output);
}

std::string read_file(const std::filesystem::path& path) {
    std::ifstream input(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(input), std::istreambuf_iterator<char>()};
}

}  // namespace

int main(const int argc, char** argv) {
    if (argc != 3) return 1;
    HANDLE current_token = nullptr;
    DWORD current_is_appcontainer = 0;
    DWORD current_returned = 0;
    const bool token_opened = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY,
                                               &current_token) != FALSE;
    const bool current_token_ok = token_opened &&
        GetTokenInformation(current_token, TokenIsAppContainer, &current_is_appcontainer,
                            sizeof(current_is_appcontainer), &current_returned) != FALSE;
    const bool current_is_restricted = token_opened && IsTokenRestricted(current_token) != FALSE;
    if (current_token != nullptr) CloseHandle(current_token);
    if (!current_token_ok) return 2;
    if (current_is_appcontainer != 0 || current_is_restricted) return 77;
    const auto suffix = std::to_string(GetCurrentProcessId()) + "-" +
                        std::to_string(GetTickCount64());
    const auto plugin_id = "owo.plugin.full-trust-test-" + suffix;
    const std::filesystem::path root(
        std::filesystem::path(argv[1]).native() +
        std::wstring(suffix.begin(), suffix.end()));
    const std::filesystem::path executable(argv[2]);
    std::error_code error;
    std::filesystem::remove_all(root, error);
    if (error || !std::filesystem::is_regular_file(executable)) return 3;
    const auto finish = [&](const int code) {
        std::filesystem::remove_all(root, error);
        return !error && !std::filesystem::exists(root) ? code : 90;
    };
    const auto initialized = owo::plugin::initialize_plugin_store(root);
    if (!initialized.ok) return finish(3);
    const auto staging = root / L"staging" / L".full-trust";
    std::filesystem::create_directories(staging / L"bin", error);
    if (error || !CopyFileW(executable.c_str(),
            (staging / L"bin" / L"example-process-plugin.exe").c_str(), TRUE))
        return finish(4);
    const std::string manifest = "{\"id\":\"" + plugin_id +
        "\",\"name\":\"Full Trust Test\",\"version\":\"1.0.0\","
        "\"api_version\":1,\"runtime\":\"process\","
        "\"entry\":\"bin/example-process-plugin.exe\","
        "\"permissions\":[\"system.full_trust\",\"ui.desktop_pet\"],"
        "\"network\":false,\"config_schema\":\"config.schema.json\"}";
    if (!write_file(staging / L"manifest.json", manifest) ||
        !write_file(staging / L"config.schema.json", "{}")) return finish(5);
    const auto published = owo::plugin::publish_staged_plugin(
        root, staging, std::string(64, 'a'), std::string(64, '0'));
    if (!published.ok || !published.activated) return finish(6);
    const auto denied = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(1));
    if (denied || denied.diagnostic.find("full-trust plugin permission") == std::string::npos)
        return finish(7);

    const owo::plugin::PluginAuthorizationContext context{
        owo::plugin::PluginTrustTier::unverified_package,
        owo::plugin::kPluginRiskDisclaimerVersion, true};
    const auto authorization = owo::plugin::make_plugin_authorization(
        published.manifest, std::string(64, 'a'), std::string(64, '0'),
        published.manifest.permissions, context);
    if (!authorization.ok ||
        !owo::plugin::save_plugin_authorization(root, authorization.value).ok)
        return finish(8);
    auto launched = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(5));
    if (!launched) {
        std::cerr << launched.diagnostic << "; client diagnostic: "
                  << read_file(root / L"data" / std::filesystem::path(plugin_id) /
                               L"connect-error.txt") << '\n';
        return finish(9);
    }
    HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE,
                                 launched.session.process_id());
    HANDLE token = nullptr;
    DWORD is_appcontainer = 1;
    DWORD returned = 0;
    BOOL in_job = FALSE;
    const bool boundary = process != nullptr &&
        OpenProcessToken(process, TOKEN_QUERY, &token) != FALSE &&
        GetTokenInformation(token, TokenIsAppContainer, &is_appcontainer,
                            sizeof(is_appcontainer), &returned) != FALSE &&
        IsProcessInJob(process, nullptr, &in_job) != FALSE &&
        is_appcontainer == 0 && in_job != FALSE;
    if (token != nullptr) CloseHandle(token);
    if (process != nullptr) CloseHandle(process);
    owo::plugin::PluginInvokeRequest request;
    request.service = "example.echo.v1";
    request.payload = "full-trust-ok";
    request.timeout = std::chrono::seconds(2);
    const auto echoed = launched.session.invoke(std::move(request));
    const auto stopped = launched.session.shutdown(std::chrono::seconds(2));
    if (!boundary || !echoed.ok || echoed.payload != "full-trust-ok" || !stopped.ok)
        return finish(10);

    const auto revoked = owo::plugin::make_plugin_authorization(
        published.manifest, std::string(64, 'a'), std::string(64, '0'), {}, context);
    if (!revoked.ok || !owo::plugin::save_plugin_authorization(root, revoked.value).ok ||
        owo::plugin::launch_active_plugin(root, plugin_id, std::chrono::seconds(1)))
        return finish(11);
    return finish(0);
}

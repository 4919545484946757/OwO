#include "owo/plugin/plugin_authorization_store.h"
#include "owo/plugin/plugin_host.h"
#include "owo/plugin/plugin_sandbox.h"
#include "owo/plugin/plugin_store.h"

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <iterator>
#include <stop_token>
#include <string>
#include <thread>
#include <utility>

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

std::string manifest_json(const std::string_view plugin_id,
                          const bool full_trust = false) {
    return "{\"id\":\"" + std::string(plugin_id) +
        "\",\"name\":\"Runtime Test\",\"version\":\"1.0.0\","
        "\"api_version\":1,\"runtime\":\"process\","
        "\"entry\":\"bin/example-process-plugin.exe\","
        "\"permissions\":" + std::string(full_trust
            ? "[\"system.full_trust\",\"ui.desktop_pet\"]"
            : "[\"input.context\"]") + ","
        "\"network\":false,\"config_schema\":\"config.schema.json\"}";
}

bool wait_for_file(const std::filesystem::path& path,
                   const std::chrono::milliseconds timeout) {
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    while (std::chrono::steady_clock::now() < deadline) {
        if (std::filesystem::is_regular_file(path)) return true;
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    }
    return std::filesystem::is_regular_file(path);
}

}  // namespace

int main(const int argc, char** argv) {
    if (argc != 3) return 1;
    const std::filesystem::path probe(argv[2]);
    const auto unique_suffix = std::to_string(GetCurrentProcessId()) + "-" +
        std::to_string(GetTickCount64());
    const auto plugin_id = "owo.plugin.runtime-test-" + unique_suffix;
    const std::filesystem::path root =
        std::filesystem::path(argv[1]).native() + L"." +
        std::wstring(unique_suffix.begin(), unique_suffix.end());
    std::error_code error;
    std::filesystem::remove_all(root, error);
    if (error || !std::filesystem::is_regular_file(probe)) return 2;

    const auto finish = [&](const int code) {
        std::filesystem::remove_all(root, error);
        const auto profile = owo::plugin::prepare_plugin_sandbox_profile(plugin_id);
        if (!profile.ok) return 90;
        const auto deleted = owo::plugin::delete_plugin_sandbox_profile(profile.profile_name);
        return deleted.ok && !std::filesystem::exists(root) ? code : 91;
    };

    const auto initialized = owo::plugin::initialize_plugin_store(root);
    if (!initialized.ok) return finish(3);
    const auto staging = root / L"staging" / L".runtime-v1";
    std::filesystem::create_directories(staging / L"bin", error);
    if (error || !CopyFileW(probe.c_str(),
                            (staging / L"bin" / L"example-process-plugin.exe").c_str(),
                            TRUE)) return finish(4);
    if (!write_file(staging / L"manifest.json", manifest_json(plugin_id)) ||
        !write_file(staging / L"config.schema.json", "{}")) return finish(5);
    const auto published = owo::plugin::publish_staged_plugin(
        root, staging, std::string(64, 'a'), std::string(64, 'b'));
    if (!published.ok || !published.activated) return finish(6);
    const auto active = owo::plugin::query_active_plugin_version(root, plugin_id);
    if (!active.ok || active.manifest.version != "1.0.0") return finish(7);

    SetEnvironmentVariableW(L"OWO_TEST_SECRET", L"must-not-leak");
    auto launched = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(5));
    SetEnvironmentVariableW(L"OWO_TEST_SECRET", nullptr);
    if (!launched) {
        std::cerr << "PluginHost launch failed: " << launched.diagnostic << '\n';
        return finish(8);
    }
    if (!launched.session.valid() || launched.session.plugin_id() != plugin_id ||
        launched.session.version() != "1.0.0" || launched.session.process_id() == 0 ||
        launched.installed_path != published.installed_path ||
        launched.data_path != root / L"data" / std::filesystem::path(plugin_id))
        return finish(9);
    if (read_file(launched.data_path / L"probe-data.txt") != "sandbox-data-ok\n" ||
        std::filesystem::exists(launched.installed_path / L"bin" /
                                L"should-not-write.tmp")) return finish(10);

    owo::plugin::PluginInvokeRequest sensitive;
    sensitive.service = "example.echo.v1";
    sensitive.payload = "authorized payload";
    sensitive.required_permissions = {"input.context"};
    sensitive.timeout = std::chrono::seconds(2);
    const auto no_user_action = launched.session.invoke(sensitive);
    if (no_user_action.status != owo::plugin::PluginStatus::permission_denied ||
        no_user_action.forced_termination || !launched.session.valid()) return finish(16);
    sensitive.user_initiated = true;
    const auto no_grant = launched.session.invoke(sensitive);
    if (no_grant.status != owo::plugin::PluginStatus::permission_denied ||
        no_grant.forced_termination || !launched.session.valid()) return finish(17);
    const auto authorization = owo::plugin::make_plugin_authorization(
        active.manifest, active.inventory_sha256, active.publisher_certificate_sha256,
        {"input.context"});
    if (!authorization.ok ||
        !owo::plugin::save_plugin_authorization(root, authorization.value).ok)
        return finish(18);
    const auto echoed = launched.session.invoke(sensitive);
    if (!echoed.ok || echoed.status != owo::plugin::PluginStatus::success ||
        echoed.payload != "authorized payload" || echoed.request_id == 0)
        return finish(19);
    auto stale_session = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(5));
    if (!stale_session) return finish(34);
    const auto deactivated = owo::plugin::deactivate_plugin(root, plugin_id, "1.0.0");
    if (!deactivated.ok) return finish(35);
    owo::plugin::PluginInvokeRequest stale_request;
    stale_request.service = "example.echo.v1";
    stale_request.payload = "must-not-dispatch-after-disable";
    stale_request.timeout = std::chrono::seconds(1);
    const auto stale_result = stale_session.session.invoke(std::move(stale_request));
    if (stale_result.status != owo::plugin::PluginStatus::permission_denied ||
        !stale_result.forced_termination || stale_session.session.valid()) return finish(36);
    const auto reactivated = owo::plugin::activate_installed_plugin_version(
        root, plugin_id, "1.0.0");
    if (!reactivated.ok) return finish(37);
    auto duplicate_permission = sensitive;
    duplicate_permission.required_permissions = {"input.context", "input.context"};
    const auto invalid_permissions = launched.session.invoke(std::move(duplicate_permission));
    if (invalid_permissions.status != owo::plugin::PluginStatus::invalid_request ||
        invalid_permissions.forced_termination || !launched.session.valid()) return finish(32);
    std::stop_source already_cancelled;
    already_cancelled.request_stop();
    owo::plugin::PluginInvokeRequest cancelled_before_dispatch;
    cancelled_before_dispatch.service = "example.echo.v1";
    cancelled_before_dispatch.payload = "must-not-dispatch";
    cancelled_before_dispatch.timeout = std::chrono::seconds(1);
    cancelled_before_dispatch.cancellation = already_cancelled.get_token();
    const auto pre_cancelled = launched.session.invoke(std::move(cancelled_before_dispatch));
    if (pre_cancelled.status != owo::plugin::PluginStatus::cancelled ||
        pre_cancelled.request_id != 0 || pre_cancelled.forced_termination ||
        !launched.session.valid()) return finish(33);
    auto unversioned = sensitive;
    unversioned.service = "example.echo";
    const auto invalid_service = launched.session.invoke(std::move(unversioned));
    if (invalid_service.status != owo::plugin::PluginStatus::invalid_request ||
        invalid_service.forced_termination || !launched.session.valid()) return finish(20);

    const auto active_marker = launched.data_path / L"invoke-active.txt";
    std::filesystem::remove(active_marker, error);
    error.clear();
    owo::plugin::PluginInvokeResult delayed;
    std::thread delay_thread([&] {
        owo::plugin::PluginInvokeRequest delay;
        delay.service = "example.delay.v1";
        delay.payload = "250";
        delay.timeout = std::chrono::seconds(2);
        delayed = launched.session.invoke(std::move(delay));
    });
    if (!wait_for_file(active_marker, std::chrono::seconds(1))) {
        delay_thread.join();
        return finish(21);
    }
    owo::plugin::PluginInvokeRequest competing;
    competing.service = "example.echo.v1";
    competing.payload = "must-not-dispatch";
    competing.timeout = std::chrono::seconds(1);
    const auto busy = launched.session.invoke(std::move(competing));
    const auto busy_shutdown = launched.session.shutdown(std::chrono::milliseconds(100));
    delay_thread.join();
    if (busy.status != owo::plugin::PluginStatus::plugin_error ||
        busy.diagnostic != "plugin invocation concurrency limit reached" ||
        busy.forced_termination || busy_shutdown.ok || busy_shutdown.forced_termination ||
        busy_shutdown.diagnostic != "cannot shut down while a plugin invocation is active" ||
        !delayed.ok || delayed.payload != "delayed:250" ||
        !launched.session.valid()) return finish(22);

    std::filesystem::remove(active_marker, error);
    error.clear();
    std::stop_source cancellation;
    owo::plugin::PluginInvokeResult cancelled;
    std::thread cancel_thread([&] {
        owo::plugin::PluginInvokeRequest delay;
        delay.service = "example.delay.v1";
        delay.payload = "5000";
        delay.timeout = std::chrono::seconds(5);
        delay.cancellation = cancellation.get_token();
        cancelled = launched.session.invoke(std::move(delay));
    });
    if (!wait_for_file(active_marker, std::chrono::seconds(1))) {
        cancellation.request_stop();
        cancel_thread.join();
        return finish(23);
    }
    cancellation.request_stop();
    cancel_thread.join();
    if (cancelled.ok || cancelled.status != owo::plugin::PluginStatus::cancelled ||
        cancelled.forced_termination || !launched.session.valid()) return finish(24);

    const auto shutdown = launched.session.shutdown(std::chrono::seconds(2));
    if (!shutdown.ok || shutdown.forced_termination || launched.session.valid())
        return finish(11);

    auto forced_cancel_session = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(5));
    if (!forced_cancel_session) return finish(29);
    const auto forced_cancel_marker = forced_cancel_session.data_path / L"invoke-active.txt";
    std::filesystem::remove(forced_cancel_marker, error);
    error.clear();
    std::stop_source forced_cancellation;
    owo::plugin::PluginInvokeResult forced_cancelled;
    std::thread forced_cancel_thread([&] {
        owo::plugin::PluginInvokeRequest hanging_cancel;
        hanging_cancel.service = "example.hang.v1";
        hanging_cancel.timeout = std::chrono::seconds(5);
        hanging_cancel.cancellation = forced_cancellation.get_token();
        forced_cancelled = forced_cancel_session.session.invoke(std::move(hanging_cancel));
    });
    if (!wait_for_file(forced_cancel_marker, std::chrono::seconds(1))) {
        forced_cancellation.request_stop();
        forced_cancel_thread.join();
        return finish(30);
    }
    forced_cancellation.request_stop();
    forced_cancel_thread.join();
    if (forced_cancelled.status != owo::plugin::PluginStatus::cancelled ||
        !forced_cancelled.forced_termination || forced_cancel_session.session.valid())
        return finish(31);

    auto timed_session = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(5));
    if (!timed_session) return finish(25);
    owo::plugin::PluginInvokeRequest hanging;
    hanging.service = "example.hang.v1";
    hanging.timeout = std::chrono::milliseconds(100);
    const auto timed_out = timed_session.session.invoke(std::move(hanging));
    if (timed_out.status != owo::plugin::PluginStatus::timeout ||
        !timed_out.forced_termination || timed_session.session.valid()) {
        std::cerr << "timeout result mismatch: status="
                  << static_cast<int>(timed_out.status)
                  << " forced=" << timed_out.forced_termination
                  << " valid=" << timed_session.session.valid()
                  << " diagnostic=" << timed_out.diagnostic << '\n';
        return finish(26);
    }

    auto disconnected_session = owo::plugin::launch_active_plugin(
        root, plugin_id, std::chrono::seconds(5));
    if (!disconnected_session) return finish(27);
    owo::plugin::PluginInvokeRequest disconnecting;
    disconnecting.service = "example.disconnect.v1";
    disconnecting.timeout = std::chrono::seconds(2);
    const auto disconnected = disconnected_session.session.invoke(std::move(disconnecting));
    if (disconnected.status != owo::plugin::PluginStatus::plugin_error ||
        !disconnected.forced_termination || disconnected_session.session.valid())
        return finish(28);

    DWORD forced_pid = 0;
    HANDLE forced_process = nullptr;
    {
        auto forced = owo::plugin::launch_active_plugin(
            root, plugin_id, std::chrono::seconds(5));
        if (!forced) {
            std::cerr << "second PluginHost launch failed: " << forced.diagnostic << '\n';
            return finish(12);
        }
        forced_pid = forced.session.process_id();
        forced_process = OpenProcess(SYNCHRONIZE, FALSE, forced_pid);
        if (forced_process == nullptr) return finish(13);
    }
    const auto forced_wait = WaitForSingleObject(forced_process, 3000);
    CloseHandle(forced_process);
    if (forced_wait != WAIT_OBJECT_0) return finish(14);
    if (owo::plugin::launch_active_plugin(
            root, "../escape", std::chrono::seconds(1))) return finish(15);

    const auto full_trust_id = "owo.plugin.full-trust-test-" + unique_suffix;
    const auto full_staging = root / L"staging" / L".runtime-full-trust";
    std::filesystem::create_directories(full_staging / L"bin", error);
    if (error || !CopyFileW(probe.c_str(),
                            (full_staging / L"bin" / L"example-process-plugin.exe").c_str(),
                            TRUE) ||
        !write_file(full_staging / L"manifest.json", manifest_json(full_trust_id, true)) ||
        !write_file(full_staging / L"config.schema.json", "{}")) return finish(38);
    const auto full_published = owo::plugin::publish_staged_plugin(
        root, full_staging, std::string(64, 'c'), std::string(64, '0'));
    if (!full_published.ok || !full_published.activated ||
        owo::plugin::launch_active_plugin(root, full_trust_id, std::chrono::seconds(1)))
        return finish(39);
    const owo::plugin::PluginAuthorizationContext full_consent{
        owo::plugin::PluginTrustTier::unverified_package,
        owo::plugin::kPluginRiskDisclaimerVersion, true};
    const auto full_authorization = owo::plugin::make_plugin_authorization(
        full_published.manifest, std::string(64, 'c'), std::string(64, '0'),
        full_published.manifest.permissions, full_consent);
    if (!full_authorization.ok ||
        !owo::plugin::save_plugin_authorization(root, full_authorization.value).ok)
        return finish(40);
    auto full_launched = owo::plugin::launch_active_plugin(
        root, full_trust_id, std::chrono::seconds(5));
    if (!full_launched) {
        std::cerr << "Full-trust PluginHost launch failed: "
                  << full_launched.diagnostic << '\n';
        return finish(41);
    }
    HANDLE full_process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE,
                                      full_launched.session.process_id());
    HANDLE full_token = nullptr;
    DWORD is_appcontainer = 1;
    DWORD returned = 0;
    BOOL in_job = FALSE;
    const bool full_boundary = full_process != nullptr &&
        OpenProcessToken(full_process, TOKEN_QUERY, &full_token) != FALSE &&
        GetTokenInformation(full_token, TokenIsAppContainer, &is_appcontainer,
                            sizeof(is_appcontainer), &returned) != FALSE &&
        IsProcessInJob(full_process, nullptr, &in_job) != FALSE &&
        is_appcontainer == 0 && in_job != FALSE;
    if (full_token != nullptr) CloseHandle(full_token);
    if (full_process != nullptr) CloseHandle(full_process);
    owo::plugin::PluginInvokeRequest full_echo;
    full_echo.service = "example.echo.v1";
    full_echo.payload = "full-trust-ok";
    full_echo.timeout = std::chrono::seconds(2);
    const auto full_echoed = full_launched.session.invoke(std::move(full_echo));
    const auto full_shutdown = full_launched.session.shutdown(std::chrono::seconds(2));
    if (!full_boundary || !full_echoed.ok || full_echoed.payload != "full-trust-ok" ||
        !full_shutdown.ok) return finish(42);
    return finish(0);
}

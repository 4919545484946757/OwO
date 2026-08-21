#include "owo/core/plugin_executor.h"
#include "owo/plugin/plugin_authorization_store.h"
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
#include <sstream>
#include <string>
#include <thread>
#include <vector>

namespace {

bool write_file(const std::filesystem::path& path, const std::string_view contents) {
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(contents.data(), static_cast<std::streamsize>(contents.size()));
    return static_cast<bool>(output);
}

std::string manifest_json(const std::string_view plugin_id) {
    return "{\"id\":\"" + std::string(plugin_id) +
        "\",\"name\":\"Core Executor Test\",\"version\":\"1.0.0\"," 
        "\"api_version\":1,\"runtime\":\"process\"," 
        "\"entry\":\"bin/example-process-plugin.exe\"," 
        "\"permissions\":[\"input.context\"],\"network\":false," 
        "\"config_schema\":\"config.schema.json\"}";
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

owo::core::PluginExecutionRequest request(
    const std::string& plugin_id, std::string service, std::string payload,
    const owo::core::PluginCallSource source = owo::core::PluginCallSource::core_background) {
    owo::core::PluginExecutionRequest value;
    value.plugin_id = plugin_id;
    value.service = std::move(service);
    value.payload = std::move(payload);
    value.source = source;
    value.timeout = std::chrono::seconds(5);
    return value;
}

}  // namespace

int main(const int argc, char** argv) {
    using namespace std::chrono_literals;
    if (argc != 3) return 1;
    const std::filesystem::path probe(argv[2]);
    const auto suffix = std::to_string(GetCurrentProcessId()) + "-" +
        std::to_string(GetTickCount64());
    const auto plugin_id = "owo.plugin.core-executor-test-" + suffix;
    const std::filesystem::path root = std::filesystem::path(argv[1]).native() + L"." +
        std::wstring(suffix.begin(), suffix.end());
    std::error_code error;
    std::filesystem::remove_all(root, error);
    if (error || !std::filesystem::is_regular_file(probe)) return 2;
    const auto finish = [&](const int code) {
        std::filesystem::remove_all(root, error);
        const auto profile = owo::plugin::prepare_plugin_sandbox_profile(plugin_id);
        if (!profile.ok) {
            std::cerr << "profile cleanup preparation failed after outcome " << code
                      << ": " << profile.diagnostic << '\n';
            return 90;
        }
        const auto deleted = owo::plugin::delete_plugin_sandbox_profile(profile.profile_name);
        if (!deleted.ok || std::filesystem::exists(root)) {
            std::cerr << "fixture cleanup failed after outcome " << code
                      << ": " << deleted.diagnostic << '\n';
            return 91;
        }
        if (code != 0) std::cerr << "core plugin executor test outcome=" << code << '\n';
        return code;
    };

    const auto initialized = owo::plugin::initialize_plugin_store(root);
    if (!initialized.ok) {
        std::cerr << "store initialization failed: " << initialized.diagnostic << '\n';
        return finish(3);
    }
    const auto staging = root / L"staging" / L".executor-v1";
    std::filesystem::create_directories(staging / L"bin", error);
    if (error || !CopyFileW(probe.c_str(),
            (staging / L"bin" / L"example-process-plugin.exe").c_str(), TRUE) ||
        !write_file(staging / L"manifest.json", manifest_json(plugin_id)) ||
        !write_file(staging / L"config.schema.json", "{}")) {
        std::cerr << "staging fixture creation failed: Windows error " << GetLastError() << '\n';
        return finish(4);
    }
    const auto published = owo::plugin::publish_staged_plugin(
        root, staging, std::string(64, 'a'), std::string(64, 'b'));
    const auto active = owo::plugin::query_active_plugin_version(root, plugin_id);
    if (!published.ok || !active.ok) {
        std::cerr << "publish fixture failed: " << published.diagnostic << " / "
                  << active.diagnostic << '\n';
        return finish(5);
    }
    const auto authorization = owo::plugin::make_plugin_authorization(
        active.manifest, active.inventory_sha256, active.publisher_certificate_sha256,
        {"input.context"});
    if (!authorization.ok ||
        !owo::plugin::save_plugin_authorization(root, authorization.value).ok)
    {
        std::cerr << "authorization fixture creation failed\n";
        return finish(6);
    }

    std::ostringstream audit;
    auto* original_log = std::clog.rdbuf(audit.rdbuf());
    int outcome = 0;
    {
        owo::core::PluginExecutor executor(root);
        auto tsf = request(plugin_id, "example.echo.v1", "never-dispatch",
                           owo::core::PluginCallSource::tsf_input);
        const auto tsf_result = executor.submit(std::move(tsf)).completion.get();
        if (tsf_result.status != owo::core::PluginExecutionStatus::rejected_source)
            outcome = 7;

        auto background_sensitive = request(
            plugin_id, "example.echo.v1", "never-dispatch",
            owo::core::PluginCallSource::core_background);
        background_sensitive.required_permissions = {"input.context"};
        const auto background_result =
            executor.submit(std::move(background_sensitive)).completion.get();
        if (outcome == 0 && background_result.status !=
                owo::core::PluginExecutionStatus::rejected_source) outcome = 8;

        auto implicit_sensitive = request(
            plugin_id, "owo.ui.overlay.v1", "{}",
            owo::core::PluginCallSource::core_background);
        const auto implicit_sensitive_result =
            executor.submit(std::move(implicit_sensitive)).completion.get();
        if (outcome == 0 && implicit_sensitive_result.status !=
                owo::core::PluginExecutionStatus::rejected_source) outcome = 23;

        auto too_deep = request(plugin_id, "example.echo.v1", "never-dispatch");
        too_deep.call_depth = owo::core::kMaximumPluginCallDepth + 1;
        if (outcome == 0 && executor.submit(std::move(too_deep)).completion.get().status !=
                owo::core::PluginExecutionStatus::invalid_request) outcome = 9;

        auto unknown_source = request(plugin_id, "example.echo.v1", "never-dispatch");
        unknown_source.source = static_cast<owo::core::PluginCallSource>(255);
        if (outcome == 0 && executor.submit(std::move(unknown_source)).completion.get().status !=
                owo::core::PluginExecutionStatus::invalid_request) outcome = 22;

        const auto submitted_at = std::chrono::steady_clock::now();
        auto echo_submission = executor.submit(request(
            plugin_id, "example.echo.v1", "core-secret-payload"));
        if (std::chrono::steady_clock::now() - submitted_at > 100ms) outcome = 10;
        const auto echoed = echo_submission.completion.get();
        if (outcome == 0 && (!echoed.ok || echoed.payload != "core-secret-payload" ||
            echoed.call_id != echo_submission.call_id)) {
            std::cerr << "background echo failed: " << echoed.diagnostic << '\n';
            outcome = 11;
        }

        auto sensitive = request(plugin_id, "example.echo.v1", "trusted-result",
                                 owo::core::PluginCallSource::trusted_user_action);
        sensitive.required_permissions = {"input.context"};
        const auto sensitive_result = executor.submit(std::move(sensitive)).completion.get();
        if (outcome == 0 && (!sensitive_result.ok ||
            sensitive_result.payload != "trusted-result")) outcome = 12;

        auto plugin_service = request(plugin_id, "example.echo.v1", "nested-result",
                                      owo::core::PluginCallSource::plugin_service);
        plugin_service.call_depth = 1;
        const auto nested = executor.submit(std::move(plugin_service)).completion.get();
        if (outcome == 0 && (!nested.ok || nested.payload != "nested-result")) outcome = 13;

        const auto marker = root / L"data" / std::filesystem::path(plugin_id) /
                            L"invoke-active.txt";
        std::filesystem::remove(marker, error);
        std::stop_source cancellation;
        auto cancellable = request(plugin_id, "example.delay.v1", "5000");
        cancellable.cancellation = cancellation.get_token();
        auto cancelling = executor.submit(std::move(cancellable));
        if (outcome == 0 && !wait_for_file(marker, 2s)) outcome = 14;
        cancellation.request_stop();
        const auto cancelled = cancelling.completion.get();
        if (outcome == 0 && (cancelled.status !=
                owo::core::PluginExecutionStatus::cancelled ||
                cancelled.forced_termination)) outcome = 15;

        std::filesystem::remove(marker, error);
        auto delay = executor.submit(request(plugin_id, "example.delay.v1", "1000"));
        if (outcome == 0 && !wait_for_file(marker, 2s)) outcome = 16;
        std::vector<owo::core::PluginExecutionSubmission> queued;
        for (std::size_t index = 0; index < owo::core::kMaximumQueuedPluginCalls; ++index)
            queued.push_back(executor.submit(request(
                plugin_id, "example.echo.v1", "queued" + std::to_string(index))));
        const auto overflow = executor.submit(request(
            plugin_id, "example.echo.v1", "must-not-queue")).completion.get();
        if (outcome == 0 && overflow.status != owo::core::PluginExecutionStatus::queue_full)
            outcome = 17;
        const auto delayed = delay.completion.get();
        if (outcome == 0 && (!delayed.ok || delayed.payload != "delayed:1000")) outcome = 18;
        for (auto& item : queued) {
            const auto completed = item.completion.get();
            if (outcome == 0 && !completed.ok) outcome = 19;
        }
        if (outcome == 0 && executor.queued_call_count() != 0) outcome = 20;
    }
    std::clog.rdbuf(original_log);
    if (outcome == 0 && (audit.str().find("core-secret-payload") != std::string::npos ||
        audit.str().find("\"source\":\"tsf_input\"") == std::string::npos ||
        audit.str().find("\"status\":\"queue_full\"") == std::string::npos)) outcome = 21;
    return finish(outcome);
}

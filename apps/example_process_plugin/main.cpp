#include "owo/plugin/plugin_pipe.h"

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>

#include <algorithm>
#include <atomic>
#include <charconv>
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <thread>
#include <utility>
#include <vector>

namespace {

struct Arguments {
    std::wstring pipe_name;
    std::string plugin_id;
    std::filesystem::path data_path;
};

bool narrow_ascii(const std::wstring_view input, std::string& output) {
    if (std::any_of(input.begin(), input.end(),
                    [](const wchar_t value) { return value > 0x7f; })) return false;
    output.assign(input.size(), '\0');
    std::transform(input.begin(), input.end(), output.begin(),
                   [](const wchar_t value) { return static_cast<char>(value); });
    return !output.empty();
}

bool parse_arguments(const int argc, wchar_t** argv, Arguments& result) {
    if (argc != 7 || std::wstring_view(argv[1]) != L"--owo-plugin-pipe" ||
        std::wstring_view(argv[3]) != L"--owo-plugin-id" ||
        std::wstring_view(argv[5]) != L"--owo-plugin-data") return false;
    result.pipe_name = argv[2];
    result.data_path = argv[6];
    return !result.pipe_name.empty() && !result.data_path.empty() &&
           narrow_ascii(argv[4], result.plugin_id);
}

bool full_trust_is_explicit() {
    wchar_t value[2]{};
    return GetEnvironmentVariableW(L"OWO_PLUGIN_FULL_TRUST", value,
                                   static_cast<DWORD>(std::size(value))) == 1 &&
           value[0] == L'1';
}

bool sandbox_is_active() {
    HANDLE token = nullptr;
    DWORD is_appcontainer = 0;
    DWORD returned = 0;
    const bool queried = OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token) != FALSE &&
        GetTokenInformation(token, TokenIsAppContainer, &is_appcontainer,
                            sizeof(is_appcontainer), &returned) != FALSE;
    if (token != nullptr) CloseHandle(token);
    BOOL in_job = FALSE;
    return queried && (is_appcontainer != 0 || full_trust_is_explicit()) &&
           IsProcessInJob(GetCurrentProcess(), nullptr, &in_job) != FALSE && in_job != FALSE;
}

bool sensitive_environment_is_absent() {
    wchar_t value[2]{};
    SetLastError(ERROR_SUCCESS);
    return GetEnvironmentVariableW(L"OWO_TEST_SECRET", value,
                                   static_cast<DWORD>(std::size(value))) == 0 &&
           GetLastError() == ERROR_ENVVAR_NOT_FOUND;
}

bool installed_directory_is_read_only() {
    wchar_t executable[32768]{};
    const auto length = GetModuleFileNameW(nullptr, executable,
                                           static_cast<DWORD>(std::size(executable)));
    if (length == 0 || length == std::size(executable)) return false;
    const auto probe = std::filesystem::path(executable).parent_path() /
                       L"should-not-write.tmp";
    HANDLE file = CreateFileW(probe.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW,
                              FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return true;
    CloseHandle(file);
    DeleteFileW(probe.c_str());
    return false;
}

bool write_data_probe(const std::filesystem::path& data_path) {
    const auto marker = data_path / L"probe-data.txt";
    HANDLE file = CreateFileW(marker.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                              FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    constexpr std::string_view contents = "sandbox-data-ok\n";
    DWORD written = 0;
    const bool ok = WriteFile(file, contents.data(), static_cast<DWORD>(contents.size()),
                              &written, nullptr) != FALSE && written == contents.size();
    CloseHandle(file);
    return ok;
}

bool write_marker(const std::filesystem::path& data_path, const std::wstring_view name,
                  const std::string_view contents) {
    const auto marker = data_path / std::filesystem::path(name);
    HANDLE file = CreateFileW(marker.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                              FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    DWORD written = 0;
    const bool ok = WriteFile(file, contents.data(), static_cast<DWORD>(contents.size()),
                              &written, nullptr) != FALSE && written == contents.size();
    CloseHandle(file);
    return ok;
}

bool delay_milliseconds(const std::string_view payload, std::chrono::milliseconds& result) {
    unsigned long value = 0;
    const auto parsed = std::from_chars(payload.data(), payload.data() + payload.size(), value);
    if (parsed.ec != std::errc{} || parsed.ptr != payload.data() + payload.size() ||
        value == 0 || value > 10000) return false;
    result = std::chrono::milliseconds(value);
    return true;
}

int run_plugin(const Arguments& arguments) {
    if (!sandbox_is_active()) return 10;
    if (!sensitive_environment_is_absent()) return 11;
    wchar_t environment_data[32768]{};
    const auto environment_length = GetEnvironmentVariableW(
        L"OWO_PLUGIN_DATA", environment_data,
        static_cast<DWORD>(std::size(environment_data)));
    if (environment_length == 0 || environment_length >= std::size(environment_data) ||
        std::filesystem::path(environment_data).lexically_normal() !=
            arguments.data_path.lexically_normal()) return 14;
    if (!full_trust_is_explicit() && !installed_directory_is_read_only()) return 12;
    if (!write_data_probe(arguments.data_path)) return 13;

    auto connected = owo::plugin::connect_plugin_pipe_client(
        arguments.pipe_name, std::chrono::seconds(5));
    if (!connected) {
        write_marker(arguments.data_path, L"connect-error.txt", connected.diagnostic);
        return 20;
    }
    owo::plugin::PluginMessage hello;
    hello.type = owo::plugin::PluginMessageType::hello_request;
    hello.status = owo::plugin::PluginStatus::success;
    hello.request_id = 1;
    hello.plugin_id = arguments.plugin_id;
    if (!owo::plugin::send_plugin_pipe_message(
            connected.pipe, hello, std::chrono::seconds(5)).ok) return 21;
    const auto response = owo::plugin::receive_plugin_pipe_message(
        connected.pipe, std::chrono::seconds(5));
    if (!response.status.ok ||
        response.message.type != owo::plugin::PluginMessageType::hello_response ||
        response.message.request_id != hello.request_id ||
        response.message.plugin_id != arguments.plugin_id ||
        response.message.capabilities !=
            std::vector<std::string>{"cancel.v1", "invoke.v1"}) return 22;

    std::mutex send_mutex;
    std::jthread invocation_worker;
    std::shared_ptr<std::atomic_bool> invocation_cancelled;
    std::atomic_bool invocation_complete{false};
    std::uint64_t invocation_id = 0;
    const auto send = [&](const owo::plugin::PluginMessage& message) {
        std::lock_guard lock(send_mutex);
        return owo::plugin::send_plugin_pipe_message(
            connected.pipe, message, std::chrono::seconds(5)).ok;
    };
    const auto reap_invocation = [&] {
        if (!invocation_complete.load(std::memory_order_acquire)) return;
        if (invocation_worker.joinable()) invocation_worker.join();
        invocation_cancelled.reset();
        invocation_id = 0;
        invocation_complete.store(false, std::memory_order_release);
    };
    const auto response_for = [&](const owo::plugin::PluginMessage& request,
                                  const owo::plugin::PluginStatus status,
                                  std::string payload, std::string diagnostic) {
        owo::plugin::PluginMessage result;
        result.type = owo::plugin::PluginMessageType::invoke_response;
        result.status = status;
        result.request_id = request.request_id;
        result.plugin_id = arguments.plugin_id;
        result.payload = std::move(payload);
        result.diagnostic = std::move(diagnostic);
        return result;
    };

    for (;;) {
        reap_invocation();
        const auto request = owo::plugin::receive_plugin_pipe_message(
            connected.pipe, std::chrono::hours(24));
        if (!request.status.ok) return 23;
        if (request.message.plugin_id != arguments.plugin_id) return 24;
        if (request.message.type == owo::plugin::PluginMessageType::shutdown_request) {
            owo::plugin::PluginMessage acknowledgement;
            acknowledgement.type = owo::plugin::PluginMessageType::acknowledgement;
            acknowledgement.status = owo::plugin::PluginStatus::success;
            acknowledgement.request_id = request.message.request_id;
            acknowledgement.plugin_id = arguments.plugin_id;
            if (!send(acknowledgement)) return 25;
            return 0;
        }
        if (request.message.type == owo::plugin::PluginMessageType::cancel_request) {
            if (request.message.target_request_id != invocation_id) return 26;
            if (invocation_cancelled == nullptr) continue;
            owo::plugin::PluginMessage acknowledgement;
            acknowledgement.type = owo::plugin::PluginMessageType::acknowledgement;
            acknowledgement.status = owo::plugin::PluginStatus::success;
            acknowledgement.request_id = request.message.request_id;
            acknowledgement.plugin_id = arguments.plugin_id;
            if (!send(acknowledgement)) return 27;
            invocation_cancelled->store(true, std::memory_order_release);
            continue;
        }
        if (request.message.type != owo::plugin::PluginMessageType::invoke_request) return 28;
        reap_invocation();
        if (invocation_id != 0) {
            auto busy = response_for(request.message, owo::plugin::PluginStatus::plugin_error,
                                     {}, "sample plugin invocation is already active");
            if (!send(busy)) return 29;
            continue;
        }
        if (request.message.service == "example.echo.v1") {
            auto echoed = response_for(request.message, owo::plugin::PluginStatus::success,
                                       request.message.payload, {});
            if (!send(echoed)) return 30;
            continue;
        }
        if (request.message.service == "example.disconnect.v1") return 77;
        if (request.message.service == "example.hang.v1") {
            if (!write_marker(arguments.data_path, L"invoke-active.txt",
                              std::to_string(request.message.request_id))) return 33;
            invocation_id = request.message.request_id;
            continue;
        }
        if (request.message.service == "example.delay.v1") {
            std::chrono::milliseconds delay;
            if (!delay_milliseconds(request.message.payload, delay) ||
                !write_marker(arguments.data_path, L"invoke-active.txt",
                              std::to_string(request.message.request_id))) {
                auto invalid = response_for(request.message,
                    owo::plugin::PluginStatus::invalid_request, {}, "invalid delay request");
                if (!send(invalid)) return 31;
                continue;
            }
            invocation_id = request.message.request_id;
            invocation_cancelled = std::make_shared<std::atomic_bool>(false);
            invocation_complete.store(false, std::memory_order_release);
            const auto cancellation = invocation_cancelled;
            const auto invocation = request.message;
            invocation_worker = std::jthread([&, cancellation, invocation, delay] {
                const auto deadline = std::chrono::steady_clock::now() + delay;
                while (!cancellation->load(std::memory_order_acquire) &&
                       std::chrono::steady_clock::now() < deadline)
                    std::this_thread::sleep_for(std::chrono::milliseconds(5));
                const bool cancelled = cancellation->load(std::memory_order_acquire);
                auto result = response_for(
                    invocation,
                    cancelled ? owo::plugin::PluginStatus::cancelled
                              : owo::plugin::PluginStatus::success,
                    cancelled ? std::string{} : "delayed:" + invocation.payload,
                    cancelled ? std::string("cancelled by host") : std::string{});
                send(result);
                invocation_complete.store(true, std::memory_order_release);
            });
            continue;
        }
        auto unsupported = response_for(request.message,
            owo::plugin::PluginStatus::invalid_request, {}, "unknown versioned service");
        if (!send(unsupported)) return 32;
    }
}

}  // namespace

int wmain(const int argc, wchar_t** argv) {
    Arguments arguments;
    if (!parse_arguments(argc, argv, arguments)) return 1;
    return run_plugin(arguments);
}

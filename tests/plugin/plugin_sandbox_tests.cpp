#include "owo/plugin/plugin_pipe.h"
#include "owo/plugin/plugin_sandbox.h"

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <winsock2.h>
#include <Windows.h>
#include <sddl.h>
#include <userenv.h>
#include <ws2tcpip.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
#include <iostream>
#include <iterator>
#include <string>
#include <string_view>
#include <vector>

namespace {

std::wstring qualified_server_pipe_name(const std::wstring& appcontainer_sid,
                                        const std::wstring& client_name) {
    constexpr std::wstring_view client_prefix = LR"(\\.\pipe\LOCAL\)";
    if (!client_name.starts_with(client_prefix)) return {};
    PSID sid = nullptr;
    if (!ConvertStringSidToSidW(appcontainer_sid.c_str(), &sid)) return {};
    ULONG path_size = 0;
    GetAppContainerNamedObjectPath(nullptr, sid, 0, nullptr, &path_size);
    std::vector<wchar_t> path(path_size);
    const bool path_ready = path_size != 0 && GetAppContainerNamedObjectPath(
        nullptr, sid, path_size, path.data(), &path_size) != FALSE;
    LocalFree(sid);
    DWORD session_id = 0;
    if (!path_ready || !ProcessIdToSessionId(GetCurrentProcessId(), &session_id)) return {};
    std::wstring object_path(path.data());
    if (!object_path.empty() && object_path.front() == L'\\') object_path.erase(0, 1);
    return LR"(\\?\pipe\Sessions\)" + std::to_wstring(session_id) + L"\\" +
        object_path + L"\\" + std::wstring(client_name.substr(client_prefix.size()));
}

std::string frame(const std::string_view payload) {
    std::string result;
    result.reserve(payload.size() + 4U);
    const auto size = static_cast<std::uint32_t>(payload.size());
    for (std::size_t shift = 0; shift < 32; shift += 8)
        result.push_back(static_cast<char>(size >> shift));
    result.append(payload);
    return result;
}

int child_probe(const wchar_t* port_text, const wchar_t* inherited_handle_text,
                const wchar_t* pipe_name, const wchar_t* plugin_id_text) {
    HANDLE token = nullptr;
    DWORD is_app_container = 0;
    DWORD returned = 0;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token) ||
        !GetTokenInformation(token, TokenIsAppContainer, &is_app_container,
                             sizeof(is_app_container), &returned)) {
        if (token != nullptr) CloseHandle(token);
        return 10;
    }
    CloseHandle(token);
    if (is_app_container == 0) return 11;
    BOOL in_job = FALSE;
    if (!IsProcessInJob(GetCurrentProcess(), nullptr, &in_job) || in_job == FALSE) return 12;
    static_cast<void>(inherited_handle_text);

    const std::wstring_view wide_plugin_id(plugin_id_text);
    if (std::any_of(wide_plugin_id.begin(), wide_plugin_id.end(),
                    [](const wchar_t value) { return value > 0x7f; })) return 30;
    std::string plugin_id(wide_plugin_id.size(), '\0');
    std::transform(wide_plugin_id.begin(), wide_plugin_id.end(), plugin_id.begin(),
                   [](const wchar_t value) { return static_cast<char>(value); });
    auto pipe = owo::plugin::connect_plugin_pipe_client(pipe_name, std::chrono::seconds(2));
    if (!pipe) {
        const auto separator = pipe.diagnostic.rfind(' ');
        if (separator != std::string::npos) {
            char* end = nullptr;
            const auto error = std::strtoul(pipe.diagnostic.c_str() + separator + 1, &end, 10);
            if (end != nullptr && *end == '\0' && error < 100000) return 1000 + error;
        }
        return 31;
    }
    owo::plugin::PluginMessage hello;
    hello.type = owo::plugin::PluginMessageType::hello_request;
    hello.status = owo::plugin::PluginStatus::success;
    hello.request_id = 1;
    hello.plugin_id = plugin_id;
    if (!owo::plugin::send_plugin_pipe_message(pipe.pipe, hello, std::chrono::seconds(2)).ok)
        return 32;
    const auto response = owo::plugin::receive_plugin_pipe_message(
        pipe.pipe, std::chrono::seconds(2));
    if (!response.status.ok ||
        response.message.type != owo::plugin::PluginMessageType::hello_response ||
        response.message.request_id != hello.request_id ||
        response.message.plugin_id != plugin_id ||
        response.message.capabilities != std::vector<std::string>{"invoke.v1"}) return 33;

    WSADATA data{};
    if (WSAStartup(MAKEWORD(2, 2), &data) != 0) return 15;
    const SOCKET socket_handle = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (socket_handle == INVALID_SOCKET) {
        WSACleanup();
        return 16;
    }
    wchar_t* port_end = nullptr;
    const auto port = std::wcstoul(port_text, &port_end, 10);
    if (port_end == port_text || *port_end != L'\0' || port > 65535) {
        closesocket(socket_handle);
        WSACleanup();
        return 17;
    }
    sockaddr_in address{};
    address.sin_family = AF_INET;
    address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    address.sin_port = htons(static_cast<u_short>(port));
    u_long nonblocking = 1;
    if (ioctlsocket(socket_handle, FIONBIO, &nonblocking) != 0) {
        closesocket(socket_handle);
        WSACleanup();
        return 18;
    }
    int connected = connect(socket_handle, reinterpret_cast<const sockaddr*>(&address),
                            sizeof(address));
    if (connected != 0 && WSAGetLastError() == WSAEWOULDBLOCK) {
        fd_set writable{};
        fd_set exceptional{};
        FD_SET(socket_handle, &writable);
        FD_SET(socket_handle, &exceptional);
        timeval timeout{1, 0};
        const auto selected = select(0, nullptr, &writable, &exceptional, &timeout);
        if (selected > 0 && FD_ISSET(socket_handle, &writable)) {
            int socket_error = 0;
            int socket_error_size = sizeof(socket_error);
            if (getsockopt(socket_handle, SOL_SOCKET, SO_ERROR,
                           reinterpret_cast<char*>(&socket_error), &socket_error_size) == 0 &&
                socket_error == 0) connected = 0;
        }
    }
    closesocket(socket_handle);
    WSACleanup();
    if (connected == 0) return 18;
    return 0;
}

}  // namespace

int wmain(const int argc, wchar_t** argv) {
    if (argc == 6 && std::wstring_view(argv[1]) == L"--child")
        ExitProcess(static_cast<UINT>(child_probe(argv[2], argv[3], argv[4], argv[5])));
    if (argc == 3 && std::wstring_view(argv[1]) == L"--delete-profile") {
        const auto deleted = owo::plugin::delete_plugin_sandbox_profile(argv[2]);
        if (!deleted.ok) std::cerr << deleted.diagnostic << '\n';
        return deleted.ok ? 0 : 1;
    }
    if (argc != 1) return 1;

    const auto plugin_id = "owo.plugin.sandbox-test-" + std::to_string(GetCurrentProcessId()) +
                           "-" + std::to_string(GetTickCount64());
    const auto profile = owo::plugin::prepare_plugin_sandbox_profile(plugin_id);
    if (!profile.ok || !profile.created || profile.profile_directory.empty()) {
        std::cerr << "sandbox profile preparation failed: " << profile.diagnostic << '\n';
        return 2;
    }
    const auto finish = [&](const int code, const std::filesystem::path& copied = {}) {
        if (!copied.empty()) DeleteFileW(copied.c_str());
        const auto deleted = owo::plugin::delete_plugin_sandbox_profile(profile.profile_name);
        return deleted.ok ? code : 90;
    };
    const auto reopened = owo::plugin::prepare_plugin_sandbox_profile(plugin_id);
    if (!reopened.ok || reopened.created || reopened.sid_string != profile.sid_string ||
        reopened.profile_directory != profile.profile_directory) return finish(3);
    if (owo::plugin::delete_plugin_sandbox_profile(L"Unrelated.Profile").ok) return finish(4);
    if (owo::plugin::create_plugin_pipe_server(L"S-1-5-18")) return finish(24);
    auto pipe_server = owo::plugin::create_plugin_pipe_server(profile.sid_string);
    if (!pipe_server) {
        std::cerr << "secure plugin pipe creation failed: " << pipe_server.diagnostic << '\n';
        return finish(25);
    }
    if (owo::plugin::connect_plugin_pipe_client(LR"(\\.\pipe\Unrelated)",
                                                std::chrono::seconds(1))) return finish(26);
    auto same_user = owo::plugin::connect_plugin_pipe_client(
        pipe_server.pipe_name, std::chrono::seconds(1));
    if (same_user) return finish(27);
    const auto qualified_name = qualified_server_pipe_name(
        profile.sid_string, pipe_server.pipe_name);
    HANDLE untrusted = qualified_name.empty() ? INVALID_HANDLE_VALUE : CreateFileW(
        qualified_name.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr, OPEN_EXISTING,
        SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION | SECURITY_EFFECTIVE_ONLY, nullptr);
    owo::plugin::PluginMessage untrusted_hello;
    untrusted_hello.type = owo::plugin::PluginMessageType::hello_request;
    untrusted_hello.status = owo::plugin::PluginStatus::success;
    untrusted_hello.request_id = 1;
    untrusted_hello.plugin_id = plugin_id;
    const auto untrusted_frame = frame(owo::plugin::encode_plugin_message(untrusted_hello));
    DWORD untrusted_written = 0;
    const bool untrusted_sent = untrusted != INVALID_HANDLE_VALUE &&
        WriteFile(untrusted, untrusted_frame.data(), static_cast<DWORD>(untrusted_frame.size()),
                  &untrusted_written, nullptr) != FALSE &&
        untrusted_written == static_cast<DWORD>(untrusted_frame.size());
    const auto untrusted_accepted = untrusted_sent
        ? owo::plugin::accept_plugin_pipe_client(pipe_server.pipe, std::chrono::seconds(1))
        : owo::plugin::PluginPipeOperationResult{};
    const auto untrusted_rejected = untrusted_accepted.ok
        ? owo::plugin::receive_plugin_pipe_message(pipe_server.pipe, std::chrono::seconds(1))
        : owo::plugin::PluginPipeReceiveResult{};
    if (untrusted != INVALID_HANDLE_VALUE) CloseHandle(untrusted);
    if (!untrusted_accepted.ok || untrusted_rejected.status.ok ||
        untrusted_rejected.status.diagnostic !=
            "named pipe client AppContainer SID mismatch") return finish(28);

    wchar_t executable_buffer[32768]{};
    const auto executable_length = GetModuleFileNameW(nullptr, executable_buffer,
                                                      static_cast<DWORD>(std::size(executable_buffer)));
    if (executable_length == 0 || executable_length == std::size(executable_buffer)) return finish(5);
    const auto copied = profile.profile_directory / L"owo-sandbox-probe.exe";
    if (!CopyFileW(executable_buffer, copied.c_str(), TRUE)) return finish(6);

    WSADATA socket_data{};
    if (WSAStartup(MAKEWORD(2, 2), &socket_data) != 0) return finish(7, copied);
    SOCKET listener = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    sockaddr_in listener_address{};
    listener_address.sin_family = AF_INET;
    listener_address.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    listener_address.sin_port = 0;
    int address_size = sizeof(listener_address);
    if (listener == INVALID_SOCKET ||
        bind(listener, reinterpret_cast<const sockaddr*>(&listener_address),
             sizeof(listener_address)) != 0 || listen(listener, 1) != 0 ||
        getsockname(listener, reinterpret_cast<sockaddr*>(&listener_address), &address_size) != 0) {
        if (listener != INVALID_SOCKET) closesocket(listener);
        WSACleanup();
        return finish(8, copied);
    }
    const SOCKET control = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    const bool control_connected = control != INVALID_SOCKET &&
        connect(control, reinterpret_cast<const sockaddr*>(&listener_address),
                sizeof(listener_address)) == 0;
    const SOCKET accepted = control_connected ? accept(listener, nullptr, nullptr) : INVALID_SOCKET;
    if (accepted != INVALID_SOCKET) closesocket(accepted);
    if (control != INVALID_SOCKET) closesocket(control);
    if (!control_connected || accepted == INVALID_SOCKET) {
        closesocket(listener); WSACleanup();
        return finish(9, copied);
    }

    PSID app_container_sid = nullptr;
    if (FAILED(DeriveAppContainerSidFromAppContainerName(
            profile.profile_name.c_str(), &app_container_sid)) || app_container_sid == nullptr) {
        closesocket(listener); WSACleanup();
        return finish(10, copied);
    }
    SIZE_T attribute_size = 0;
    InitializeProcThreadAttributeList(nullptr, 1, 0, &attribute_size);
    auto* attributes = static_cast<LPPROC_THREAD_ATTRIBUTE_LIST>(
        HeapAlloc(GetProcessHeap(), 0, attribute_size));
    SECURITY_CAPABILITIES capabilities{};
    capabilities.AppContainerSid = app_container_sid;
    STARTUPINFOEXW startup{};
    startup.StartupInfo.cb = sizeof(startup);
    bool attributes_ready = attributes != nullptr &&
        InitializeProcThreadAttributeList(attributes, 1, 0, &attribute_size) != FALSE &&
        UpdateProcThreadAttribute(attributes, 0, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                                  &capabilities, sizeof(capabilities), nullptr, nullptr) != FALSE;
    startup.lpAttributeList = attributes;
    if (!attributes_ready) {
        if (attributes != nullptr) HeapFree(GetProcessHeap(), 0, attributes);
        FreeSid(app_container_sid); closesocket(listener); WSACleanup();
        return finish(20, copied);
    }

    HANDLE job = CreateJobObjectW(nullptr, nullptr);
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE |
        JOB_OBJECT_LIMIT_ACTIVE_PROCESS | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = 128ULL * 1024ULL * 1024ULL;
    if (job == nullptr || !SetInformationJobObject(job, JobObjectExtendedLimitInformation,
                                                   &limits, sizeof(limits))) {
        if (job != nullptr) CloseHandle(job);
        DeleteProcThreadAttributeList(attributes); HeapFree(GetProcessHeap(), 0, attributes);
        FreeSid(app_container_sid); closesocket(listener); WSACleanup();
        return finish(21, copied);
    }
    SECURITY_ATTRIBUTES inheritable_attributes{sizeof(inheritable_attributes), nullptr, TRUE};
    HANDLE inheritable = CreateEventW(&inheritable_attributes, TRUE, FALSE, nullptr);
    if (inheritable == nullptr) {
        CloseHandle(job); DeleteProcThreadAttributeList(attributes);
        HeapFree(GetProcessHeap(), 0, attributes); FreeSid(app_container_sid);
        closesocket(listener); WSACleanup();
        return finish(22, copied);
    }
    const auto port = ntohs(listener_address.sin_port);
    const std::wstring wide_plugin_id(plugin_id.begin(), plugin_id.end());
    std::wstring command = L"\"" + copied.native() + L"\" --child " +
        std::to_wstring(port) + L" " +
        std::to_wstring(reinterpret_cast<std::uintptr_t>(inheritable)) + L" \"" +
        pipe_server.pipe_name + L"\" \"" + wide_plugin_id + L"\"";
    PROCESS_INFORMATION process{};
    const DWORD flags = CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT;
    const bool launched = CreateProcessW(copied.c_str(), command.data(), nullptr, nullptr, FALSE,
                                         flags, nullptr, profile.profile_directory.c_str(),
                                         &startup.StartupInfo, &process) != FALSE;
    const bool pipe_bound = launched && owo::plugin::bind_plugin_pipe_client_process(
        pipe_server.pipe, process.dwProcessId);
    HANDLE child_handle_copy = nullptr;
    const bool copied_child_handle = launched &&
        DuplicateHandle(process.hProcess, inheritable, GetCurrentProcess(), &child_handle_copy,
                        0, FALSE, DUPLICATE_SAME_ACCESS) != FALSE;
    const bool inherited_same = copied_child_handle &&
        CompareObjectHandles(child_handle_copy, inheritable) != FALSE;
    if (child_handle_copy != nullptr) CloseHandle(child_handle_copy);
    bool assigned = pipe_bound && !inherited_same &&
        AssignProcessToJobObject(job, process.hProcess) != FALSE;
    bool resumed = assigned && ResumeThread(process.hThread) != static_cast<DWORD>(-1);
    bool pipe_ok = false;
    std::string pipe_diagnostic;
    const auto accepted_pipe = resumed
        ? owo::plugin::accept_plugin_pipe_client(pipe_server.pipe, std::chrono::seconds(2))
        : owo::plugin::PluginPipeOperationResult{};
    if (!accepted_pipe.ok) pipe_diagnostic = accepted_pipe.diagnostic;
    if (resumed && accepted_pipe.ok) {
        const auto request = owo::plugin::receive_plugin_pipe_message(
            pipe_server.pipe, std::chrono::seconds(2));
        if (!request.status.ok) pipe_diagnostic = request.status.diagnostic;
        if (request.status.ok &&
            request.message.type == owo::plugin::PluginMessageType::hello_request &&
            request.message.request_id == 1 && request.message.plugin_id == plugin_id) {
            owo::plugin::PluginMessage response;
            response.type = owo::plugin::PluginMessageType::hello_response;
            response.status = owo::plugin::PluginStatus::success;
            response.request_id = request.message.request_id;
            response.plugin_id = plugin_id;
            response.capabilities = {"invoke.v1"};
            const auto sent = owo::plugin::send_plugin_pipe_message(
                pipe_server.pipe, response, std::chrono::seconds(2));
            pipe_ok = sent.ok;
            if (!sent.ok) pipe_diagnostic = sent.diagnostic;
        } else if (request.status.ok) {
            pipe_diagnostic = "unexpected hello request";
        }
    }
    DWORD exit_code = 99;
    const bool completed = pipe_ok &&
        WaitForSingleObject(process.hProcess, 5000) == WAIT_OBJECT_0;
    if (completed)
        GetExitCodeProcess(process.hProcess, &exit_code);
    else if (launched && WaitForSingleObject(process.hProcess, 100) == WAIT_OBJECT_0)
        GetExitCodeProcess(process.hProcess, &exit_code);
    if (launched) {
        if (!assigned) TerminateProcess(process.hProcess, 98);
        else if (!completed) TerminateJobObject(job, 98);
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    CloseHandle(inheritable);
    CloseHandle(job);
    DeleteProcThreadAttributeList(attributes);
    HeapFree(GetProcessHeap(), 0, attributes);
    FreeSid(app_container_sid);
    closesocket(listener);
    WSACleanup();
    if (!launched || !assigned || !resumed || !completed || exit_code != 0) {
        std::cerr << "sandbox launch probe failed: launched=" << launched
                  << " assigned=" << assigned << " resumed=" << resumed
                  << " pipe_ok=" << pipe_ok << " completed=" << completed
                  << " child_exit=" << exit_code
                  << " inherited_same=" << inherited_same
                  << " pipe_diagnostic=" << pipe_diagnostic
                  << " win32_error=" << GetLastError() << '\n';
    }
    return finish(launched && assigned && resumed && pipe_ok && completed && exit_code == 0
                      ? 0 : 23, copied);
}

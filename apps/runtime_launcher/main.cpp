#include <Windows.h>
#include <TlHelp32.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <string>
#include <string_view>
#include <vector>

namespace {
constexpr wchar_t kCoreReadyEvent[] = L"Local\\OwO.InputMethod.Core.Ready.P1";

std::wstring quote(const std::wstring_view value) {
    std::wstring result = L"\"";
    std::size_t slashes = 0;
    for (const auto ch : value) {
        if (ch == L'\\') {
            ++slashes;
        } else if (ch == L'\"') {
            result.append(slashes * 2 + 1, L'\\');
            result.push_back(ch);
            slashes = 0;
        } else {
            result.append(slashes, L'\\');
            slashes = 0;
            result.push_back(ch);
        }
    }
    result.append(slashes * 2, L'\\');
    result.push_back(L'\"');
    return result;
}

std::wstring utf8_to_wide(const std::string_view value) {
    if (value.empty()) return {};
    const auto size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0);
    if (size <= 0) return {};
    std::wstring result(static_cast<std::size_t>(size), L'\0');
    if (MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                            static_cast<int>(value.size()), result.data(), size) != size)
        return {};
    return result;
}

bool same_path(const std::filesystem::path& left, const std::filesystem::path& right) {
    return _wcsicmp(left.lexically_normal().c_str(), right.lexically_normal().c_str()) == 0;
}

bool process_is_running(const std::filesystem::path& executable) {
    const HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot == INVALID_HANDLE_VALUE) return false;
    PROCESSENTRY32W entry{sizeof(entry)};
    bool found = false;
    if (Process32FirstW(snapshot, &entry)) {
        do {
            const HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE,
                                               entry.th32ProcessID);
            if (process == nullptr) continue;
            std::wstring path(32768, L'\0');
            DWORD size = static_cast<DWORD>(path.size());
            if (QueryFullProcessImageNameW(process, 0, path.data(), &size)) {
                path.resize(size);
                found = same_path(path, executable);
            }
            CloseHandle(process);
            if (found) break;
        } while (Process32NextW(snapshot, &entry));
    }
    CloseHandle(snapshot);
    return found;
}

struct StartedProcess {
    HANDLE process{};
    DWORD id{};
};

StartedProcess start_process(const std::filesystem::path& executable,
                             const std::vector<std::wstring>& arguments,
                             const std::filesystem::path& working_directory,
                             const HANDLE log) {
    std::wstring command = quote(executable.native());
    for (const auto& argument : arguments) {
        command.push_back(L' ');
        command += quote(argument);
    }
    std::vector<wchar_t> mutable_command(command.begin(), command.end());
    mutable_command.push_back(L'\0');
    STARTUPINFOW startup{sizeof(startup)};
    const bool inherit_handles = log != nullptr && log != INVALID_HANDLE_VALUE;
    if (inherit_handles) {
        startup.dwFlags = STARTF_USESTDHANDLES;
        startup.hStdInput = log;
        startup.hStdOutput = log;
        startup.hStdError = log;
    }
    PROCESS_INFORMATION process{};
    if (!CreateProcessW(executable.c_str(), mutable_command.data(), nullptr, nullptr,
                        inherit_handles ? TRUE : FALSE,
                        CREATE_NO_WINDOW, nullptr, working_directory.c_str(), &startup,
                        &process))
        return {};
    CloseHandle(process.hThread);
    return {process.hProcess, process.dwProcessId};
}

void write_log(const HANDLE log, const std::string_view message) {
    if (log == INVALID_HANDLE_VALUE) return;
    DWORD written{};
    WriteFile(log, message.data(), static_cast<DWORD>(message.size()), &written, nullptr);
}
}  // namespace

int WINAPI wWinMain(HINSTANCE, HINSTANCE, PWSTR command_line, int) {
    const auto started_at = std::chrono::steady_clock::now();
    std::wstring executable_path(32768, L'\0');
    const auto executable_size = GetModuleFileNameW(
        nullptr, executable_path.data(), static_cast<DWORD>(executable_path.size()));
    if (executable_size == 0 || executable_size == executable_path.size()) return 2;
    executable_path.resize(executable_size);
    const auto bin = std::filesystem::path(executable_path).parent_path();
    const auto root = bin.parent_path();
    const auto core = bin / L"owo_core_service.exe";
    const auto model_host = bin / L"owo_model_host.exe";
    const auto bridge = root / L"model" / L"runtime" / L"owo_libime_bridge.dll";
    const auto lexicon = root / L"data" / L"owo-cn.owolx";
    auto model = root / L"model" / L"zh_CN.lm";
    const auto pointer = root / L"model" / L"active-model-path.txt";
    if (std::ifstream input(pointer, std::ios::binary); input) {
        std::string value((std::istreambuf_iterator<char>(input)), {});
        while (!value.empty() && (value.back() == '\r' || value.back() == '\n' ||
                                  value.back() == ' ' || value.back() == '\t'))
            value.pop_back();
        const auto wide = utf8_to_wide(value);
        if (!wide.empty() && std::filesystem::is_regular_file(wide)) model = wide;
    }

    wchar_t local_app_data[32768]{};
    const auto data_size = GetEnvironmentVariableW(L"LOCALAPPDATA", local_app_data,
                                                    static_cast<DWORD>(std::size(local_app_data)));
    if (data_size == 0 || data_size >= std::size(local_app_data)) return 3;
    const auto data_root = std::filesystem::path(local_app_data) / L"OwO" / L"InputMethod";
    const auto logs = data_root / L"logs";
    const auto frequency = data_root / L"data" / L"user-frequency.owuf";
    std::error_code error;
    std::filesystem::create_directories(logs, error);
    std::filesystem::create_directories(frequency.parent_path(), error);
    if (error) return 3;
    const auto log_path = logs / L"startup-native.log";
    SECURITY_ATTRIBUTES security{sizeof(security), nullptr, TRUE};
    const HANDLE log = CreateFileW(log_path.c_str(), FILE_APPEND_DATA,
                                   FILE_SHARE_READ | FILE_SHARE_WRITE, &security,
                                   OPEN_ALWAYS, FILE_ATTRIBUTE_NORMAL, nullptr);
    SYSTEMTIME now{};
    GetLocalTime(&now);
    wchar_t stamp[32]{};
    swprintf_s(stamp, L"%04u%02u%02u-%02u%02u%02u", now.wYear, now.wMonth,
               now.wDay, now.wHour, now.wMinute, now.wSecond);
    const HANDLE core_log = CreateFileW(
        (logs / (std::wstring(L"core-") + stamp + L".stderr.log")).c_str(),
        FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, &security, OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL, nullptr);
    const HANDLE model_log = CreateFileW(
        (logs / (std::wstring(L"model-") + stamp + L".stderr.log")).c_str(),
        FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, &security, OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL, nullptr);
    write_log(log, R"({"process":"runtime_launcher","module":"startup","level":"info","event_id":"launcher_started"})" "\r\n");

    if (!std::filesystem::is_regular_file(core) ||
        !std::filesystem::is_regular_file(model_host) ||
        !std::filesystem::is_regular_file(bridge) ||
        !std::filesystem::is_regular_file(model) ||
        !std::filesystem::is_regular_file(lexicon)) {
        write_log(log, R"({"process":"runtime_launcher","module":"startup","level":"error","event_id":"runtime_file_missing"})" "\r\n");
        if (log != INVALID_HANDLE_VALUE) CloseHandle(log);
        return 4;
    }

    const bool core_was_running = process_is_running(core);
    const HANDLE core_ready = CreateEventW(nullptr, TRUE, FALSE, kCoreReadyEvent);
    if (!core_was_running && core_ready != nullptr) ResetEvent(core_ready);
    StartedProcess core_process;
    if (!core_was_running)
        core_process = start_process(core,
            {L"--lexicon", lexicon.native(), L"--user-frequency", frequency.native(),
             L"--model-host"}, root, core_log);
    StartedProcess model_process;
    if (!process_is_running(model_host))
        model_process = start_process(model_host,
            {L"--libime-bridge", bridge.native(), L"--libime-model", model.native()},
            bridge.parent_path(), model_log);
    if (core_log != INVALID_HANDLE_VALUE) CloseHandle(core_log);
    if (model_log != INVALID_HANDLE_VALUE) CloseHandle(model_log);

    DWORD ready_status = core_was_running ? WAIT_OBJECT_0 : WAIT_TIMEOUT;
    if (!core_was_running && core_process.process == nullptr)
        ready_status = WAIT_FAILED;
    else if (!core_was_running && core_ready != nullptr)
        ready_status = WaitForSingleObject(core_ready, 30000);
    const auto elapsed_us = std::chrono::duration_cast<std::chrono::microseconds>(
        std::chrono::steady_clock::now() - started_at).count();
    write_log(log, std::string("{\"process\":\"runtime_launcher\",\"module\":\"startup\",\"level\":\"info\",\"event_id\":\"launcher_finished\",\"duration_us\":") +
                       std::to_string(elapsed_us) + ",\"core_ready\":" +
                       (ready_status == WAIT_OBJECT_0 ? "true" : "false") + "}\r\n");
    if (core_ready != nullptr) CloseHandle(core_ready);
    if (core_process.process != nullptr) CloseHandle(core_process.process);
    if (model_process.process != nullptr) CloseHandle(model_process.process);
    if (log != INVALID_HANDLE_VALUE) CloseHandle(log);

    if (std::wstring_view(command_line).find(L"--open-settings") != std::wstring_view::npos) {
        const auto settings = root / L"settings" / L"OwO.Settings.exe";
        if (std::filesystem::is_regular_file(settings))
            start_process(settings, {}, settings.parent_path(), INVALID_HANDLE_VALUE);
    }
    return ready_status == WAIT_OBJECT_0 ? 0 : 5;
}

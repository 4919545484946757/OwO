#include <Windows.h>

#include <chrono>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>
#include <thread>

namespace {

bool launch(const std::wstring& command, PROCESS_INFORMATION& process) {
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    std::wstring mutable_command = command;
    if (!CreateProcessW(nullptr, mutable_command.data(), nullptr, nullptr, FALSE, CREATE_NO_WINDOW,
                        nullptr, nullptr, &startup, &process)) return false;
    CloseHandle(process.hThread);
    process.hThread = nullptr;
    return true;
}

DWORD wait_and_close(PROCESS_INFORMATION& process, const DWORD timeout = 5000) {
    if (WaitForSingleObject(process.hProcess, timeout) != WAIT_OBJECT_0) {
        TerminateProcess(process.hProcess, 99);
        WaitForSingleObject(process.hProcess, 1000);
    }
    DWORD exit_code = STILL_ACTIVE;
    GetExitCodeProcess(process.hProcess, &exit_code);
    CloseHandle(process.hProcess);
    process.hProcess = nullptr;
    return exit_code;
}

std::wstring quote(const std::wstring_view value) { return L"\"" + std::wstring(value) + L"\""; }

}  // namespace

int wmain(const int argc, wchar_t** argv) {
    if (argc != 3) return 1;
    const std::filesystem::path root(argv[2]);
    std::error_code error;
    std::filesystem::remove_all(root, error);
    std::filesystem::create_directories(root, error);
    if (error) return 2;
    const auto path = root / L"owo.conf";
    const std::wstring executable = quote(argv[1]);
    const std::wstring config = quote(path.wstring());

    PROCESS_INFORMATION initial{};
    if (!launch(executable + L" " + config + L" set candidate_page_size 5", initial) ||
        wait_and_close(initial) != 0) return 3;

    PROCESS_INFORMATION watcher{};
    if (!launch(executable + L" " + config + L" watch 3000", watcher)) return 4;
    std::this_thread::sleep_for(std::chrono::milliseconds(200));
    PROCESS_INFORMATION setter{};
    if (!launch(executable + L" " + config + L" set candidate_page_size 7", setter) ||
        wait_and_close(setter) != 0 || wait_and_close(watcher) != 0) return 5;

    std::ifstream input(path, std::ios::binary);
    const std::string bytes((std::istreambuf_iterator<char>(input)), {});
    input.close();
    if (bytes.find("candidate_page_size=7\n") == std::string::npos) return 6;
    PROCESS_INFORMATION invalid{};
    if (!launch(executable + L" " + config + L" set candidate_page_size 99", invalid)) return 7;
    if (wait_and_close(invalid) == 0) return 8;
    std::ifstream unchanged_input(path, std::ios::binary);
    const std::string unchanged((std::istreambuf_iterator<char>(unchanged_input)), {});
    unchanged_input.close();
    if (unchanged != bytes) return 9;

    PROCESS_INFORMATION set_all{};
    if (!launch(executable + L" " + config + L" set-all 3 false true 25", set_all) ||
        wait_and_close(set_all) != 0) return 13;
    std::ifstream set_all_input(path, std::ios::binary);
    const std::string set_all_bytes((std::istreambuf_iterator<char>(set_all_input)), {});
    set_all_input.close();
    if (set_all_bytes.find("candidate_page_size=3\n") == std::string::npos ||
        set_all_bytes.find("user_learning_enabled=false\n") == std::string::npos ||
        set_all_bytes.find("model_ranking_enabled=true\n") == std::string::npos ||
        set_all_bytes.find("model_timeout_ms=25\n") == std::string::npos) return 14;
    PROCESS_INFORMATION shortcuts{};
    if (!launch(executable + L" " + config +
                L" set-all 4 true false 40 true Ctrl+Alt+C false Ctrl+Shift+Space true Enter 18",
                shortcuts) || wait_and_close(shortcuts) != 0) return 17;
    std::ifstream shortcut_input(path, std::ios::binary);
    const std::string shortcut_bytes((std::istreambuf_iterator<char>(shortcut_input)), {});
    shortcut_input.close();
    if (shortcut_bytes.find("correction_shortcut=Ctrl+Alt+C\n") == std::string::npos ||
        shortcut_bytes.find("language_shortcut_enabled=false\n") == std::string::npos ||
        shortcut_bytes.find("language_shortcut=Ctrl+Shift+Space\n") == std::string::npos ||
        shortcut_bytes.find("raw_input_shortcut=Enter\n") == std::string::npos ||
        shortcut_bytes.find("candidate_wrap_length=18\n") == std::string::npos) return 18;
    PROCESS_INFORMATION sensitivity{};
    if (!launch(executable + L" " + config +
                L" set-all 4 true true 40 true Ctrl+Alt+C false Ctrl+Shift+Space true Enter 18 9",
                sensitivity) || wait_and_close(sensitivity) != 0) return 19;
    std::ifstream sensitivity_input(path, std::ios::binary);
    const std::string sensitivity_bytes((std::istreambuf_iterator<char>(sensitivity_input)), {});
    sensitivity_input.close();
    if (sensitivity_bytes.find("user_learning_sensitivity=9\n") == std::string::npos)
        return 20;
    PROCESS_INFORMATION invalid_all{};
    if (!launch(executable + L" " + config + L" set-all 4 true false 999", invalid_all) ||
        wait_and_close(invalid_all) == 0) return 15;
    std::ifstream invalid_all_input(path, std::ios::binary);
    const std::string after_invalid_all((std::istreambuf_iterator<char>(invalid_all_input)), {});
    invalid_all_input.close();
    if (after_invalid_all != sensitivity_bytes) return 16;

    PROCESS_INFORMATION show{};
    if (!launch(executable + L" " + config + L" show", show) || wait_and_close(show) != 0) return 10;
    { std::ofstream corrupt(path, std::ios::binary | std::ios::trunc); corrupt << "broken"; }
    PROCESS_INFORMATION repair{};
    if (!launch(executable + L" " + config + L" repair", repair) ||
        wait_and_close(repair) != 0) return 11;
    std::ifstream repaired_input(path, std::ios::binary);
    const std::string repaired((std::istreambuf_iterator<char>(repaired_input)), {});
    if (repaired.find("candidate_page_size=4\n") == std::string::npos ||
        repaired.find("user_learning_sensitivity=7\n") == std::string::npos) return 12;
    std::filesystem::remove_all(root, error);
    return 0;
}

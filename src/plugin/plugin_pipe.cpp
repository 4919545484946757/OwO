#include "owo/plugin/plugin_pipe.h"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#include <bcrypt.h>
#include <sddl.h>
#include <securityappcontainer.h>
#endif

#include <algorithm>
#include <array>
#include <limits>
#include <utility>
#include <vector>

namespace owo::plugin {

struct PluginPipeAccess {
    static std::uintptr_t native(const PluginPipe& pipe) { return pipe.native_handle_; }
    static const std::wstring& expected_sid(const PluginPipe& pipe) {
        return pipe.expected_peer_sid_;
    }
    static bool expected_appcontainer(const PluginPipe& pipe) {
        return pipe.expected_appcontainer_;
    }
    static std::uint32_t expected_process_id(const PluginPipe& pipe) {
        return pipe.expected_process_id_;
    }
};

namespace {

#ifdef _WIN32
constexpr std::wstring_view kPipePrefix =
    LR"(\\.\pipe\LOCAL\OwO.InputMethod.PluginHost.)";
#endif

PluginPipeOpenResult open_failure(std::string diagnostic) {
    return {{}, {}, std::move(diagnostic)};
}

PluginPipeOperationResult operation_failure(std::string diagnostic) {
    return {false, std::move(diagnostic)};
}

#ifdef _WIN32
bool generated_pipe_name(const std::wstring_view value) {
    return value.size() == kPipePrefix.size() + 32 &&
           value.starts_with(kPipePrefix) &&
           std::all_of(value.begin() + static_cast<std::ptrdiff_t>(kPipePrefix.size()),
                       value.end(), [](const wchar_t character) {
                           return (character >= L'0' && character <= L'9') ||
                                  (character >= L'a' && character <= L'f');
                       });
}

using Deadline = std::chrono::steady_clock::time_point;

HANDLE native_handle(const PluginPipe& pipe) {
    return reinterpret_cast<HANDLE>(PluginPipeAccess::native(pipe));
}

std::string win32_error(const char* operation, const DWORD error = GetLastError()) {
    return std::string(operation) + " failed with Win32 error " + std::to_string(error);
}

DWORD remaining_milliseconds(const Deadline deadline) {
    const auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
        deadline - std::chrono::steady_clock::now());
    if (remaining.count() <= 0) return 0;
    return static_cast<DWORD>((std::min)(
        remaining.count(), static_cast<long long>(INFINITE - 1)));
}

bool valid_appcontainer_sid(PSID sid) {
    if (sid == nullptr || !IsValidSid(sid)) return false;
    const auto* authority = GetSidIdentifierAuthority(sid);
    const auto count = *GetSidSubAuthorityCount(sid);
    return authority != nullptr && authority->Value[0] == 0 && authority->Value[1] == 0 &&
           authority->Value[2] == 0 && authority->Value[3] == 0 &&
           authority->Value[4] == 0 && authority->Value[5] == 15 && count >= 2 &&
           *GetSidSubAuthority(sid, 0) == 2;
}

std::wstring current_user_sid() {
    HANDLE token = nullptr;
    if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &token)) return {};
    DWORD size = 0;
    GetTokenInformation(token, TokenUser, nullptr, 0, &size);
    std::vector<unsigned char> buffer(size);
    if (size == 0 || !GetTokenInformation(token, TokenUser, buffer.data(), size, &size)) {
        CloseHandle(token);
        return {};
    }
    const auto* user = reinterpret_cast<const TOKEN_USER*>(buffer.data());
    LPWSTR text = nullptr;
    const bool converted = ConvertSidToStringSidW(user->User.Sid, &text) != FALSE;
    std::wstring result = converted && text != nullptr ? text : L"";
    if (text != nullptr) LocalFree(text);
    CloseHandle(token);
    return result;
}

std::wstring random_pipe_suffix() {
    std::array<unsigned char, 16> random{};
    if (BCryptGenRandom(nullptr, random.data(), static_cast<ULONG>(random.size()),
                        BCRYPT_USE_SYSTEM_PREFERRED_RNG) < 0) return {};
    constexpr wchar_t hex[] = L"0123456789abcdef";
    std::wstring result;
    result.reserve(32);
    for (const auto byte : random) {
        result.push_back(hex[byte >> 4U]);
        result.push_back(hex[byte & 0x0fU]);
    }
    return result;
}

std::wstring appcontainer_named_object_path(PSID sid) {
    ULONG size = 0;
    GetAppContainerNamedObjectPath(nullptr, sid, 0, nullptr, &size);
    if (size == 0) return {};
    std::vector<wchar_t> buffer(size);
    if (!GetAppContainerNamedObjectPath(nullptr, sid, size, buffer.data(), &size) ||
        buffer.empty() || buffer.front() == L'\0') return {};
    return buffer.data();
}

bool transfer(const HANDLE pipe, void* buffer, const DWORD size, const bool write,
              const Deadline deadline, DWORD& transferred) {
    OVERLAPPED operation{};
    operation.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (operation.hEvent == nullptr) return false;
    transferred = 0;
    const BOOL immediate = write
        ? WriteFile(pipe, buffer, size, &transferred, &operation)
        : ReadFile(pipe, buffer, size, &transferred, &operation);
    if (!immediate && GetLastError() != ERROR_IO_PENDING) {
        CloseHandle(operation.hEvent);
        return false;
    }
    if (!immediate) {
        const DWORD remaining = remaining_milliseconds(deadline);
        const DWORD waited = remaining == 0 ? WAIT_TIMEOUT
            : WaitForSingleObject(operation.hEvent, remaining);
        if (waited != WAIT_OBJECT_0) {
            CancelIoEx(pipe, &operation);
            GetOverlappedResult(pipe, &operation, &transferred, TRUE);
            CloseHandle(operation.hEvent);
            SetLastError(waited == WAIT_TIMEOUT ? ERROR_TIMEOUT : ERROR_OPERATION_ABORTED);
            return false;
        }
        if (!GetOverlappedResult(pipe, &operation, &transferred, FALSE)) {
            CloseHandle(operation.hEvent);
            return false;
        }
    }
    CloseHandle(operation.hEvent);
    return transferred != 0;
}

bool write_all(const HANDLE pipe, const std::string_view bytes, const Deadline deadline) {
    std::size_t offset = 0;
    while (offset < bytes.size()) {
        const auto chunk = static_cast<DWORD>((std::min)(
            bytes.size() - offset,
            static_cast<std::size_t>((std::numeric_limits<DWORD>::max)())));
        DWORD written = 0;
        if (!transfer(pipe, const_cast<char*>(bytes.data() + offset), chunk, true,
                      deadline, written)) return false;
        offset += written;
    }
    return true;
}

bool read_exact(const HANDLE pipe, char* output, const DWORD size, const Deadline deadline) {
    DWORD offset = 0;
    while (offset < size) {
        DWORD read = 0;
        if (!transfer(pipe, output + offset, size - offset, false, deadline, read)) return false;
        offset += read;
    }
    return true;
}

bool expected_pipe_client(const PluginPipe& pipe, std::string& diagnostic) {
    const auto& expected_sid = PluginPipeAccess::expected_sid(pipe);
    if (expected_sid.empty()) return true;
    ULONG client_process_id = 0;
    if (PluginPipeAccess::expected_process_id(pipe) != 0 &&
        (!GetNamedPipeClientProcessId(native_handle(pipe), &client_process_id) ||
         client_process_id != PluginPipeAccess::expected_process_id(pipe))) {
        diagnostic = "named pipe client process identity mismatch";
        return false;
    }
    if (!ImpersonateNamedPipeClient(native_handle(pipe))) {
        diagnostic = win32_error("ImpersonateNamedPipeClient");
        return false;
    }
    HANDLE token = nullptr;
    const bool opened = OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, TRUE, &token) != FALSE;
    const DWORD open_error = opened ? ERROR_SUCCESS : GetLastError();
    const bool reverted = RevertToSelf() != FALSE;
    if (!opened || !reverted) {
        if (token != nullptr) CloseHandle(token);
        diagnostic = !opened ? win32_error("OpenThreadToken", open_error)
                             : win32_error("RevertToSelf");
        return false;
    }
    DWORD is_appcontainer = 0;
    DWORD returned = 0;
    const bool appcontainer =
        GetTokenInformation(token, TokenIsAppContainer, &is_appcontainer,
                            sizeof(is_appcontainer), &returned) != FALSE &&
        is_appcontainer != 0;
    PSID expected = nullptr;
    const bool parsed = ConvertStringSidToSidW(expected_sid.c_str(), &expected) != FALSE;
    bool matches = false;
    if (PluginPipeAccess::expected_appcontainer(pipe)) {
        DWORD size = 0;
        GetTokenInformation(token, TokenAppContainerSid, nullptr, 0, &size);
        std::vector<unsigned char> buffer(size);
        const bool read_sid = size >= sizeof(TOKEN_APPCONTAINER_INFORMATION) &&
            GetTokenInformation(token, TokenAppContainerSid, buffer.data(), size, &size) != FALSE;
        const auto* information = read_sid
            ? reinterpret_cast<const TOKEN_APPCONTAINER_INFORMATION*>(buffer.data()) : nullptr;
        matches = appcontainer && parsed && information != nullptr &&
            information->TokenAppContainer != nullptr &&
            EqualSid(expected, information->TokenAppContainer) != FALSE;
    } else {
        DWORD size = 0;
        GetTokenInformation(token, TokenUser, nullptr, 0, &size);
        std::vector<unsigned char> buffer(size);
        const bool read_user = size >= sizeof(TOKEN_USER) &&
            GetTokenInformation(token, TokenUser, buffer.data(), size, &size) != FALSE;
        const auto* user = read_user ? reinterpret_cast<const TOKEN_USER*>(buffer.data()) : nullptr;
        matches = !appcontainer && parsed && user != nullptr &&
            EqualSid(expected, user->User.Sid) != FALSE;
    }
    if (expected != nullptr) LocalFree(expected);
    CloseHandle(token);
    if (!matches) diagnostic = "named pipe client token identity mismatch";
    return matches;
}
#endif

PluginPipeReceiveResult receive_failure(PluginPipe& pipe, std::string diagnostic) {
#ifdef _WIN32
    if (pipe.valid() && !PluginPipeAccess::expected_sid(pipe).empty())
        DisconnectNamedPipe(native_handle(pipe));
#else
    static_cast<void>(pipe);
#endif
    return {{}, operation_failure(std::move(diagnostic))};
}

}  // namespace

PluginPipe::PluginPipe(const std::uintptr_t native_handle,
                       std::wstring expected_peer_sid,
                       const bool expected_appcontainer)
    : native_handle_(native_handle),
      expected_peer_sid_(std::move(expected_peer_sid)),
      expected_appcontainer_(expected_appcontainer) {}

PluginPipe::PluginPipe(PluginPipe&& other) noexcept
    : native_handle_(std::exchange(other.native_handle_, static_cast<std::uintptr_t>(-1))),
      expected_peer_sid_(std::move(other.expected_peer_sid_)),
      expected_appcontainer_(other.expected_appcontainer_),
      expected_process_id_(other.expected_process_id_) {
    other.expected_appcontainer_ = false;
    other.expected_process_id_ = 0;
}

PluginPipe& PluginPipe::operator=(PluginPipe&& other) noexcept {
    if (this != &other) {
        close();
        native_handle_ = std::exchange(other.native_handle_, static_cast<std::uintptr_t>(-1));
        expected_peer_sid_ = std::move(other.expected_peer_sid_);
        expected_appcontainer_ = other.expected_appcontainer_;
        expected_process_id_ = other.expected_process_id_;
        other.expected_appcontainer_ = false;
        other.expected_process_id_ = 0;
    }
    return *this;
}

PluginPipe::~PluginPipe() { close(); }

bool PluginPipe::valid() const noexcept {
    return native_handle_ != static_cast<std::uintptr_t>(-1);
}

void PluginPipe::close() noexcept {
#ifdef _WIN32
    if (valid()) CloseHandle(native_handle(*this));
#endif
    native_handle_ = static_cast<std::uintptr_t>(-1);
    expected_peer_sid_.clear();
    expected_appcontainer_ = false;
    expected_process_id_ = 0;
}

PluginPipeOpenResult create_plugin_pipe_server(
    const std::wstring_view expected_appcontainer_sid) {
#ifdef _WIN32
    const std::wstring expected_text(expected_appcontainer_sid);
    PSID expected = nullptr;
    if (!ConvertStringSidToSidW(expected_text.c_str(), &expected) ||
        !valid_appcontainer_sid(expected)) {
        if (expected != nullptr) LocalFree(expected);
        return open_failure("invalid AppContainer SID for plugin pipe");
    }
    const auto user_sid = current_user_sid();
    const auto object_path = appcontainer_named_object_path(expected);
    const DWORD object_path_error = object_path.empty() ? GetLastError() : ERROR_SUCCESS;
    const auto suffix = random_pipe_suffix();
    LocalFree(expected);
    if (object_path.empty())
        return open_failure(win32_error("GetAppContainerNamedObjectPath", object_path_error));
    if (user_sid.empty() || suffix.empty())
        return open_failure("cannot prepare secure plugin pipe identity");
    DWORD session_id = 0;
    if (!ProcessIdToSessionId(GetCurrentProcessId(), &session_id))
        return open_failure(win32_error("ProcessIdToSessionId"));
    const std::wstring client_name = std::wstring(kPipePrefix) + suffix;
    const std::wstring server_name = LR"(\\?\pipe\Sessions\)" +
        std::to_wstring(session_id) + L"\\" +
        (object_path.front() == L'\\' ? object_path.substr(1) : object_path) +
        L"\\OwO.InputMethod.PluginHost." + suffix;
    const std::wstring sddl = L"D:P(A;;GA;;;" + user_sid +
        L")(A;;0x12019b;;;" + expected_text + L")";
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.c_str(), SDDL_REVISION_1, &descriptor, nullptr))
        return open_failure(win32_error("ConvertStringSecurityDescriptor"));
    SECURITY_ATTRIBUTES security{sizeof(security), descriptor, FALSE};
    const HANDLE handle = CreateNamedPipeW(
        server_name.c_str(), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED |
                          FILE_FLAG_FIRST_PIPE_INSTANCE,
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
        1, static_cast<DWORD>(kMaximumPluginWireBytes + 4U),
        static_cast<DWORD>(kMaximumPluginWireBytes + 4U), 1000, &security);
    const DWORD error = handle == INVALID_HANDLE_VALUE ? GetLastError() : ERROR_SUCCESS;
    LocalFree(descriptor);
    if (handle == INVALID_HANDLE_VALUE)
        return open_failure(win32_error("CreateNamedPipeW", error));
    return {PluginPipe(reinterpret_cast<std::uintptr_t>(handle), expected_text, true),
            client_name, {}};
#else
    static_cast<void>(expected_appcontainer_sid);
    return open_failure("secure plugin pipes are currently available on Windows only");
#endif
}

PluginPipeOpenResult create_full_trust_plugin_pipe_server() {
#ifdef _WIN32
    const auto user_sid = current_user_sid();
    const auto suffix = random_pipe_suffix();
    if (user_sid.empty() || suffix.empty())
        return open_failure("cannot prepare full-trust plugin pipe identity");
    const std::wstring name = std::wstring(kPipePrefix) + suffix;
    const std::wstring sddl = L"D:P(A;;GA;;;" + user_sid + L")";
    PSECURITY_DESCRIPTOR descriptor = nullptr;
    if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.c_str(), SDDL_REVISION_1, &descriptor, nullptr))
        return open_failure(win32_error("ConvertStringSecurityDescriptor"));
    SECURITY_ATTRIBUTES security{sizeof(security), descriptor, FALSE};
    const HANDLE handle = CreateNamedPipeW(
        name.c_str(), PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED |
                          FILE_FLAG_FIRST_PIPE_INSTANCE,
        PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
        1, static_cast<DWORD>(kMaximumPluginWireBytes + 4U),
        static_cast<DWORD>(kMaximumPluginWireBytes + 4U), 1000, &security);
    const DWORD error = handle == INVALID_HANDLE_VALUE ? GetLastError() : ERROR_SUCCESS;
    LocalFree(descriptor);
    if (handle == INVALID_HANDLE_VALUE)
        return open_failure(win32_error("CreateNamedPipeW", error));
    return {PluginPipe(reinterpret_cast<std::uintptr_t>(handle), user_sid, false), name, {}};
#else
    return open_failure("secure plugin pipes are currently available on Windows only");
#endif
}

bool bind_plugin_pipe_client_process(PluginPipe& server,
                                     const std::uint32_t process_id) noexcept {
    if (!server.valid() || server.expected_peer_sid_.empty() || process_id == 0 ||
        server.expected_process_id_ != 0) return false;
    server.expected_process_id_ = process_id;
    return true;
}

PluginPipeOpenResult connect_plugin_pipe_client(
    const std::wstring_view pipe_name,
    const std::chrono::milliseconds timeout) {
#ifdef _WIN32
    if (!generated_pipe_name(pipe_name)) return open_failure("invalid plugin pipe name");
    if (timeout.count() <= 0 || timeout.count() >= INFINITE)
        return open_failure("invalid plugin pipe connection timeout");
    const std::wstring name(pipe_name);
    if (!WaitNamedPipeW(name.c_str(), static_cast<DWORD>(timeout.count())))
        return open_failure(win32_error("WaitNamedPipeW"));
    constexpr DWORD access = FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_EA | FILE_WRITE_EA |
        FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
    const HANDLE handle = CreateFileW(
        name.c_str(), access, 0, nullptr, OPEN_EXISTING,
        FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION |
            SECURITY_EFFECTIVE_ONLY,
        nullptr);
    if (handle == INVALID_HANDLE_VALUE) return open_failure(win32_error("CreateFileW"));
    return {PluginPipe(reinterpret_cast<std::uintptr_t>(handle), {}, false), name, {}};
#else
    static_cast<void>(pipe_name);
    static_cast<void>(timeout);
    return open_failure("secure plugin pipes are currently available on Windows only");
#endif
}

PluginPipeOperationResult accept_plugin_pipe_client(
    PluginPipe& server,
    const std::chrono::milliseconds timeout) {
#ifdef _WIN32
    if (!server.valid() || server.expected_peer_sid_.empty())
        return operation_failure("plugin pipe is not a server");
    if (timeout.count() <= 0) return operation_failure("invalid plugin pipe accept timeout");
    OVERLAPPED operation{};
    operation.hEvent = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (operation.hEvent == nullptr) return operation_failure(win32_error("CreateEventW"));
    const BOOL immediate = ConnectNamedPipe(native_handle(server), &operation);
    const DWORD connect_error = immediate ? ERROR_SUCCESS : GetLastError();
    bool connected = immediate || connect_error == ERROR_PIPE_CONNECTED;
    if (!connected && connect_error == ERROR_IO_PENDING) {
        const DWORD waited = WaitForSingleObject(operation.hEvent,
            static_cast<DWORD>((std::min)(timeout.count(),
                                         static_cast<long long>(INFINITE - 1))));
        if (waited == WAIT_OBJECT_0) {
            DWORD transferred = 0;
            connected = GetOverlappedResult(native_handle(server), &operation,
                                            &transferred, FALSE) != FALSE;
        } else {
            CancelIoEx(native_handle(server), &operation);
            DWORD transferred = 0;
            GetOverlappedResult(native_handle(server), &operation, &transferred, TRUE);
            SetLastError(waited == WAIT_TIMEOUT ? ERROR_TIMEOUT : ERROR_OPERATION_ABORTED);
        }
    } else if (!connected) {
        SetLastError(connect_error);
    }
    const DWORD error = connected ? ERROR_SUCCESS : GetLastError();
    CloseHandle(operation.hEvent);
    return connected ? PluginPipeOperationResult{true, {}}
                     : operation_failure(win32_error("ConnectNamedPipe", error));
#else
    static_cast<void>(server);
    static_cast<void>(timeout);
    return operation_failure("secure plugin pipes are currently available on Windows only");
#endif
}

PluginPipeOperationResult send_plugin_pipe_message(
    PluginPipe& pipe,
    const PluginMessage& message,
    const std::chrono::milliseconds timeout) {
#ifdef _WIN32
    if (!pipe.valid() || timeout.count() <= 0)
        return operation_failure("invalid plugin pipe send arguments");
    const auto encoded = encode_plugin_message(message);
    if (encoded.empty() || encoded.size() > kMaximumPluginWireBytes)
        return operation_failure("invalid or oversized plugin message");
    std::string framed;
    framed.reserve(encoded.size() + 4U);
    const auto size = static_cast<std::uint32_t>(encoded.size());
    for (std::size_t shift = 0; shift < 32; shift += 8)
        framed.push_back(static_cast<char>(size >> shift));
    framed.append(encoded);
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    if (!write_all(native_handle(pipe), framed, deadline))
        return operation_failure(win32_error("WriteFile"));
    return {true, {}};
#else
    static_cast<void>(pipe);
    static_cast<void>(message);
    static_cast<void>(timeout);
    return operation_failure("secure plugin pipes are currently available on Windows only");
#endif
}

PluginPipeReceiveResult receive_plugin_pipe_message(
    PluginPipe& pipe,
    const std::chrono::milliseconds timeout) {
#ifdef _WIN32
    if (!pipe.valid() || timeout.count() <= 0)
        return receive_failure(pipe, "invalid plugin pipe receive arguments");
    const auto deadline = std::chrono::steady_clock::now() + timeout;
    std::array<unsigned char, 4> prefix{};
    if (!read_exact(native_handle(pipe), reinterpret_cast<char*>(prefix.data()), 4, deadline))
        return receive_failure(pipe, win32_error("ReadFile"));
    const std::uint32_t size = static_cast<std::uint32_t>(prefix[0]) |
        (static_cast<std::uint32_t>(prefix[1]) << 8U) |
        (static_cast<std::uint32_t>(prefix[2]) << 16U) |
        (static_cast<std::uint32_t>(prefix[3]) << 24U);
    if (size == 0 || size > kMaximumPluginWireBytes)
        return receive_failure(pipe, "invalid plugin pipe frame size");
    std::string payload(size, '\0');
    if (!read_exact(native_handle(pipe), payload.data(), size, deadline))
        return receive_failure(pipe, win32_error("ReadFile"));
    std::string peer_diagnostic;
    if (!expected_pipe_client(pipe, peer_diagnostic))
        return receive_failure(pipe, std::move(peer_diagnostic));
    const auto decoded = decode_plugin_message(payload);
    if (!decoded.validation)
        return receive_failure(pipe, "invalid plugin protocol frame: " +
                                     decoded.validation.message);
    return {decoded.message, {true, {}}};
#else
    static_cast<void>(pipe);
    static_cast<void>(timeout);
    return receive_failure(pipe,
        "secure plugin pipes are currently available on Windows only");
#endif
}

void disconnect_plugin_pipe(PluginPipe& server) noexcept {
#ifdef _WIN32
    if (server.valid() && !server.expected_peer_sid_.empty()) {
        DisconnectNamedPipe(native_handle(server));
    }
#else
    static_cast<void>(server);
#endif
}

}  // namespace owo::plugin

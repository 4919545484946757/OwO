#include "owo/plugin/plugin_authorization_store.h"

#include "owo/plugin/plugin_store.h"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#endif

#include <algorithm>
#include <fstream>
#include <limits>

namespace owo::plugin {
namespace {

PluginAuthorizationStoreResult failure(std::string diagnostic) {
    return {false, {}, {}, std::move(diagnostic)};
}

#ifdef _WIN32
bool safe_directory(const std::filesystem::path& path) {
    const auto attributes = GetFileAttributesW(path.c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & FILE_ATTRIBUTE_DIRECTORY) != 0 &&
           (attributes & FILE_ATTRIBUTE_REPARSE_POINT) == 0;
}

bool safe_file(const std::filesystem::path& path) {
    const auto attributes = GetFileAttributesW(path.c_str());
    return attributes != INVALID_FILE_ATTRIBUTES &&
           (attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)) == 0;
}

bool ensure_directory(const std::filesystem::path& path) {
    if (CreateDirectoryW(path.c_str(), nullptr) == FALSE && GetLastError() != ERROR_ALREADY_EXISTS)
        return false;
    return safe_directory(path);
}

bool write_exact(HANDLE file, const std::string_view bytes) {
    std::size_t offset = 0;
    while (offset < bytes.size()) {
        DWORD written = 0;
        const auto chunk = static_cast<DWORD>((std::min)(bytes.size() - offset,
            static_cast<std::size_t>((std::numeric_limits<DWORD>::max)())));
        if (!WriteFile(file, bytes.data() + offset, chunk, &written, nullptr) || written != chunk)
            return false;
        offset += written;
    }
    return FlushFileBuffers(file) != FALSE;
}

bool atomic_write(const std::filesystem::path& target, const std::string_view bytes) {
    const auto attributes = GetFileAttributesW(target.c_str());
    if (attributes != INVALID_FILE_ATTRIBUTES && !safe_file(target)) return false;
    auto temporary = target;
    temporary += L".tmp." + std::to_wstring(GetCurrentProcessId()) + L"." +
                 std::to_wstring(GetTickCount64());
    HANDLE file = CreateFileW(temporary.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_NEW,
                              FILE_ATTRIBUTE_NORMAL | FILE_FLAG_WRITE_THROUGH, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    const bool written = write_exact(file, bytes);
    CloseHandle(file);
    if (!written || MoveFileExW(temporary.c_str(), target.c_str(),
                                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) == FALSE) {
        DeleteFileW(temporary.c_str());
        return false;
    }
    return true;
}

bool read_record(const std::filesystem::path& path, std::string& bytes) {
    if (!safe_file(path)) return false;
    std::error_code error;
    const auto size = std::filesystem::file_size(path, error);
    if (error || size == 0 || size > 4096) return false;
    std::ifstream input(path, std::ios::binary);
    bytes.assign(static_cast<std::size_t>(size), '\0');
    return input.read(bytes.data(), static_cast<std::streamsize>(bytes.size())) &&
           input.peek() == std::char_traits<char>::eof();
}

PluginAuthorizationResult canonical_authorization(
    const InstalledPluginVersionResult& installed,
    const PluginAuthorization& authorization) {
    if (authorization.schema_version != kPluginAuthorizationSchemaVersion ||
        authorization.plugin_id != installed.manifest.id ||
        authorization.version != installed.manifest.version ||
        authorization.inventory_sha256 != installed.inventory_sha256 ||
        authorization.publisher_certificate_sha256 != installed.publisher_certificate_sha256 ||
        (authorization.schema_version == 1 &&
         installed.trust_tier != PluginTrustTier::trusted_publisher) ||
        (authorization.schema_version >= 2 &&
         authorization.context.trust_tier != installed.trust_tier))
        return {false, {}, "authorization does not match installed version binding"};
    auto canonical = make_plugin_authorization(
        installed.manifest, installed.inventory_sha256,
        installed.publisher_certificate_sha256,
        authorization.granted_permissions, authorization.context);
    if (canonical.ok && authorization.schema_version == 1)
        canonical.value.schema_version = 1;
    return canonical;
}
#endif

}  // namespace

PluginAuthorizationStoreResult save_plugin_authorization(
    const std::filesystem::path& plugin_store_root,
    const PluginAuthorization& authorization) {
#ifdef _WIN32
    const auto initialized = initialize_plugin_store(plugin_store_root);
    if (!initialized.ok) return failure(initialized.diagnostic);
    const auto installed = query_installed_plugin_version(
        plugin_store_root, authorization.plugin_id, authorization.version);
    if (!installed.ok) return failure(installed.diagnostic);
    const auto canonical = canonical_authorization(installed, authorization);
    if (!canonical.ok) return failure(canonical.diagnostic);
    const auto directory = plugin_store_root / L"authorizations" /
                           std::filesystem::path(authorization.plugin_id);
    if (!ensure_directory(directory)) return failure("cannot create safe authorization directory");
    const auto path = directory / std::filesystem::path(authorization.version + ".record");
    const auto bytes = serialize_plugin_authorization(canonical.value);
    if (bytes.empty() || !atomic_write(path, bytes))
        return failure("cannot atomically store plugin authorization");
    return {true, canonical.value, path, {}};
#else
    static_cast<void>(plugin_store_root); static_cast<void>(authorization);
    return failure("plugin authorization persistence is currently available on Windows only");
#endif
}

PluginAuthorizationStoreResult load_plugin_authorization(
    const std::filesystem::path& plugin_store_root, const std::string_view plugin_id,
    const std::string_view version) {
#ifdef _WIN32
    const auto installed = query_installed_plugin_version(plugin_store_root, plugin_id, version);
    if (!installed.ok) return failure(installed.diagnostic);
    const auto path = plugin_store_root / L"authorizations" /
                      std::filesystem::path(plugin_id) /
                      std::filesystem::path(std::string(version) + ".record");
    std::string bytes;
    if (!read_record(path, bytes)) return failure("authorization record is missing or unsafe");
    const auto parsed = parse_plugin_authorization(bytes);
    if (!parsed.ok) return failure(parsed.diagnostic);
    const auto canonical = canonical_authorization(installed, parsed.value);
    if (!canonical.ok || serialize_plugin_authorization(canonical.value) != bytes)
        return failure(canonical.ok ? "authorization record is not canonical" : canonical.diagnostic);
    return {true, canonical.value, path, {}};
#else
    static_cast<void>(plugin_store_root); static_cast<void>(plugin_id); static_cast<void>(version);
    return failure("plugin authorization persistence is currently available on Windows only");
#endif
}

}  // namespace owo::plugin

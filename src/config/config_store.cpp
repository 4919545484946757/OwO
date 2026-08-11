#include "owo/config/config_store.h"

#ifdef _WIN32
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <Windows.h>
#endif

#include <charconv>
#include <algorithm>
#include <fstream>
#include <limits>
#include <map>
#include <set>
#include <system_error>

namespace owo::config {
namespace {

constexpr std::uintmax_t kMaximumConfigBytes = 16U * 1024U;

bool parse_u32(const std::string_view text, std::uint32_t& output) {
    const auto result = std::from_chars(text.data(), text.data() + text.size(), output);
    return result.ec == std::errc{} && result.ptr == text.data() + text.size();
}

bool parse_bool(const std::string_view text, bool& output) {
    if (text == "true") {
        output = true;
        return true;
    }
    if (text == "false") {
        output = false;
        return true;
    }
    return false;
}

bool valid_primary_shortcut_key(const std::string_view key) {
    if (key.size() == 1 && ((key.front() >= 'A' && key.front() <= 'Z') ||
                            (key.front() >= '0' && key.front() <= '9'))) return true;
    constexpr std::string_view named[]{
        "Space", "Enter", "Tab", "Escape", "Backspace", "Delete", "Insert",
        "Home", "End", "PageUp", "PageDown", "Left", "Right", "Up", "Down",
        "[", "]", "Minus", "Plus", "Comma", "Period", "Slash", "Semicolon",
        "Quote", "Backtick"};
    if (std::find(std::begin(named), std::end(named), key) != std::end(named)) return true;
    if (key.size() >= 2 && key.front() == 'F') {
        std::uint32_t number{};
        return parse_u32(key.substr(1), number) && number >= 1 && number <= 24;
    }
    return false;
}

bool valid_shortcut(const std::string_view shortcut) {
    if (shortcut.empty() || shortcut.size() > 64) return false;
    bool control = false;
    bool alt = false;
    bool shift = false;
    std::string_view primary;
    std::size_t offset = 0;
    while (offset < shortcut.size()) {
        const auto separator = shortcut.find('+', offset);
        const auto token = shortcut.substr(
            offset, separator == std::string_view::npos ? shortcut.size() - offset
                                                        : separator - offset);
        if (token.empty()) return false;
        if (token == "Ctrl") {
            if (control) return false;
            control = true;
        } else if (token == "Alt") {
            if (alt) return false;
            alt = true;
        } else if (token == "Shift") {
            if (shift) return false;
            shift = true;
        } else {
            if (!primary.empty() || !valid_primary_shortcut_key(token)) return false;
            primary = token;
        }
        if (separator == std::string_view::npos) break;
        offset = separator + 1;
    }
    const auto modifier_count = static_cast<unsigned>(control) +
                                static_cast<unsigned>(alt) +
                                static_cast<unsigned>(shift);
    if (primary.empty() && shortcut != "Alt" && modifier_count < 2) return false;
    std::string canonical;
    const auto append = [&canonical](const std::string_view token) {
        if (!canonical.empty()) canonical.push_back('+');
        canonical += token;
    };
    if (control) append("Ctrl");
    if (alt) append("Alt");
    if (shift) append("Shift");
    if (!primary.empty()) append(primary);
    return canonical == shortcut;
}

bool parse_shortcut_list(const std::string_view text, std::vector<std::string>& output) {
    output.clear();
    std::size_t offset = 0;
    while (offset < text.size()) {
        const auto separator = text.find(';', offset);
        const auto item = text.substr(offset, separator == std::string_view::npos
            ? text.size() - offset : separator - offset);
        if (item.empty()) return false;
        output.emplace_back(item);
        if (separator == std::string_view::npos) break;
        offset = separator + 1;
    }
    return !output.empty();
}

std::string serialize_shortcut_list(const std::vector<std::string>& shortcuts) {
    std::string result;
    for (const auto& shortcut : shortcuts) {
        if (!result.empty()) result.push_back(';');
        result += shortcut;
    }
    return result;
}

ConfigParseResult read_config(const std::filesystem::path& path) {
    std::error_code error;
    const auto size = std::filesystem::file_size(path, error);
    if (error) return {false, {}, "configuration file is missing"};
    if (size == 0 || size > kMaximumConfigBytes)
        return {false, {}, "configuration file is empty or too large"};
    std::ifstream input(path, std::ios::binary);
    if (!input) return {false, {}, "cannot open configuration file"};
    std::string bytes(static_cast<std::size_t>(size), '\0');
    if (!input.read(bytes.data(), static_cast<std::streamsize>(bytes.size())))
        return {false, {}, "cannot read configuration file"};
    return parse_config(bytes);
}

bool write_durable(const std::filesystem::path& path, const std::string_view bytes) {
#ifdef _WIN32
    const HANDLE file = CreateFileW(path.c_str(), GENERIC_WRITE, 0, nullptr, CREATE_ALWAYS,
                                    FILE_ATTRIBUTE_NORMAL, nullptr);
    if (file == INVALID_HANDLE_VALUE) return false;
    std::size_t offset = 0;
    bool ok = true;
    while (offset < bytes.size()) {
        const auto remaining = bytes.size() - offset;
        const auto chunk = static_cast<DWORD>((std::min)(remaining,
            static_cast<std::size_t>((std::numeric_limits<DWORD>::max)())));
        DWORD written{};
        if (!WriteFile(file, bytes.data() + offset, chunk, &written, nullptr) || written != chunk) {
            ok = false;
            break;
        }
        offset += written;
    }
    if (ok && !FlushFileBuffers(file)) ok = false;
    CloseHandle(file);
    return ok;
#else
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
    output.flush();
    return static_cast<bool>(output);
#endif
}

bool replace_file(const std::filesystem::path& temporary, const std::filesystem::path& target) {
#ifdef _WIN32
    return MoveFileExW(temporary.c_str(), target.c_str(),
                       MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != FALSE;
#else
    std::error_code error;
    std::filesystem::rename(temporary, target, error);
    return !error;
#endif
}

}  // namespace

ConfigValidationResult validate_config(const AppConfig& value) {
    if (value.candidate_page_size < 1 || value.candidate_page_size > 9)
        return {false, "candidate_page_size must be between 1 and 9"};
    if (value.candidate_wrap_length < 4 || value.candidate_wrap_length > 64)
        return {false, "candidate_wrap_length must be between 4 and 64"};
    if (value.user_learning_sensitivity < 1 || value.user_learning_sensitivity > 10)
        return {false, "user_learning_sensitivity must be between 1 and 10"};
    if (value.model_timeout_ms < 5 || value.model_timeout_ms > 500)
        return {false, "model_timeout_ms must be between 5 and 500"};
    const auto valid_list = [](const std::vector<std::string>& shortcuts) {
        return !shortcuts.empty() && shortcuts.size() <= 16 &&
            std::all_of(shortcuts.begin(), shortcuts.end(), valid_shortcut);
    };
    if (!valid_list(value.correction_shortcuts) ||
        !valid_list(value.language_shortcuts) ||
        !valid_list(value.raw_input_shortcuts) ||
        !valid_list(value.cursor_left_shortcuts) ||
        !valid_list(value.cursor_right_shortcuts) ||
        !valid_list(value.previous_page_shortcuts) ||
        !valid_list(value.next_page_shortcuts))
        return {false, "shortcut list must contain canonical key or modifier combinations"};
    std::set<std::string_view> enabled_shortcuts;
    const auto unique_if_enabled = [&enabled_shortcuts](const bool enabled,
                                                         const std::vector<std::string>& shortcuts) {
        if (!enabled) return true;
        return std::all_of(shortcuts.begin(), shortcuts.end(), [&](const auto& shortcut) {
            return enabled_shortcuts.insert(shortcut).second;
        });
    };
    if (!unique_if_enabled(value.correction_shortcut_enabled, value.correction_shortcuts) ||
        !unique_if_enabled(value.language_shortcut_enabled, value.language_shortcuts) ||
        !unique_if_enabled(value.raw_input_shortcut_enabled, value.raw_input_shortcuts) ||
        !unique_if_enabled(value.cursor_left_shortcut_enabled, value.cursor_left_shortcuts) ||
        !unique_if_enabled(value.cursor_right_shortcut_enabled, value.cursor_right_shortcuts) ||
        !unique_if_enabled(value.previous_page_shortcut_enabled,
                           value.previous_page_shortcuts) ||
        !unique_if_enabled(value.next_page_shortcut_enabled,
                           value.next_page_shortcuts))
        return {false, "enabled shortcuts must be unique"};
    return {true, {}};
}

ConfigParseResult parse_config(const std::string_view utf8) {
    if (utf8.empty() || utf8.size() > kMaximumConfigBytes)
        return {false, {}, "configuration is empty or too large"};
    if (utf8.size() >= 3 && static_cast<unsigned char>(utf8[0]) == 0xef &&
        static_cast<unsigned char>(utf8[1]) == 0xbb &&
        static_cast<unsigned char>(utf8[2]) == 0xbf)
        return {false, {}, "UTF-8 BOM is forbidden"};
    std::map<std::string, std::string> fields;
    std::size_t offset = 0;
    while (offset < utf8.size()) {
        const auto end = utf8.find('\n', offset);
        auto line = utf8.substr(offset, end == std::string_view::npos ? utf8.size() - offset
                                                                      : end - offset);
        if (!line.empty() && line.back() == '\r') line.remove_suffix(1);
        if (line.empty()) return {false, {}, "blank configuration lines are forbidden"};
        const auto separator = line.find('=');
        if (separator == std::string_view::npos || separator == 0 || separator + 1 == line.size())
            return {false, {}, "invalid configuration field"};
        if (!fields.emplace(std::string(line.substr(0, separator)),
                            std::string(line.substr(separator + 1))).second)
            return {false, {}, "duplicate configuration field"};
        if (end == std::string_view::npos) break;
        offset = end + 1;
        if (offset == utf8.size()) break;
    }
    std::uint32_t version{};
    if (!fields.contains("schema_version") ||
        !parse_u32(fields["schema_version"], version) ||
        (version < 1 || version > kConfigSchemaVersion))
        return {false, {}, "unsupported configuration schema_version"};
    constexpr std::string_view base_required[]{"schema_version", "candidate_page_size",
        "user_learning_enabled", "model_ranking_enabled", "model_timeout_ms"};
    constexpr std::string_view shortcut_required[]{"correction_shortcut_enabled",
        "correction_shortcut", "language_shortcut_enabled", "language_shortcut",
        "raw_input_shortcut_enabled", "raw_input_shortcut"};
    constexpr std::string_view wrap_required[]{"candidate_wrap_length"};
    constexpr std::string_view learning_required[]{"user_learning_sensitivity"};
    constexpr std::string_view navigation_required[]{
        "cursor_left_shortcut_enabled", "cursor_left_shortcut",
        "cursor_right_shortcut_enabled", "cursor_right_shortcut",
        "previous_page_shortcut_enabled", "previous_page_shortcut",
        "next_page_shortcut_enabled", "next_page_shortcut"};
    const auto expected_size = std::size(base_required) +
                               (version >= 2 ? std::size(shortcut_required) : 0) +
                               (version >= 3 ? std::size(wrap_required) : 0) +
                               (version >= 4 ? std::size(learning_required) : 0) +
                               (version >= 5 ? std::size(navigation_required) : 0);
    if (fields.size() != expected_size)
        return {false, {}, "configuration fields are missing or unknown"};
    for (const auto key : base_required)
        if (!fields.contains(std::string(key)))
            return {false, {}, "configuration fields are missing or unknown"};
    if (version >= 2) {
        for (const auto key : shortcut_required)
            if (!fields.contains(std::string(key)))
                return {false, {}, "configuration fields are missing or unknown"};
    }
    if (version >= 3 && !fields.contains("candidate_wrap_length"))
        return {false, {}, "configuration fields are missing or unknown"};
    if (version >= 4 && !fields.contains("user_learning_sensitivity"))
        return {false, {}, "configuration fields are missing or unknown"};
    if (version >= 5) {
        for (const auto key : navigation_required)
            if (!fields.contains(std::string(key)))
                return {false, {}, "configuration fields are missing or unknown"};
    }
    AppConfig value;
    if (!parse_u32(fields["candidate_page_size"], value.candidate_page_size) ||
        !parse_bool(fields["user_learning_enabled"], value.user_learning_enabled) ||
        !parse_bool(fields["model_ranking_enabled"], value.model_ranking_enabled) ||
        !parse_u32(fields["model_timeout_ms"], value.model_timeout_ms))
        return {false, {}, "configuration field type is invalid"};
    if (version >= 2) {
        if (!parse_bool(fields["correction_shortcut_enabled"],
                        value.correction_shortcut_enabled) ||
            !parse_bool(fields["language_shortcut_enabled"],
                        value.language_shortcut_enabled) ||
            !parse_bool(fields["raw_input_shortcut_enabled"],
                        value.raw_input_shortcut_enabled))
            return {false, {}, "configuration field type is invalid"};
        const auto load_shortcuts = [version](const std::string& text,
                                              std::vector<std::string>& output) {
            if (version < 6) {
                output = {text};
                return true;
            }
            return parse_shortcut_list(text, output);
        };
        if (!load_shortcuts(fields["correction_shortcut"], value.correction_shortcuts) ||
            !load_shortcuts(fields["language_shortcut"], value.language_shortcuts) ||
            !load_shortcuts(fields["raw_input_shortcut"], value.raw_input_shortcuts))
            return {false, {}, "configuration shortcut list is invalid"};
    }
    if (version >= 3 &&
        !parse_u32(fields["candidate_wrap_length"], value.candidate_wrap_length))
        return {false, {}, "configuration field type is invalid"};
    if (version >= 4 &&
        !parse_u32(fields["user_learning_sensitivity"], value.user_learning_sensitivity))
        return {false, {}, "configuration field type is invalid"};
    if (version >= 5) {
        if (!parse_bool(fields["cursor_left_shortcut_enabled"],
                        value.cursor_left_shortcut_enabled) ||
            !parse_bool(fields["cursor_right_shortcut_enabled"],
                        value.cursor_right_shortcut_enabled) ||
            !parse_bool(fields["previous_page_shortcut_enabled"],
                        value.previous_page_shortcut_enabled) ||
            !parse_bool(fields["next_page_shortcut_enabled"],
                        value.next_page_shortcut_enabled))
            return {false, {}, "configuration field type is invalid"};
        const auto load_shortcuts = [version](const std::string& text,
                                              std::vector<std::string>& output) {
            if (version < 6) {
                output = {text};
                return true;
            }
            return parse_shortcut_list(text, output);
        };
        if (!load_shortcuts(fields["cursor_left_shortcut"], value.cursor_left_shortcuts) ||
            !load_shortcuts(fields["cursor_right_shortcut"], value.cursor_right_shortcuts) ||
            !load_shortcuts(fields["previous_page_shortcut"], value.previous_page_shortcuts) ||
            !load_shortcuts(fields["next_page_shortcut"], value.next_page_shortcuts))
            return {false, {}, "configuration shortcut list is invalid"};
        if (version < 6) {
            if (value.previous_page_shortcuts.front() != "[")
                value.previous_page_shortcuts.insert(value.previous_page_shortcuts.begin(), "[");
            if (value.next_page_shortcuts.front() != "]")
                value.next_page_shortcuts.insert(value.next_page_shortcuts.begin(), "]");
        }
    }
    const auto validation = validate_config(value);
    if (!validation.ok) return {false, {}, validation.diagnostic};
    return {true, value, {}};
}

std::string serialize_config(const AppConfig& value) {
    if (!validate_config(value).ok) return {};
    return "schema_version=" + std::to_string(kConfigSchemaVersion) +
           "\ncandidate_page_size=" + std::to_string(value.candidate_page_size) +
           "\ncandidate_wrap_length=" + std::to_string(value.candidate_wrap_length) +
           "\nuser_learning_enabled=" + (value.user_learning_enabled ? std::string("true") : "false") +
           "\nuser_learning_sensitivity=" +
               std::to_string(value.user_learning_sensitivity) +
           "\nmodel_ranking_enabled=" + (value.model_ranking_enabled ? std::string("true") : "false") +
           "\nmodel_timeout_ms=" + std::to_string(value.model_timeout_ms) +
           "\ncorrection_shortcut_enabled=" +
               (value.correction_shortcut_enabled ? std::string("true") : "false") +
           "\ncorrection_shortcut=" + serialize_shortcut_list(value.correction_shortcuts) +
           "\nlanguage_shortcut_enabled=" +
               (value.language_shortcut_enabled ? std::string("true") : "false") +
           "\nlanguage_shortcut=" + serialize_shortcut_list(value.language_shortcuts) +
           "\nraw_input_shortcut_enabled=" +
               (value.raw_input_shortcut_enabled ? std::string("true") : "false") +
           "\nraw_input_shortcut=" + serialize_shortcut_list(value.raw_input_shortcuts) +
           "\ncursor_left_shortcut_enabled=" +
               (value.cursor_left_shortcut_enabled ? std::string("true") : "false") +
           "\ncursor_left_shortcut=" + serialize_shortcut_list(value.cursor_left_shortcuts) +
           "\ncursor_right_shortcut_enabled=" +
               (value.cursor_right_shortcut_enabled ? std::string("true") : "false") +
           "\ncursor_right_shortcut=" + serialize_shortcut_list(value.cursor_right_shortcuts) +
           "\nprevious_page_shortcut_enabled=" +
               (value.previous_page_shortcut_enabled ? std::string("true") : "false") +
           "\nprevious_page_shortcut=" + serialize_shortcut_list(value.previous_page_shortcuts) +
           "\nnext_page_shortcut_enabled=" +
               (value.next_page_shortcut_enabled ? std::string("true") : "false") +
           "\nnext_page_shortcut=" + serialize_shortcut_list(value.next_page_shortcuts) + "\n";
}

ConfigIoResult ConfigStore::load(const std::filesystem::path& path) {
    path_ = path;
    const auto main = read_config(path_);
    if (main.ok) {
        const bool changed = generation_ == 0 || current_ != main.value;
        current_ = main.value;
        if (changed) ++generation_;
        return {true, false, false, changed, generation_, {}};
    }
    auto backup_path = path_;
    backup_path += L".bak";
    const auto backup = read_config(backup_path);
    if (backup.ok) {
        const bool changed = generation_ == 0 || current_ != backup.value;
        current_ = backup.value;
        if (changed) ++generation_;
        return {true, true, false, changed, generation_, "recovered configuration backup"};
    }
    const AppConfig defaults;
    const bool changed = generation_ == 0 || current_ != defaults;
    current_ = defaults;
    if (changed) ++generation_;
    const bool missing = !std::filesystem::exists(path_) && !std::filesystem::exists(backup_path);
    return {true, false, true, changed, generation_,
            missing ? "configuration missing; using defaults"
                    : "configuration and backup invalid; using defaults"};
}

ConfigIoResult ConfigStore::reload() {
    if (path_.empty()) return {false, false, false, false, generation_, "configuration path is not set"};
    const auto parsed = read_config(path_);
    if (!parsed.ok) return {false, false, false, false, generation_, parsed.diagnostic};
    const bool changed = current_ != parsed.value;
    if (changed) {
        current_ = parsed.value;
        ++generation_;
    }
    return {true, false, false, changed, generation_, {}};
}

ConfigIoResult ConfigStore::save(const AppConfig& value) {
    if (path_.empty()) return {false, false, false, false, generation_, "configuration path is not set"};
    const auto validation = validate_config(value);
    if (!validation.ok) return {false, false, false, false, generation_, validation.diagnostic};
    auto temporary = path_;
    temporary += L".tmp";
    auto backup = path_;
    backup += L".bak";
    const auto bytes = serialize_config(value);
    if (!write_durable(temporary, bytes)) {
        std::error_code ignored;
        std::filesystem::remove(temporary, ignored);
        return {false, false, false, false, generation_, "cannot durably write temporary configuration"};
    }
    const auto existing = read_config(path_);
    if (existing.ok) {
        auto backup_temporary = backup;
        backup_temporary += L".tmp";
        if (!write_durable(backup_temporary, serialize_config(existing.value)) ||
            !replace_file(backup_temporary, backup)) {
            std::error_code ignored;
            std::filesystem::remove(backup_temporary, ignored);
            std::filesystem::remove(temporary, ignored);
            return {false, false, false, false, generation_, "cannot update configuration backup"};
        }
    }
    if (!replace_file(temporary, path_)) {
        std::error_code ignored;
        std::filesystem::remove(temporary, ignored);
        return {false, false, false, false, generation_, "cannot atomically replace configuration"};
    }
    const bool changed = generation_ == 0 || current_ != value;
    current_ = value;
    if (changed) ++generation_;
    return {true, false, false, changed, generation_, {}};
}

}  // namespace owo::config

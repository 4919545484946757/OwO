#include "owo/plugin/plugin_manifest.h"
#include "owo/plugin/plugin_permissions.h"

#include <algorithm>
#include <charconv>
#include <cctype>
#include <fstream>
#include <limits>
#include <set>
#include <unordered_map>
#include <variant>

namespace owo::plugin {
namespace {

constexpr std::size_t kMaximumManifestBytes = 64U * 1024U;
using Value = std::variant<std::string, std::uint64_t, bool, std::vector<std::string>>;

class Parser final {
public:
    explicit Parser(const std::string_view input) : input_(input) {}

    bool object(std::unordered_map<std::string, Value>& fields) {
        space();
        if (!take('{')) return fail("manifest must be a JSON object");
        space();
        if (take('}')) return finish();
        while (true) {
            std::string key;
            if (!string(key)) return false;
            space();
            if (!take(':')) return fail("expected ':' after field name");
            space();
            Value value;
            if (!value_for(key, value)) return false;
            if (!fields.emplace(key, std::move(value)).second)
                return fail("duplicate field: " + key);
            space();
            if (take('}')) return finish();
            if (!take(',')) return fail("expected ',' or '}'");
            space();
        }
    }

    const std::string& error() const { return error_; }

private:
    bool finish() {
        space();
        return offset_ == input_.size() || fail("trailing data after manifest");
    }

    void space() {
        while (offset_ < input_.size() &&
               (input_[offset_] == ' ' || input_[offset_] == '\t' ||
                input_[offset_] == '\r' || input_[offset_] == '\n')) ++offset_;
    }

    bool take(const char expected) {
        if (offset_ >= input_.size() || input_[offset_] != expected) return false;
        ++offset_;
        return true;
    }

    bool string(std::string& output) {
        space();
        if (!take('"')) return fail("expected JSON string");
        while (offset_ < input_.size()) {
            const auto byte = static_cast<unsigned char>(input_[offset_++]);
            if (byte == '"') return true;
            if (byte < 0x20U) return fail("control character in JSON string");
            if (byte == '\\') {
                if (offset_ >= input_.size()) return fail("truncated JSON escape");
                const char escaped = input_[offset_++];
                if (escaped == '"' || escaped == '\\' || escaped == '/') output.push_back(escaped);
                else return fail("only quote, slash and backslash escapes are accepted in v1");
            } else {
                output.push_back(static_cast<char>(byte));
            }
        }
        return fail("unterminated JSON string");
    }

    bool unsigned_integer(std::uint64_t& output) {
        const auto begin = input_.data() + offset_;
        const auto end = input_.data() + input_.size();
        const auto parsed = std::from_chars(begin, end, output);
        if (parsed.ec != std::errc{} || parsed.ptr == begin) return fail("expected unsigned integer");
        offset_ += static_cast<std::size_t>(parsed.ptr - begin);
        return true;
    }

    bool boolean(bool& output) {
        if (input_.substr(offset_, 4) == "true") { offset_ += 4; output = true; return true; }
        if (input_.substr(offset_, 5) == "false") { offset_ += 5; output = false; return true; }
        return fail("expected boolean");
    }

    bool strings(std::vector<std::string>& output) {
        if (!take('[')) return fail("expected string array");
        space();
        if (take(']')) return true;
        while (true) {
            std::string item;
            if (!string(item)) return false;
            output.push_back(std::move(item));
            space();
            if (take(']')) return true;
            if (!take(',')) return fail("expected ',' or ']' in array");
            space();
        }
    }

    bool value_for(const std::string& key, Value& output) {
        static const std::set<std::string> string_fields{"id", "name", "version", "runtime", "entry",
                                                         "config_schema"};
        if (string_fields.contains(key)) { std::string value; if (!string(value)) return false; output = std::move(value); return true; }
        if (key == "api_version") { std::uint64_t value{}; if (!unsigned_integer(value)) return false; output = value; return true; }
        if (key == "network") { bool value{}; if (!boolean(value)) return false; output = value; return true; }
        if (key == "permissions") { std::vector<std::string> value; if (!strings(value)) return false; output = std::move(value); return true; }
        return fail("unknown field: " + key);
    }

    bool fail(std::string message) {
        if (error_.empty()) error_ = std::move(message) + " at byte " + std::to_string(offset_);
        return false;
    }

    std::string_view input_;
    std::size_t offset_{};
    std::string error_;
};

bool ascii_identifier(const std::string_view value) {
    if (value.size() < 3 || value.size() > 128 || value.front() == '.' || value.back() == '.' ||
        value.find('.') == std::string_view::npos) return false;
    bool previous_dot = false;
    for (const unsigned char byte : value) {
        const bool valid = std::islower(byte) != 0 || std::isdigit(byte) != 0 || byte == '-' || byte == '.';
        if (!valid || (byte == '.' && previous_dot)) return false;
        previous_dot = byte == '.';
    }
    return true;
}

bool valid_utf8(const std::string_view text) {
    std::size_t offset = 0;
    while (offset < text.size()) {
        const auto first = static_cast<unsigned char>(text[offset]);
        std::size_t count = 0;
        std::uint32_t scalar = 0;
        if (first <= 0x7fU) { count = 1; scalar = first; }
        else if (first >= 0xc2U && first <= 0xdfU) { count = 2; scalar = first & 0x1fU; }
        else if (first >= 0xe0U && first <= 0xefU) { count = 3; scalar = first & 0x0fU; }
        else if (first >= 0xf0U && first <= 0xf4U) { count = 4; scalar = first & 0x07U; }
        else return false;
        if (offset + count > text.size()) return false;
        for (std::size_t index = 1; index < count; ++index) {
            const auto next = static_cast<unsigned char>(text[offset + index]);
            if ((next & 0xc0U) != 0x80U) return false;
            scalar = (scalar << 6U) | (next & 0x3fU);
        }
        if ((count == 3 && scalar < 0x800U) || (count == 4 && scalar < 0x10000U) ||
            (scalar >= 0xd800U && scalar <= 0xdfffU) || scalar > 0x10ffffU) return false;
        offset += count;
    }
    return true;
}

bool semantic_version(const std::string_view value) {
    unsigned parts = 0;
    std::size_t start = 0;
    while (start < value.size()) {
        const auto end = value.find('.', start);
        const auto token = value.substr(start, end == std::string_view::npos ? value.size() - start : end - start);
        if (token.empty() || (token.size() > 1 && token.front() == '0') ||
            !std::all_of(token.begin(), token.end(), [](const unsigned char c) { return std::isdigit(c) != 0; })) return false;
        ++parts;
        if (end == std::string_view::npos) break;
        start = end + 1;
    }
    return parts == 3;
}

bool safe_relative_path(const std::string_view text, const std::string_view required_prefix,
                        const std::string_view required_extension) {
    if (text.empty() || text.size() > 256 || text.find('\\') != std::string_view::npos ||
        text.find(':') != std::string_view::npos || text.find("//") != std::string_view::npos ||
        !std::all_of(text.begin(), text.end(), [](const unsigned char byte) {
            return std::isalnum(byte) != 0 || byte == '.' || byte == '_' || byte == '-' || byte == '/';
        })) return false;
    const std::filesystem::path path(text);
    if (path.is_absolute() || text.starts_with('/') || text.find("..") != std::string_view::npos ||
        !text.starts_with(required_prefix) || !text.ends_with(required_extension) ||
        path.generic_string() != text) return false;
    return true;
}

template <typename T>
bool get(const std::unordered_map<std::string, Value>& fields, const char* key, T& output) {
    const auto found = fields.find(key);
    if (found == fields.end()) return false;
    const auto value = std::get_if<T>(&found->second);
    if (value == nullptr) return false;
    output = *value;
    return true;
}

}  // namespace

ManifestResult parse_manifest(const std::string_view json) {
    if (json.empty() || json.size() > kMaximumManifestBytes) return {false, {}, "manifest size is outside [1, 65536]"};
    if (json.size() >= 3 && static_cast<unsigned char>(json[0]) == 0xefU &&
        static_cast<unsigned char>(json[1]) == 0xbbU && static_cast<unsigned char>(json[2]) == 0xbfU)
        return {false, {}, "UTF-8 BOM is not accepted"};
    std::unordered_map<std::string, Value> fields;
    Parser parser(json);
    if (!parser.object(fields)) return {false, {}, parser.error()};
    if (fields.size() != 9) return {false, {}, "manifest must contain exactly the nine v1 fields"};
    PluginManifest result;
    std::uint64_t api_version{};
    if (!get(fields, "id", result.id) || !get(fields, "name", result.name) ||
        !get(fields, "version", result.version) || !get(fields, "api_version", api_version) ||
        !get(fields, "runtime", result.runtime) || !get(fields, "entry", result.entry) ||
        !get(fields, "permissions", result.permissions) || !get(fields, "network", result.network) ||
        !get(fields, "config_schema", result.config_schema))
        return {false, {}, "manifest is missing a required field"};
    if (!ascii_identifier(result.id)) return {false, {}, "invalid reverse-domain plugin id"};
    if (result.name.empty() || result.name.size() > 128 || !valid_utf8(result.name))
        return {false, {}, "plugin name must be valid UTF-8 within [1, 128] bytes"};
    if (!semantic_version(result.version)) return {false, {}, "version must be strict major.minor.patch"};
    if (api_version != kPluginApiVersion) return {false, {}, "unsupported plugin api_version"};
    result.api_version = static_cast<std::uint32_t>(api_version);
    if (result.runtime != "process") return {false, {}, "P3C v1 only accepts process runtime"};
    if (!safe_relative_path(result.entry, "bin/", ".exe")) return {false, {}, "entry must be a safe bin/*.exe path"};
    if (!safe_relative_path(result.config_schema, "", ".json")) return {false, {}, "config_schema must be a safe relative JSON path"};
    std::set<std::string> seen;
    for (const auto& permission : result.permissions) {
        if (!is_known_plugin_permission(permission))
            return {false, {}, "unknown permission: " + permission};
        if (!seen.insert(permission).second) return {false, {}, "duplicate permission: " + permission};
    }
    if (result.permissions.size() > 32) return {false, {}, "too many permissions"};
    const bool network_permission = std::find(result.permissions.begin(), result.permissions.end(),
                                              "network.client") != result.permissions.end();
    if (result.network != network_permission)
        return {false, {}, "network must exactly match the network.client permission"};
    const bool full_trust = std::find(result.permissions.begin(), result.permissions.end(),
                                      "system.full_trust") != result.permissions.end();
    if (plugin_permissions_require_full_trust(result.permissions) && !full_trust)
        return {false, {}, "requested capabilities require system.full_trust"};
    return {true, std::move(result), {}};
}

ManifestResult load_manifest(const std::filesystem::path& path) {
    std::error_code error;
    const auto size = std::filesystem::file_size(path, error);
    if (error || size == 0 || size > kMaximumManifestBytes) return {false, {}, "manifest file size is invalid"};
    std::ifstream input(path, std::ios::binary);
    if (!input) return {false, {}, "cannot open manifest"};
    std::string json(static_cast<std::size_t>(size), '\0');
    input.read(json.data(), static_cast<std::streamsize>(json.size()));
    if (!input || input.peek() != std::char_traits<char>::eof()) return {false, {}, "cannot read manifest exactly"};
    return parse_manifest(json);
}

}  // namespace owo::plugin

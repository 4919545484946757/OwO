#pragma once

#include <cstdint>
#include <filesystem>
#include <string>
#include <string_view>

namespace owo::config {

inline constexpr std::uint32_t kConfigSchemaVersion = 5;

struct AppConfig {
    std::uint32_t candidate_page_size{5};
    std::uint32_t candidate_wrap_length{12};
    bool user_learning_enabled{true};
    std::uint32_t user_learning_sensitivity{7};
    bool model_ranking_enabled{false};
    std::uint32_t model_timeout_ms{50};
    bool correction_shortcut_enabled{true};
    std::string correction_shortcut{"Alt"};
    bool language_shortcut_enabled{true};
    std::string language_shortcut{"Ctrl+Space"};
    bool raw_input_shortcut_enabled{true};
    std::string raw_input_shortcut{"Enter"};
    bool cursor_left_shortcut_enabled{true};
    std::string cursor_left_shortcut{"Shift+Left"};
    bool cursor_right_shortcut_enabled{true};
    std::string cursor_right_shortcut{"Shift+Right"};
    bool previous_page_shortcut_enabled{true};
    std::string previous_page_shortcut{"Shift+Up"};
    bool next_page_shortcut_enabled{true};
    std::string next_page_shortcut{"Shift+Down"};
    bool operator==(const AppConfig&) const = default;
};

struct ConfigValidationResult {
    bool ok{};
    std::string diagnostic;
};

struct ConfigParseResult {
    bool ok{};
    AppConfig value;
    std::string diagnostic;
};

struct ConfigIoResult {
    bool success{};
    bool recovered_from_backup{};
    bool used_defaults{};
    bool changed{};
    std::uint64_t generation{};
    std::string diagnostic;
};

[[nodiscard]] ConfigValidationResult validate_config(const AppConfig&);
[[nodiscard]] ConfigParseResult parse_config(std::string_view utf8);
[[nodiscard]] std::string serialize_config(const AppConfig&);

class ConfigStore final {
public:
    /// 首次加载允许从备份恢复；主文件和备份都不可用时返回默认配置及诊断。
    [[nodiscard]] ConfigIoResult load(const std::filesystem::path& path);
    /// 热加载只接受有效主文件；失败时保留当前不可变快照和 generation。
    [[nodiscard]] ConfigIoResult reload();
    /// 严格校验后原子保存；只用有效旧主文件更新备份。
    [[nodiscard]] ConfigIoResult save(const AppConfig& value);
    [[nodiscard]] const AppConfig& snapshot() const noexcept { return current_; }
    [[nodiscard]] std::uint64_t generation() const noexcept { return generation_; }

private:
    std::filesystem::path path_;
    AppConfig current_;
    std::uint64_t generation_{};
};

}  // namespace owo::config

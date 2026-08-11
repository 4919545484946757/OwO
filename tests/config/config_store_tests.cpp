#include "owo/config/config_store.h"

#include <filesystem>
#include <fstream>
#include <string>

namespace {

void write(const std::filesystem::path& path, const std::string_view text) {
    std::ofstream output(path, std::ios::binary | std::ios::trunc);
    output.write(text.data(), static_cast<std::streamsize>(text.size()));
}

}  // namespace

int main(const int argc, char** argv) {
    if (argc != 2) return 1;
    const std::filesystem::path root(argv[1]);
    std::error_code error;
    std::filesystem::remove_all(root, error);
    std::filesystem::create_directories(root, error);
    if (error) return 2;
    const auto path = root / "owo.conf";

    owo::config::ConfigStore store;
    const auto defaults = store.load(path);
    if (!defaults.success || !defaults.used_defaults || defaults.generation != 1 ||
        store.snapshot() != owo::config::AppConfig{}) return 3;

    auto changed = store.snapshot();
    changed.candidate_page_size = 7;
    changed.candidate_wrap_length = 18;
    changed.user_learning_enabled = false;
    changed.user_learning_sensitivity = 9;
    changed.model_ranking_enabled = true;
    changed.model_timeout_ms = 80;
    changed.correction_shortcuts = {"Ctrl+Alt+C", "F8"};
    changed.language_shortcuts = {"Ctrl+Shift+Space"};
    changed.raw_input_shortcut_enabled = false;
    changed.cursor_left_shortcuts = {"Ctrl+Shift+Left"};
    changed.cursor_right_shortcuts = {"Ctrl+Shift+Right"};
    changed.previous_page_shortcuts = {"[", "Ctrl+Shift+Up"};
    changed.next_page_shortcuts = {"]", "Ctrl+Shift+Down"};
    const auto first_save = store.save(changed);
    if (!first_save.success || !first_save.changed || first_save.generation != 2) return 4;
    changed.candidate_page_size = 8;
    if (!store.save(changed).success) return 5;
    if (std::filesystem::exists(path.wstring() + L".tmp") ||
        std::filesystem::exists(path.wstring() + L".bak.tmp")) return 6;

    owo::config::ConfigStore loaded;
    if (!loaded.load(path).success || loaded.snapshot() != changed) return 7;
    auto hot = changed;
    hot.model_timeout_ms = 90;
    write(path, owo::config::serialize_config(hot));
    const auto hot_reload = loaded.reload();
    if (!hot_reload.success || !hot_reload.changed || hot_reload.generation != 2 ||
        loaded.snapshot() != hot) return 8;
    const auto unchanged_reload = loaded.reload();
    if (!unchanged_reload.success || unchanged_reload.changed || unchanged_reload.generation != 2)
        return 9;
    write(path, "broken");
    const auto invalid_reload = loaded.reload();
    if (invalid_reload.success || invalid_reload.changed || loaded.snapshot() != hot ||
        loaded.generation() != 2) return 10;

    owo::config::ConfigStore recovered;
    const auto recovery = recovered.load(path);
    auto backup_value = changed;
    backup_value.candidate_page_size = 7;
    if (!recovery.success || !recovery.recovered_from_backup ||
        recovered.snapshot() != backup_value) return 11;

    write(path.wstring() + L".bak", "also broken");
    owo::config::ConfigStore fallback;
    const auto fallback_result = fallback.load(path);
    if (!fallback_result.success || !fallback_result.used_defaults ||
        fallback.snapshot() != owo::config::AppConfig{}) return 12;

    auto invalid_value = fallback.snapshot();
    invalid_value.candidate_page_size = 0;
    if (fallback.save(invalid_value).success || fallback.snapshot() != owo::config::AppConfig{} ||
        fallback.generation() != 1) return 13;
    invalid_value = fallback.snapshot();
    invalid_value.candidate_wrap_length = 3;
    if (fallback.save(invalid_value).success) return 23;
    invalid_value = fallback.snapshot();
    invalid_value.user_learning_sensitivity = 11;
    if (fallback.save(invalid_value).success) return 29;

    if (owo::config::parse_config("schema_version=2\ncandidate_page_size=5\n"
            "user_learning_enabled=true\nmodel_ranking_enabled=false\nmodel_timeout_ms=50\n").ok) return 14;
    if (owo::config::parse_config("schema_version=1\ncandidate_page_size=10\n"
            "user_learning_enabled=true\nmodel_ranking_enabled=false\nmodel_timeout_ms=50\n").ok) return 15;
    if (owo::config::parse_config("schema_version=1\ncandidate_page_size=5\n"
            "user_learning_enabled=yes\nmodel_ranking_enabled=false\nmodel_timeout_ms=50\n").ok) return 16;
    if (owo::config::parse_config("schema_version=1\ncandidate_page_size=5\n"
            "user_learning_enabled=true\nmodel_ranking_enabled=false\nmodel_timeout_ms=50\nunknown=x\n").ok) return 17;
    if (owo::config::parse_config("\xef\xbb\xbfschema_version=1\n").ok) return 18;

    const auto legacy = owo::config::parse_config(
        "schema_version=1\ncandidate_page_size=6\nuser_learning_enabled=false\n"
        "model_ranking_enabled=true\nmodel_timeout_ms=75\n");
    if (!legacy.ok || legacy.value.candidate_page_size != 6 ||
        legacy.value.correction_shortcuts != std::vector<std::string>{"Ctrl+Alt+Shift"} ||
        legacy.value.language_shortcuts != std::vector<std::string>{"Ctrl+Space"} ||
        legacy.value.raw_input_shortcuts != std::vector<std::string>{"Enter"} ||
        legacy.value.cursor_left_shortcuts != std::vector<std::string>{"Shift+Left"} ||
        legacy.value.next_page_shortcuts != std::vector<std::string>{"]", "Shift+Down"} ||
        legacy.value.candidate_wrap_length != 12 ||
        legacy.value.user_learning_sensitivity != 7) return 20;

    const auto version_two = owo::config::parse_config(
        "schema_version=2\ncandidate_page_size=5\nuser_learning_enabled=true\n"
        "model_ranking_enabled=false\nmodel_timeout_ms=50\n"
        "correction_shortcut_enabled=true\ncorrection_shortcut=Alt\n"
        "language_shortcut_enabled=true\nlanguage_shortcut=Ctrl+Space\n"
        "raw_input_shortcut_enabled=true\nraw_input_shortcut=Enter\n");
    if (!version_two.ok || version_two.value.candidate_wrap_length != 12 ||
        version_two.value.user_learning_sensitivity != 7) return 24;

    auto duplicate_shortcuts = changed;
    duplicate_shortcuts.raw_input_shortcut_enabled = true;
    duplicate_shortcuts.raw_input_shortcuts = duplicate_shortcuts.language_shortcuts;
    if (owo::config::validate_config(duplicate_shortcuts).ok) return 21;
    auto noncanonical_shortcut = changed;
    noncanonical_shortcut.correction_shortcuts = {"Alt+Ctrl+C"};
    if (owo::config::validate_config(noncanonical_shortcut).ok) return 22;
    noncanonical_shortcut = changed;
    noncanonical_shortcut.correction_shortcuts = {"Ctrl"};
    if (owo::config::validate_config(noncanonical_shortcut).ok) return 25;
    noncanonical_shortcut.correction_shortcuts = {"Shift"};
    if (owo::config::validate_config(noncanonical_shortcut).ok) return 26;
    noncanonical_shortcut.correction_shortcuts = {"Ctrl+Alt"};
    if (!owo::config::validate_config(noncanonical_shortcut).ok) return 27;
    noncanonical_shortcut.correction_shortcuts = {"Alt"};
    if (!owo::config::validate_config(noncanonical_shortcut).ok) return 28;

    auto duplicate_navigation = changed;
    duplicate_navigation.cursor_right_shortcuts =
        duplicate_navigation.cursor_left_shortcuts;
    if (owo::config::validate_config(duplicate_navigation).ok) return 30;

    const auto stable = owo::config::serialize_config(changed);
    const auto round_trip = owo::config::parse_config(stable);
    if (!round_trip.ok || round_trip.value != changed ||
        stable != owo::config::serialize_config(round_trip.value)) return 19;

    std::filesystem::remove_all(root, error);
    return 0;
}

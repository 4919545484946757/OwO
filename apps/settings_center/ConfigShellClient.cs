using System.Diagnostics;

namespace OwO_Settings;

internal sealed record SettingsSnapshot(uint CandidatePageSize, uint CandidateWrapLength,
                                        uint UserLearningSensitivity,
                                        bool UserLearningEnabled,
                                        bool ModelRankingEnabled, uint ModelTimeoutMs,
                                        bool CorrectionShortcutEnabled, IReadOnlyList<string> CorrectionShortcuts,
                                        bool LanguageShortcutEnabled, IReadOnlyList<string> LanguageShortcuts,
                                        bool RawInputShortcutEnabled, IReadOnlyList<string> RawInputShortcuts,
                                        bool CursorLeftShortcutEnabled, IReadOnlyList<string> CursorLeftShortcuts,
                                        bool CursorRightShortcutEnabled, IReadOnlyList<string> CursorRightShortcuts,
                                        bool PreviousPageShortcutEnabled, IReadOnlyList<string> PreviousPageShortcuts,
                                        bool NextPageShortcutEnabled, IReadOnlyList<string> NextPageShortcuts);

internal sealed class ConfigShellClient
{
    private readonly string _shellPath = Environment.GetEnvironmentVariable("OWO_CONFIG_SHELL_PATH")
        ?? Path.Combine(AppContext.BaseDirectory, "owo_config_shell.exe");
    private readonly string _configPath = Environment.GetEnvironmentVariable("OWO_CONFIG_PATH")
        ?? Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                        "OwO", "InputMethod", "config", "owo.conf");

    internal string ConfigPath => _configPath;

    internal async Task<SettingsSnapshot> LoadAsync(CancellationToken cancellationToken = default)
    {
        var result = await RunAsync([_configPath, "show"], cancellationToken);
        var values = result.Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => line.TrimEnd('\r').Split('=', 2)).Where(parts => parts.Length == 2)
            .ToDictionary(parts => parts[0], parts => parts[1], StringComparer.Ordinal);
        return new(uint.Parse(values["candidate_page_size"]),
                   uint.Parse(values["candidate_wrap_length"]),
                   uint.Parse(values["user_learning_sensitivity"]),
                   bool.Parse(values["user_learning_enabled"]),
                   bool.Parse(values["model_ranking_enabled"]),
                   uint.Parse(values["model_timeout_ms"]),
                   bool.Parse(values["correction_shortcut_enabled"]),
                   SplitShortcuts(values["correction_shortcut"]),
                   bool.Parse(values["language_shortcut_enabled"]),
                   SplitShortcuts(values["language_shortcut"]),
                   bool.Parse(values["raw_input_shortcut_enabled"]),
                   SplitShortcuts(values["raw_input_shortcut"]),
                   bool.Parse(values["cursor_left_shortcut_enabled"]),
                   SplitShortcuts(values["cursor_left_shortcut"]),
                   bool.Parse(values["cursor_right_shortcut_enabled"]),
                   SplitShortcuts(values["cursor_right_shortcut"]),
                   bool.Parse(values["previous_page_shortcut_enabled"]),
                   SplitShortcuts(values["previous_page_shortcut"]),
                   bool.Parse(values["next_page_shortcut_enabled"]),
                   SplitShortcuts(values["next_page_shortcut"]));
    }

    internal Task SaveAsync(SettingsSnapshot value, CancellationToken cancellationToken = default) =>
        RunAsync([_configPath, "set-all", value.CandidatePageSize.ToString(),
                  value.UserLearningEnabled.ToString().ToLowerInvariant(),
                  value.ModelRankingEnabled.ToString().ToLowerInvariant(),
                  value.ModelTimeoutMs.ToString(),
                  value.CorrectionShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.CorrectionShortcuts),
                  value.LanguageShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.LanguageShortcuts),
                  value.RawInputShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.RawInputShortcuts),
                  value.CandidateWrapLength.ToString(),
                  value.UserLearningSensitivity.ToString(),
                  value.CursorLeftShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.CursorLeftShortcuts),
                  value.CursorRightShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.CursorRightShortcuts),
                  value.PreviousPageShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.PreviousPageShortcuts),
                  value.NextPageShortcutEnabled.ToString().ToLowerInvariant(),
                  JoinShortcuts(value.NextPageShortcuts)], cancellationToken);

    private static string[] SplitShortcuts(string value) =>
        value.Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);

    private static string JoinShortcuts(IReadOnlyList<string> value) => string.Join(';', value);

    private async Task<string> RunAsync(IEnumerable<string> arguments,
                                        CancellationToken cancellationToken)
    {
        if (!File.Exists(_shellPath))
            throw new FileNotFoundException("找不到 OwO 配置后端。", _shellPath);
        Directory.CreateDirectory(Path.GetDirectoryName(_configPath)!);
        var start = new ProcessStartInfo(_shellPath) {
            UseShellExecute = false, CreateNoWindow = true,
            RedirectStandardOutput = true, RedirectStandardError = true,
        };
        foreach (var argument in arguments) start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("无法启动配置后端。");
        var output = process.StandardOutput.ReadToEndAsync(cancellationToken);
        var error = process.StandardError.ReadToEndAsync(cancellationToken);
        await process.WaitForExitAsync(cancellationToken);
        if (process.ExitCode != 0) {
            var message = (await error).Trim();
            throw new InvalidOperationException(message.Length > 0 ? message :
                $"配置后端退出码：{process.ExitCode}");
        }
        return await output;
    }
}

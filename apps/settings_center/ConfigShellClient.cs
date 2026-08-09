using System.Diagnostics;

namespace OwO_Settings;

internal sealed record SettingsSnapshot(uint CandidatePageSize, uint CandidateWrapLength,
                                        bool UserLearningEnabled,
                                        bool ModelRankingEnabled, uint ModelTimeoutMs,
                                        bool CorrectionShortcutEnabled, string CorrectionShortcut,
                                        bool LanguageShortcutEnabled, string LanguageShortcut,
                                        bool RawInputShortcutEnabled, string RawInputShortcut);

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
                   bool.Parse(values["user_learning_enabled"]),
                   bool.Parse(values["model_ranking_enabled"]),
                   uint.Parse(values["model_timeout_ms"]),
                   bool.Parse(values["correction_shortcut_enabled"]),
                   values["correction_shortcut"],
                   bool.Parse(values["language_shortcut_enabled"]),
                   values["language_shortcut"],
                   bool.Parse(values["raw_input_shortcut_enabled"]),
                   values["raw_input_shortcut"]);
    }

    internal Task SaveAsync(SettingsSnapshot value, CancellationToken cancellationToken = default) =>
        RunAsync([_configPath, "set-all", value.CandidatePageSize.ToString(),
                  value.UserLearningEnabled.ToString().ToLowerInvariant(),
                  value.ModelRankingEnabled.ToString().ToLowerInvariant(),
                  value.ModelTimeoutMs.ToString(),
                  value.CorrectionShortcutEnabled.ToString().ToLowerInvariant(),
                  value.CorrectionShortcut,
                  value.LanguageShortcutEnabled.ToString().ToLowerInvariant(),
                  value.LanguageShortcut,
                  value.RawInputShortcutEnabled.ToString().ToLowerInvariant(),
                  value.RawInputShortcut,
                  value.CandidateWrapLength.ToString()], cancellationToken);

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

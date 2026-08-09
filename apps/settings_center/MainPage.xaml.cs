using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using System.Runtime.InteropServices;
using Windows.System;
using Windows.Storage.Pickers;

namespace OwO_Settings;

public sealed partial class MainPage : Page
{
    private readonly ConfigShellClient _client = new();
    private readonly PluginShellClient _pluginClient = new();
    private Button? _shortcutCaptureTarget;
    private string _correctionShortcut = "Alt";
    private string _languageShortcut = "Ctrl+Space";
    private string _rawInputShortcut = "Enter";

    [DllImport("user32.dll")]
    private static extern short GetKeyState(int virtualKey);

    public MainPage()
    {
        InitializeComponent();
        ConfigPath.Text = _client.ConfigPath;
        PluginPath.Text = _pluginClient.StorePath;
        Loaded += async (_, _) => {
            await LoadConfigAsync();
            await LoadPluginsAsync();
        };
    }

    private async Task LoadConfigAsync()
    {
        SetBusy(true);
        try {
            var value = await _client.LoadAsync();
            CandidatePageSize.Value = value.CandidatePageSize;
            CandidateWrapLength.Value = value.CandidateWrapLength;
            UserLearning.IsOn = value.UserLearningEnabled;
            ModelRanking.IsOn = value.ModelRankingEnabled;
            ModelTimeout.Value = value.ModelTimeoutMs;
            CorrectionShortcutEnabled.IsOn = value.CorrectionShortcutEnabled;
            LanguageShortcutEnabled.IsOn = value.LanguageShortcutEnabled;
            RawInputShortcutEnabled.IsOn = value.RawInputShortcutEnabled;
            _correctionShortcut = value.CorrectionShortcut;
            _languageShortcut = value.LanguageShortcut;
            _rawInputShortcut = value.RawInputShortcut;
            UpdateShortcutButtons();
            ShowStatus("配置已加载", InfoBarSeverity.Success);
        } catch (Exception error) {
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async void SaveButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        SetBusy(true);
        try {
            ValidateShortcutConflicts();
            var value = new SettingsSnapshot((uint)CandidatePageSize.Value,
                (uint)CandidateWrapLength.Value,
                UserLearning.IsOn, ModelRanking.IsOn, (uint)ModelTimeout.Value,
                CorrectionShortcutEnabled.IsOn, _correctionShortcut,
                LanguageShortcutEnabled.IsOn, _languageShortcut,
                RawInputShortcutEnabled.IsOn, _rawInputShortcut);
            await _client.SaveAsync(value);
            ShowStatus("配置已保存，Core Service 将自动应用。", InfoBarSeverity.Success);
        } catch (Exception error) {
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async void ReloadButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        await LoadConfigAsync();

    private void ShortcutButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not Button button) return;
        _shortcutCaptureTarget = button;
        button.Content = "请按新的快捷键…";
        button.Focus(Microsoft.UI.Xaml.FocusState.Programmatic);
    }

    private void Page_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (_shortcutCaptureTarget is null) return;
        var shortcut = ShortcutForKeyEvent(e.Key);
        if (shortcut is null) {
            ShowStatus("该按键暂不支持，请使用字母、数字、功能键、导航键或常用符号键。",
                       InfoBarSeverity.Warning);
            e.Handled = true;
            return;
        }
        switch (_shortcutCaptureTarget.Tag?.ToString()) {
            case "correction": _correctionShortcut = shortcut; break;
            case "language": _languageShortcut = shortcut; break;
            case "raw": _rawInputShortcut = shortcut; break;
        }
        _shortcutCaptureTarget = null;
        UpdateShortcutButtons();
        ShowStatus($"快捷键已改为 {shortcut}，点击“保存”后生效。", InfoBarSeverity.Informational);
        e.Handled = true;
    }

    private static bool IsDown(int key) => (GetKeyState(key) & 0x8000) != 0;

    private static string? ShortcutForKeyEvent(VirtualKey key)
    {
        const int shiftKey = 0x10;
        const int controlKey = 0x11;
        const int altKey = 0x12;
        var code = (int)key;
        var control = code is 0x11 or 0xA2 or 0xA3 || IsDown(controlKey);
        var alt = code is 0x12 or 0xA4 or 0xA5 || IsDown(altKey);
        var shift = code is 0x10 or 0xA0 or 0xA1 || IsDown(shiftKey);
        var parts = new List<string>();
        if (control) parts.Add("Ctrl");
        if (alt) parts.Add("Alt");
        if (shift) parts.Add("Shift");
        if (code is not (0x10 or 0x11 or 0x12 or 0xA0 or 0xA1 or 0xA2 or 0xA3 or 0xA4 or 0xA5)) {
            var primary = PrimaryKeyName(code);
            if (primary is null) return null;
            parts.Add(primary);
        }
        return parts.Count == 0 ? null : string.Join('+', parts);
    }

    private static string? PrimaryKeyName(int key)
    {
        if (key is >= 0x41 and <= 0x5A || key is >= 0x30 and <= 0x39)
            return ((char)key).ToString();
        if (key is >= 0x70 and <= 0x87) return $"F{key - 0x6F}";
        return key switch {
            0x20 => "Space", 0x0D => "Enter", 0x09 => "Tab", 0x1B => "Escape",
            0x08 => "Backspace", 0x2E => "Delete", 0x2D => "Insert",
            0x24 => "Home", 0x23 => "End", 0x21 => "PageUp", 0x22 => "PageDown",
            0x25 => "Left", 0x27 => "Right", 0x26 => "Up", 0x28 => "Down",
            0xDB => "[", 0xDD => "]", 0xBD => "Minus", 0xBB => "Plus",
            0xBC => "Comma", 0xBE => "Period", 0xBF => "Slash",
            0xBA => "Semicolon", 0xDE => "Quote", 0xC0 => "Backtick", _ => null,
        };
    }

    private void UpdateShortcutButtons()
    {
        CorrectionShortcutButton.Content = _correctionShortcut;
        LanguageShortcutButton.Content = _languageShortcut;
        RawInputShortcutButton.Content = _rawInputShortcut;
    }

    private void ValidateShortcutConflicts()
    {
        var shortcuts = new List<string>();
        if (CorrectionShortcutEnabled.IsOn) shortcuts.Add(_correctionShortcut);
        if (LanguageShortcutEnabled.IsOn) shortcuts.Add(_languageShortcut);
        if (RawInputShortcutEnabled.IsOn) shortcuts.Add(_rawInputShortcut);
        if (shortcuts.Count != shortcuts.Distinct(StringComparer.Ordinal).Count())
            throw new InvalidOperationException("启用的快捷键不能重复。");
    }

    private async Task LoadPluginsAsync()
    {
        SetBusy(true);
        try {
            var value = await _pluginClient.LoadAsync();
            PluginVersions.ItemsSource = value.Plugins;
            RecoveryItems.ItemsSource = value.Recovery;
            NoPlugins.Visibility = value.Plugins.Count == 0
                ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;
            NoRecovery.Visibility = value.Recovery.Count == 0
                ? Microsoft.UI.Xaml.Visibility.Visible : Microsoft.UI.Xaml.Visibility.Collapsed;
            ShowStatus($"插件状态已刷新：{value.Plugins.Count} 个版本，{value.Recovery.Count} 个恢复项。",
                       InfoBarSeverity.Success);
        } catch (Exception error) {
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async void PluginReloadButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e) =>
        await LoadPluginsAsync();

    private async void PluginInstallButton_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        var picker = new FileOpenPicker {
            SuggestedStartLocation = PickerLocationId.Downloads,
            ViewMode = PickerViewMode.List,
        };
        picker.FileTypeFilter.Add(".owopkg");
        var window = ((App)Microsoft.UI.Xaml.Application.Current).MainWindow;
        WinRT.Interop.InitializeWithWindow.Initialize(
            picker, WinRT.Interop.WindowNative.GetWindowHandle(window));
        var package = await picker.PickSingleFileAsync();
        if (package is null) return;

        SetBusy(true);
        try {
            var preview = await _pluginClient.InspectInstallAsync(package.Path);
            PluginInstallSnapshot result;
            if (preview.RequiresRiskConsent) {
                SetBusy(false);
                if (!await ConfirmRiskInstallAsync(package.Name, package.Path, preview)) return;
                SetBusy(true);
                result = await _pluginClient.InstallRiskAsync(package.Path, preview.InventorySha256);
            } else {
                SetBusy(false);
                var message = $"{package.Name}\n{package.Path}\n\n"
                    + $"插件：{preview.Name} {preview.Version}（{preview.PluginId}）\n"
                    + $"发布者：{PluginUiText.Trust(preview.TrustTier)}\n"
                    + "系统将重新核对精确包摘要，随后原子安装并立即启用。";
                if (!await ConfirmAsync("安装插件包", message, "验证并安装")) return;
                SetBusy(true);
                result = await _pluginClient.InstallAsync(package.Path);
            }
            await LoadPluginsAsync();
            var publisher = string.IsNullOrWhiteSpace(result.PublisherDisplayName)
                ? PluginUiText.Trust(result.TrustTier) : result.PublisherDisplayName;
            var state = result.Activated ? "已安装并启用" : "已安装但保持停用";
            ShowStatus($"{state} {result.Name} {result.Version}（{result.PluginId}）；发布者：{publisher}。",
                       InfoBarSeverity.Success);
        } catch (Exception error) {
            await LoadPluginsAsync();
            ShowStatus($"插件安装失败：{error.Message}", InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async Task<bool> ConfirmRiskInstallAsync(
        string packageName, string packagePath, PluginInstallPreviewSnapshot preview)
    {
        var permissions = preview.Permissions.Count == 0 ? "（未申请具名权限）"
            : string.Join("\n", preview.Permissions.Select(
                permission => $"• {PluginUiText.Permission(permission)}（{permission}）"));
        var trustDetail = string.IsNullOrWhiteSpace(preview.TrustDiagnostic)
            ? "未能建立 Windows 发布者信任。" : preview.TrustDiagnostic;
        var acknowledgement = new CheckBox {
            Content = "我已阅读并理解：该插件可能造成隐私泄露、文件或账户损失，风险由我承担。",
        };
        var content = new StackPanel { Spacing = 12, MaxWidth = 620 };
        content.Children.Add(new TextBlock {
            Text = $"{packageName}\n{packagePath}\n\n"
                + $"插件：{preview.Name} {preview.Version}（{preview.PluginId}）\n"
                + $"风险：{PluginUiText.Risk(preview.RiskLevel)}\n"
                + $"信任：{PluginUiText.Trust(preview.TrustTier)}\n"
                + $"包摘要：{preview.InventorySha256}\n\n"
                + $"申请的能力：\n{permissions}\n\n"
                + $"验证说明：{trustDetail}\n\n"
                + "风险与免责：第三方插件可能读取、修改、删除或上传你的数据，捕获屏幕/音频、"
                + "覆盖界面、启动程序或以当前 Windows 用户权限执行操作。OwO 项目不代表已审计"
                + "第三方代码，也不保证其安全性、可用性或数据可恢复性。继续安装表示你理解并自行承担后果。"
                + "本授权只绑定上方精确版本与包摘要，更新或新增权限必须重新授权。",
            TextWrapping = Microsoft.UI.Xaml.TextWrapping.Wrap,
        });
        content.Children.Add(acknowledgement);
        var disclosure = new ContentDialog {
            XamlRoot = XamlRoot,
            Title = "高风险插件安全告知",
            Content = new ScrollViewer {
                Content = content,
                MaxHeight = 520,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            },
            PrimaryButtonText = "我已理解风险",
            CloseButtonText = "取消",
            DefaultButton = ContentDialogButton.Close,
            IsPrimaryButtonEnabled = false,
        };
        acknowledgement.Checked += (_, _) => disclosure.IsPrimaryButtonEnabled = true;
        acknowledgement.Unchecked += (_, _) => disclosure.IsPrimaryButtonEnabled = false;
        if (await disclosure.ShowAsync() != ContentDialogResult.Primary) return false;
        return await ConfirmAsync("单独授权安装",
            $"只授权安装此包：\n{preview.Name} {preview.Version}\n{preview.PluginId}\n"
                + $"SHA-256 清单摘要：{preview.InventorySha256}\n\n"
                + "安装后将保持停用；启用前仍可撤销权限或卸载。",
            "授权并安装");
    }

    private async Task<bool> ConfirmAsync(string title, string message, string action)
    {
        var dialog = new ContentDialog {
            XamlRoot = XamlRoot,
            Title = title,
            Content = message,
            PrimaryButtonText = action,
            CloseButtonText = "取消",
            DefaultButton = ContentDialogButton.Close,
        };
        return await dialog.ShowAsync() == ContentDialogResult.Primary;
    }

    private async void PluginVersionAction_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not Button { Tag: PluginVersionSnapshot plugin }) return;
        var action = plugin.Active ? "停用" : "启用";
        var warning = plugin.Active ? "" : $"\n\n{PluginUiText.Risk(plugin.RiskLevel)} · "
            + $"{PluginUiText.Trust(plugin.TrustTier)}\n"
            + (plugin.Permissions.Count == 0 ? "未申请具名权限" :
               $"已授权：{string.Join("、", plugin.Permissions.Select(PluginUiText.Permission))}");
        if (!await ConfirmAsync($"{action}插件",
            $"{plugin.Name} {plugin.Version}\n{plugin.Id}{warning}", action))
            return;
        SetBusy(true);
        try {
            if (plugin.Active) await _pluginClient.DeactivateAsync(plugin.Id, plugin.Version);
            else await _pluginClient.ActivateAsync(plugin.Id, plugin.Version);
            await LoadPluginsAsync();
            ShowStatus($"插件已{action}。", InfoBarSeverity.Success);
        } catch (Exception error) {
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async void PluginVersionRevoke_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not Button { Tag: PluginVersionSnapshot plugin } || !plugin.CanRevoke) return;
        var message = $"{plugin.Name} {plugin.Version}\n{plugin.Id}\n\n"
            + "将撤销此精确版本的全部运行权限。插件文件与数据保留；已运行的插件应先停用，"
            + "再次授权需要重新安装该精确包。";
        if (!await ConfirmAsync("撤销插件权限", message, "撤销权限")) return;
        SetBusy(true);
        try {
            if (plugin.Active) await _pluginClient.DeactivateAsync(plugin.Id, plugin.Version);
            await _pluginClient.RevokeAsync(plugin.Id, plugin.Version);
            await LoadPluginsAsync();
            ShowStatus("插件权限已撤销，插件已停用。", InfoBarSeverity.Success);
        } catch (Exception error) {
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async void PluginVersionUninstall_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not Button { Tag: PluginVersionSnapshot plugin } || plugin.Active) return;
        var message = $"{plugin.Name} {plugin.Version}\n{plugin.Id}\n\n"
            + "此操作会删除该版本及其精确授权，无法撤销。插件用户数据会保留。";
        if (!await ConfirmAsync("卸载插件版本", message, "卸载")) return;
        SetBusy(true);
        try {
            await _pluginClient.UninstallAsync(plugin.Id, plugin.Version);
            await LoadPluginsAsync();
            ShowStatus("插件版本已卸载；用户数据已保留。", InfoBarSeverity.Success);
        } catch (Exception error) {
            await LoadPluginsAsync();
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private async void RecoveryAction_Click(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (sender is not Button { Tag: PluginRecoverySnapshot item } || !item.CanApply) return;
        var activating = item.Action == "activate";
        var action = activating ? "切换版本" : "清理";
        if (!await ConfirmAsync(action, $"{item.Title}\n{item.Detail}", action)) return;
        SetBusy(true);
        try {
            if (activating) await _pluginClient.ActivateAsync(item.PluginId, item.Version);
            else await _pluginClient.CleanupAsync(item);
            await LoadPluginsAsync();
            ShowStatus($"恢复操作“{action}”已完成。", InfoBarSeverity.Success);
        } catch (Exception error) {
            ShowStatus(error.Message, InfoBarSeverity.Error);
        } finally {
            SetBusy(false);
        }
    }

    private void SetBusy(bool busy)
    {
        SaveButton.IsEnabled = !busy;
        ShortcutSection.IsEnabled = !busy;
        PluginSection.IsEnabled = !busy;
        Status.IsOpen = true;
    }

    private void ShowStatus(string message, InfoBarSeverity severity)
    {
        Status.Title = message;
        Status.Severity = severity;
        Status.IsOpen = true;
    }
}

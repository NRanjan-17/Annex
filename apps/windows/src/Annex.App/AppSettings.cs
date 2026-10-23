using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.Win32;

namespace Annex.App;

/// <summary>User preferences in %LOCALAPPDATA%\Annex\settings.json (removed by the uninstaller).</summary>
public sealed class AppSettings
{
    /// <summary>The name other devices see; null = the PC's name.</summary>
    public string? DeviceName { get; set; }
    public bool PreferHardwareEncoder { get; set; } = true;
    /// <summary>Graphics card for sharing (adapter LUID); 0 = automatic.</summary>
    public ulong GpuLuid { get; set; }
    public bool StartSharingAtLaunch { get; set; }
    /// <summary>Viewer: ask hosts to let this PC's mouse and keyboard control them. On by default: the
    /// sharing computer still decides per device (Paired devices), so asking is harmless.</summary>
    public bool UseMouseAndKeyboard { get; set; } = true;
    public bool ExtendByDefault { get; set; }
    /// <summary>Viewer: move each new session onto this PC's hotspot right after it connects.</summary>
    public bool ConnectOverDirectWifi { get; set; }
    /// <summary>Show identity codes, addresses and connection figures (Settings → Developer).</summary>
    public bool ShowTechnicalDetails { get; set; }
    /// <summary>Look for updates at launch and once a day.</summary>
    public bool CheckForUpdates { get; set; } = true;
    /// <summary>Include pre-releases; off goes back to the latest normal release.</summary>
    public bool BetaUpdates { get; set; }
    /// <summary>The update the user chose "Not now" for (the banner returns for a newer one).</summary>
    public string? DismissedUpdate { get; set; }
    /// <summary>When Annex last finished checking for updates (shown on the About page).</summary>
    public DateTimeOffset? LastUpdateCheck { get; set; }
    /// <summary>Write annex.log for bug reports. Off by default.</summary>
    public bool SaveLog { get; set; }
    /// <summary>Viewer: bitrate ceiling asked of hosts (Fast suits Wi-Fi).</summary>
    public Annex.Interop.Quality Quality { get; set; } = Annex.Interop.Quality.Best;

    private static string FilePath => Path.Combine(MainWindow.DataDirectory, "settings.json");

    public static AppSettings Load()
    {
        try
        {
            return JsonSerializer.Deserialize(File.ReadAllText(FilePath), SettingsJson.Default.AppSettings) ?? new AppSettings();
        }
        catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
        {
            return new AppSettings();
        }
    }

    public void Save()
    {
        try
        {
            Directory.CreateDirectory(MainWindow.DataDirectory);
            var tmp = FilePath + ".tmp";
            File.WriteAllText(tmp, JsonSerializer.Serialize(this, SettingsJson.Default.AppSettings));
            File.Move(tmp, FilePath, overwrite: true);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // Not fatal: settings apply for this session.
        }
    }

    // ---- Start with Windows (HKCU Run key; per user, no admin) -----------------------------

    private const string RunKey = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string RunValue = "Annex";
    /// <summary>Passed by the Run key: start quietly in the tray.</summary>
    public const string BackgroundArg = "--background";

    /// <summary>Points "Start with Windows" at this copy of Annex (it may have moved, e.g. after an update).</summary>
    public static void RefreshStartWithWindows()
    {
        using var key = Registry.CurrentUser.OpenSubKey(RunKey, writable: true);
        var expected = $"\"{Environment.ProcessPath}\" {BackgroundArg}";
        if (key?.GetValue(RunValue) is string current && !string.Equals(current, expected, StringComparison.OrdinalIgnoreCase))
            key.SetValue(RunValue, expected);
    }

    public static bool StartWithWindows
    {
        get
        {
            using var key = Registry.CurrentUser.OpenSubKey(RunKey);
            return key?.GetValue(RunValue) is string;
        }
        set
        {
            using var key = Registry.CurrentUser.CreateSubKey(RunKey);
            if (value)
                key.SetValue(RunValue, $"\"{Environment.ProcessPath}\" {BackgroundArg}");
            else
                key.DeleteValue(RunValue, throwOnMissingValue: false);
        }
    }
}

[JsonSerializable(typeof(AppSettings))]
[JsonSourceGenerationOptions(WriteIndented = true)]
internal sealed partial class SettingsJson : JsonSerializerContext;

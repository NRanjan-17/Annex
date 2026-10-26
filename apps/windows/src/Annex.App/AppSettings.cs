using System.Runtime.InteropServices;
using System.Text;
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
    /// <summary>Include pre-releases; off goes back to the latest normal release. Until the user flips
    /// it (<see cref="BetaUpdatesChosen"/>), it follows what's installed: someone who installed a beta
    /// by hand gets betas, not an offer to go back.</summary>
    public bool BetaUpdates { get; set; }
    /// <summary>The user has flipped Beta updates. (Older settings files always hold a BetaUpdates
    /// value, so its presence can't say this.)</summary>
    public bool BetaUpdatesChosen { get; set; }

    public static bool RunningBeta => Annex.Interop.AnnexNative.Version.Contains('-');
    /// <summary>The update the user chose "Not now" for (the banner returns for a newer one).</summary>
    public string? DismissedUpdate { get; set; }
    /// <summary>When Annex last finished checking for updates (shown on the About page).</summary>
    public DateTimeOffset? LastUpdateCheck { get; set; }
    /// <summary>Write annex.log for bug reports. Off by default.</summary>
    public bool SaveLog { get; set; }
    /// <summary>Viewer: bitrate ceiling asked of hosts (Fast suits Wi-Fi).</summary>
    public Annex.Interop.Quality Quality { get; set; } = Annex.Interop.Quality.Best;
    /// <summary>Share and connect over the internet with codes. Off by default: it uses a server.</summary>
    public bool InternetEnabled { get; set; }
    /// <summary>The rendezvous server (wss://…) that swaps the two computers' addresses; empty = Annex's.</summary>
    public string RendezvousServer { get; set; } = "";
    public const string AnnexServer = "wss://connect.getannex.dev";
    /// <summary>The server codes use: the one set here, else Annex's.</summary>
    [JsonIgnore]
    public string Server => RendezvousServer.Trim() is { Length: > 0 } s ? s : AnnexServer;
    /// <summary>The user's own relay (turn:host:port) for networks that block direct connections;
    /// empty = the server's relay, if it offers one. The password is in <see cref="RelayPassword"/>.</summary>
    public string RelayUrl { get; set; } = "";
    public string RelayUsername { get; set; } = "";

    [JsonIgnore]
    public bool InternetReady => InternetEnabled;

    /// <summary>Kept out of settings.json: encrypted for this Windows user (DPAPI) in relay.dat.</summary>
    [JsonIgnore]
    public string RelayPassword
    {
        get => Secret.Read(RelayPasswordPath);
        set => Secret.Write(RelayPasswordPath, value);
    }

    private static string RelayPasswordPath => Path.Combine(MainWindow.DataDirectory, "relay.dat");

    private static string FilePath => Path.Combine(MainWindow.DataDirectory, "settings.json");

    public static AppSettings Load()
    {
        AppSettings settings;
        try
        {
            settings = JsonSerializer.Deserialize(File.ReadAllText(FilePath), SettingsJson.Default.AppSettings) ?? new AppSettings();
        }
        catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
        {
            settings = new AppSettings();
        }
        if (!settings.BetaUpdatesChosen) settings.BetaUpdates = RunningBeta;
        return settings;
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

/// <summary>Small secrets encrypted for the current Windows user (CryptProtectData), so only this
/// user on this PC can read them. The file lives in the data folder the uninstaller removes.</summary>
internal static unsafe partial class Secret
{
    public static string Read(string path)
    {
        try
        {
            var data = File.ReadAllBytes(path);
            fixed (byte* p = data)
            {
                var input = new Blob { Size = (uint)data.Length, Data = p };
                if (!CryptUnprotectData(&input, 0, null, 0, 0, 0, out var output)) return "";
                try { return Encoding.UTF8.GetString(output.Data, (int)output.Size); }
                finally { LocalFree((nint)output.Data); }
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return "";
        }
    }

    public static void Write(string path, string value)
    {
        try
        {
            if (value.Length == 0)
            {
                File.Delete(path);
                return;
            }
            var data = Encoding.UTF8.GetBytes(value);
            fixed (byte* p = data)
            {
                var input = new Blob { Size = (uint)data.Length, Data = p };
                if (!CryptProtectData(&input, null, null, 0, 0, 0, out var output)) return;
                try
                {
                    Directory.CreateDirectory(Path.GetDirectoryName(path)!);
                    File.WriteAllBytes(path, new ReadOnlySpan<byte>(output.Data, (int)output.Size).ToArray());
                }
                finally { LocalFree((nint)output.Data); }
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            // Not fatal: the relay just won't have its password after a restart.
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Blob
    {
        public uint Size;
        public byte* Data;
    }

    [LibraryImport("crypt32.dll", StringMarshalling = StringMarshalling.Utf16)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CryptProtectData(Blob* input, string? description, Blob* entropy, nint reserved, nint prompt,
        uint flags, out Blob output);

    [LibraryImport("crypt32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CryptUnprotectData(Blob* input, nint description, Blob* entropy, nint reserved, nint prompt,
        uint flags, out Blob output);

    [LibraryImport("kernel32.dll")]
    private static partial nint LocalFree(nint mem);
}

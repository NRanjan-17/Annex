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
    /// <summary>HEVC with computers that support it too (sharper at the same bitrate); H.264 otherwise.</summary>
    public bool Hevc { get; set; } = true;
    /// <summary>Graphics card for sharing (adapter LUID); 0 = automatic.</summary>
    public ulong GpuLuid { get; set; }
    public bool StartSharingAtLaunch { get; set; }
    /// <summary>Nobody at this PC's screen: pairing PINs also go to pairing-pin.txt in the data folder, and
    /// paired devices may control it without asking. Over SSH: set "Headless": true in settings.json.</summary>
    public bool Headless { get; set; }
    /// <summary>Viewer: ask hosts to let this PC's mouse and keyboard control them. On by default: the
    /// sharing computer still decides per device (Paired devices), so asking is harmless.</summary>
    public bool UseMouseAndKeyboard { get; set; } = true;
    public bool ExtendByDefault { get; set; }
    /// <summary>Copy on one computer, paste on the other, while one controls the other.</summary>
    public bool ShareClipboard { get; set; } = true;
    /// <summary>Take files the other computer sends, into Downloads.</summary>
    public bool ReceiveFiles { get; set; } = true;
    /// <summary>Viewers of this PC hear what it plays.</summary>
    public bool ShareSound { get; set; } = true;
    /// <summary>Play the shared computer's sound, and a viewer's microphone, here.</summary>
    public bool HearSound { get; set; } = true;
    /// <summary>Send this PC's microphone to the computer it views.</summary>
    public bool SendMicrophone { get; set; }
    /// <summary>A viewer's microphone becomes "Annex Microphone" instead of playing aloud (where supported).</summary>
    public bool VirtualMicrophone { get; set; } = true;
    /// <summary>Drag files to the top of the screen to send them while connected.</summary>
    public bool DropAtTop { get; set; } = true;
    /// <summary>Viewer: move each new session onto this PC's hotspot right after it connects.</summary>
    public bool ConnectOverDirectWifi { get; set; }
    /// <summary>Show identity codes, addresses and connection figures (Settings → Developer).</summary>
    public bool ShowTechnicalDetails { get; set; }
    /// <summary>Settings → Developer: also log each input event and progress step (annex.log grows faster). On by
    /// default, so a first report already says what broke.</summary>
    public bool VerboseLogging { get; set; } = true;
    /// <summary>Verbose logging was switched on once for installs from before it was the default.</summary>
    public bool VerboseLoggingDefaulted { get; set; }
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
    /// <summary>The version whose "What's new" was last shown (or that was installed fresh); null in
    /// settings files from before 0.4.0.</summary>
    public string? LastSeenVersion { get; set; }
    /// <summary>When Annex last finished checking for updates (shown on the About page).</summary>
    public DateTimeOffset? LastUpdateCheck { get; set; }
    /// <summary>Viewer: bitrate ceiling asked of hosts (Fast suits Wi-Fi).</summary>
    public Annex.Interop.Quality Quality { get; set; } = Annex.Interop.Quality.Best;
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

    /// <summary>Kept out of settings.json: encrypted for this Windows user (DPAPI) in relay.dat.</summary>
    [JsonIgnore]
    public string RelayPassword
    {
        get => Secret.Read(RelayPasswordPath);
        set => Secret.Write(RelayPasswordPath, value);
    }

    private static string RelayPasswordPath => Path.Combine(MainWindow.DataDirectory, "relay.dat");

    private static string FilePath => Path.Combine(MainWindow.DataDirectory, "settings.json");

    /// <summary>Why settings.json couldn't be read at launch (logged once the log is open), or null.</summary>
    [JsonIgnore]
    public string? LoadProblem { get; private set; }

    /// <summary>Annex ran on this PC before this launch: settings.json, the identity or the log was
    /// already in the data folder. Tells an upgrade from 0.3.0 (no <see cref="LastSeenVersion"/>) from a first install.</summary>
    [JsonIgnore]
    public bool UsedBefore { get; private set; }

    /// <summary>The settings for the log: nothing secret (the relay's username and password stay out).</summary>
    public string Describe() =>
        $"name {(DeviceName is null ? "(this PC's)" : $"\"{DeviceName}\"")}, hardware encoder {PreferHardwareEncoder}, HEVC {Hevc}, gpu {GpuLuid:x}, "
        + $"share at launch {StartSharingAtLaunch}, no one at screen {Headless}, control {UseMouseAndKeyboard}, extend by default {ExtendByDefault}, "
        + $"clipboard {ShareClipboard}, receive files {ReceiveFiles}, drop at top {DropAtTop}, "
        + $"share sound {ShareSound}, hear sound {HearSound}, send microphone {SendMicrophone}, virtual microphone {VirtualMicrophone}, direct Wi-Fi first {ConnectOverDirectWifi}, "
        + $"technical details {ShowTechnicalDetails}, verbose log {VerboseLogging}, updates {CheckForUpdates} (beta {BetaUpdates}), quality {Quality}, "
        + $"server {(RendezvousServer.Trim().Length == 0 ? "Annex's" : RendezvousServer.Trim())}, own relay {(RelayUrl.Trim().Length == 0 ? "no" : "yes")}";

    public static AppSettings Load()
    {
        AppSettings settings;
        try
        {
            settings = JsonSerializer.Deserialize(File.ReadAllText(FilePath), SettingsJson.Default.AppSettings) ?? new AppSettings();
        }
        catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
        {
            settings = new AppSettings
            {
                LoadProblem = e is FileNotFoundException or DirectoryNotFoundException ? null : $"{e.GetType().Name}: {e.Message}",
            };
        }
        if (!settings.BetaUpdatesChosen) settings.BetaUpdates = RunningBeta;
        if (!settings.VerboseLoggingDefaulted)
        {
            settings.VerboseLogging = true;
            settings.VerboseLoggingDefaulted = true;
        }
        // Checked before the core opens, which creates the identity and the log.
        settings.UsedBefore = File.Exists(FilePath)
            || File.Exists(Path.Combine(MainWindow.DataDirectory, "identity.cert.der"))
            || File.Exists(Path.Combine(MainWindow.DataDirectory, "annex.log"));
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
            Annex.Interop.AnnexLog.Warn($"[settings] couldn't save settings.json: {e.Message}");
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
                if (!CryptUnprotectData(&input, 0, null, 0, 0, 0, out var output))
                {
                    Annex.Interop.AnnexLog.Warn($"[settings] couldn't decrypt {Path.GetFileName(path)} (error {Marshal.GetLastPInvokeError()})");
                    return "";
                }
                try { return Encoding.UTF8.GetString(output.Data, (int)output.Size); }
                finally { LocalFree((nint)output.Data); }
            }
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            if (e is not (FileNotFoundException or DirectoryNotFoundException))
                Annex.Interop.AnnexLog.Warn($"[settings] couldn't read {Path.GetFileName(path)}: {e.Message}");
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
                if (!CryptProtectData(&input, null, null, 0, 0, 0, out var output))
                {
                    Annex.Interop.AnnexLog.Warn($"[settings] couldn't encrypt {Path.GetFileName(path)} (error {Marshal.GetLastPInvokeError()})");
                    return;
                }
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
            Annex.Interop.AnnexLog.Warn($"[settings] couldn't save {Path.GetFileName(path)}: {e.Message}");
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Blob
    {
        public uint Size;
        public byte* Data;
    }

    [LibraryImport("crypt32.dll", StringMarshalling = StringMarshalling.Utf16, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CryptProtectData(Blob* input, string? description, Blob* entropy, nint reserved, nint prompt,
        uint flags, out Blob output);

    [LibraryImport("crypt32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool CryptUnprotectData(Blob* input, nint description, Blob* entropy, nint reserved, nint prompt,
        uint flags, out Blob output);

    [LibraryImport("kernel32.dll")]
    private static partial nint LocalFree(nint mem);
}

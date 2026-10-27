using System.Diagnostics;
using System.Runtime.InteropServices;
using Annex.Interop;
using Microsoft.Win32;

namespace Annex.App;

/// <summary>
/// In-app updates from this project's GitHub releases. The core picks the release and checks the
/// download against the release's signed checksums; nothing is installed unless that passes.
/// Only GitHub is contacted, and nothing about this PC is sent.
/// </summary>
internal sealed class Updater
{
    public enum Stage { Idle, Checking, UpToDate, Available, Downloading, Installing, Failed }

    public Stage State { get; private set; } = Stage.Idle;
    public UpdateInfo? Available { get; private set; }
    public string? Error { get; private set; }

    /// <summary>When a check last finished (up to date or found an update).</summary>
    public DateTimeOffset? LastChecked { get; set; }

    /// <summary>Raised on any change (possibly off the UI thread).</summary>
    public event Action? Changed;

    private static readonly HttpClient s_http = CreateClient();

    private static HttpClient CreateClient()
    {
        var http = new HttpClient { Timeout = TimeSpan.FromMinutes(5) };
        http.DefaultRequestHeaders.UserAgent.ParseAdd($"Annex/{AnnexNative.Version}");
        return http;
    }

    /// <summary>This PC's installer in a release.</summary>
    public static string AssetSuffix => RuntimeInformation.OSArchitecture == Architecture.Arm64 ? "-arm64.exe" : "-x64.exe";

    /// <summary>Inno Setup's uninstall registration (see AppId in installer/windows/Annex.iss).</summary>
    private const string UninstallKey = @"Software\Microsoft\Windows\CurrentVersion\Uninstall\{6F4C2B1E-8A0D-4F57-9B3A-2E7D5C1A9F40}_is1";

    /// <summary>This copy of Annex was installed for all users (wherever it is, e.g. another drive),
    /// so the update must be too. Read from Setup's own registration, not guessed from the folder.</summary>
    public static bool InstalledForAllUsers
    {
        get
        {
            foreach (var view in new[] { RegistryView.Registry64, RegistryView.Registry32 })
            {
                using var hklm = RegistryKey.OpenBaseKey(RegistryHive.LocalMachine, view);
                if (SameFolder(hklm.OpenSubKey(UninstallKey)?.GetValue("Inno Setup: App Path") as string, AppContext.BaseDirectory))
                    return true;
            }
            return false;
        }
    }

    private static bool SameFolder(string? a, string b) =>
        a is not null && string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(a)),
            Path.TrimEndingDirectorySeparator(Path.GetFullPath(b)), StringComparison.OrdinalIgnoreCase);

    public bool Busy => State is Stage.Checking or Stage.Downloading or Stage.Installing;

    /// <summary>Beta updates changed while a check was running: check again with this value after it.</summary>
    private bool? _recheckBeta;

    /// <summary>Beta updates was flipped: the old offer was for the other setting, so it goes now,
    /// then a check runs with the new one.</summary>
    public Task BetaChangedAsync(bool beta)
    {
        if (State == Stage.Available)
        {
            Available = null;
            Set(Stage.Idle);
        }
        return CheckAsync(beta);
    }

    /// <summary><paramref name="automatic"/>: the daily check, which never offers going back to an
    /// older normal release (only turning Beta updates off, or Check now, does).</summary>
    public async Task CheckAsync(bool beta, bool automatic = false)
    {
        if (State == Stage.Checking)
        {
            if (!automatic) _recheckBeta = beta;
            return;
        }
        if (Busy) return;
        Set(Stage.Checking);
        try
        {
            var json = await s_http.GetStringAsync(AnnexNative.UpdateReleasesUrl).ConfigureAwait(false);
            var found = AnnexNative.PickUpdate(json, beta, AssetSuffix);
            Available = automatic && found is { Downgrade: true } ? null : found;
            Error = null;
            LastChecked = DateTimeOffset.Now;
            Set(Available is null ? Stage.UpToDate : Stage.Available);
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or AnnexException)
        {
            Available = null;
            Error = "Couldn't check for updates. Check the internet connection and try again.";
            Set(Stage.Failed);
        }
        // The setting changed mid-check: that answer was for the old one.
        if (_recheckBeta is { } again)
        {
            _recheckBeta = null;
            if (again != beta) await CheckAsync(again).ConfigureAwait(false);
        }
    }

    /// <summary>Downloads and verifies the update, then starts Setup silently. Setup closes Annex and
    /// starts it again when it's done (see /UPDATE in Annex.iss). Returns true once Setup is running.</summary>
    public async Task<bool> InstallAsync(bool beta)
    {
        if (Available is null || Busy) return false;
        Set(Stage.Downloading);
        // The offer can be hours old and releases come quickly: install the newest release now,
        // not the next one after this version. If GitHub can't be reached, the earlier offer stands.
        try
        {
            var json = await s_http.GetStringAsync(AnnexNative.UpdateReleasesUrl).ConfigureAwait(false);
            if (AnnexNative.PickUpdate(json, beta, AssetSuffix) is { } newest) Available = newest;
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or AnnexException)
        {
        }
        var u = Available!;
        Set(Stage.Downloading); // shows the version actually being installed
        var dir = Path.Combine(Path.GetTempPath(), "Annex-Update-" + Guid.NewGuid().ToString("N"));
        try
        {
            // The name comes from GitHub; it must be a plain file name (and it's checked against the
            // signed checksums below).
            var name = Path.GetFileName(u.AssetName);
            if (name != u.AssetName || name.Length == 0) throw new AnnexException("The update's file name isn't valid.");
            Directory.CreateDirectory(dir);
            var file = Path.Combine(dir, name);
            var sums = Path.Combine(dir, "SHA256SUMS.txt");
            var sig = Path.Combine(dir, "SHA256SUMS.txt.sig");
            await Task.WhenAll(Download(u.AssetUrl, file), Download(u.SumsUrl, sums), Download(u.SigUrl, sig)).ConfigureAwait(false);
            AnnexNative.VerifyUpdate(sums, sig, file, name);

            Set(Stage.Installing);
            // Same install mode and folder as this copy, so Setup updates it in place.
            var folder = Path.TrimEndingDirectorySeparator(AppContext.BaseDirectory);
            var args = "/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /UPDATE=1 "
                + (InstalledForAllUsers ? "/ALLUSERS" : "/CURRENTUSER") + $" /DIR=\"{folder}\"";
            Process.Start(new ProcessStartInfo(file, args) { UseShellExecute = true });
            return true;
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or IOException or AnnexException
                                      or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            Error = e is AnnexException ? e.Message : $"The update couldn't be installed: {e.Message}";
            Set(Stage.Failed);
            try { Directory.Delete(dir, recursive: true); } catch (IOException) { } catch (UnauthorizedAccessException) { }
            return false;
        }
    }

    private static async Task Download(string url, string path)
    {
        using var response = await s_http.GetAsync(url, HttpCompletionOption.ResponseHeadersRead).ConfigureAwait(false);
        response.EnsureSuccessStatusCode();
        await using var source = await response.Content.ReadAsStreamAsync().ConfigureAwait(false);
        await using var target = File.Create(path);
        await source.CopyToAsync(target).ConfigureAwait(false);
    }

    private void Set(Stage stage)
    {
        State = stage;
        Changed?.Invoke();
    }
}

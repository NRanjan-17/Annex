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
    /// <summary>Ready: No one at this screen, the update is downloaded and verified and installs once
    /// no one is connected.</summary>
    public enum Stage { Idle, Checking, UpToDate, Available, Downloading, Installing, Failed, Ready }

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

    /// <summary>No one at this screen is on: updates install without asking (set by MainWindow).</summary>
    public Func<bool> Unattended { get; set; } = () => false;
    /// <summary>Someone views this PC, or it views another (set by MainWindow).</summary>
    public Func<bool> InSession { get; set; } = () => true;
    /// <summary>Just before an unattended update closes Annex (MainWindow remembers to share again).</summary>
    public Action WillQuitToUpdate { get; set; } = () => { };

    /// <summary>No one connected this long before an unattended update installs, so a viewer
    /// switching from mirror to extend isn't dropped in between.</summary>
    private static readonly TimeSpan IdleBeforeInstall = TimeSpan.FromSeconds(60);
    /// <summary>The new version must be running (and sharing, if it was) this soon, or the old one
    /// comes back.</summary>
    private const int ConfirmWithinSeconds = 90;
    private (UpdateInfo Info, string File, string Dir)? _staged;
    private DateTimeOffset? _idleSince;

    /// <summary>Files in the data folder between this copy, the watchdog and the next launch.</summary>
    private static string Marker(string name) => Path.Combine(MainWindow.DataDirectory, name);
    /// <summary>The version an unattended update installed, until it says it's running.</summary>
    private static string PendingFile => Marker("update-pending");
    /// <summary>What the watchdog waits for: the new version writes its version here.</summary>
    private static string OkFile => Marker("update-ok");
    /// <summary>The watchdog put the old version back: this version never installs unattended again.</summary>
    private static string FailedFile => Marker("update-failed");
    /// <summary>Sharing when the update closed Annex: share again at the next launch.</summary>
    public static string ShareAfterUpdateFile => Marker("share-after-update");

    private static string? ReadMarker(string path)
    {
        try { return File.Exists(path) ? File.ReadAllText(path).Trim() : null; }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException) { return null; }
    }

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
            AnnexLog.Debug($"[update] check asked while one runs (beta {beta}){(automatic ? ", skipped" : ", runs after it")}");
            return;
        }
        if (Busy)
        {
            AnnexLog.Debug($"[update] check skipped: {State}");
            return;
        }
        AnnexLog.Info($"[update] checking (beta {beta}, {(automatic ? "automatic" : "asked")}, {AssetSuffix})");
        Set(Stage.Checking);
        try
        {
            var json = await s_http.GetStringAsync(AnnexNative.UpdateReleasesUrl).ConfigureAwait(false);
            var found = AnnexNative.PickUpdate(json, beta, AssetSuffix);
            Available = automatic && found is { Downgrade: true } ? null : found;
            Error = null;
            LastChecked = DateTimeOffset.Now;
            if (_staged is { } waiting && Available?.Version == waiting.Info.Version)
            {
                Available = waiting.Info;
                Set(Stage.Ready);
                return;
            }
            Discard();
            Set(Available is null ? Stage.UpToDate : Stage.Available);
            AnnexLog.Info(found is null ? $"[update] up to date ({AnnexNative.Version})"
                : $"[update] {found.Version} available{(found.Prerelease ? " (pre-release)" : "")}{(found.Downgrade ? " (back to normal release)" : "")}"
                  + (Available is null ? ", not offered by the daily check" : ""));
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or AnnexException)
        {
            AnnexLog.Warn($"[update] check failed: {e.GetType().Name}: {e.Message}");
            if (_staged is { } waiting)
            {
                Available = waiting.Info;
                Set(Stage.Ready);
                return;
            }
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
        if (Available is null || Busy)
        {
            AnnexLog.Warn($"[update] install skipped: {(Available is null ? "nothing to install" : State.ToString())}");
            return false;
        }
        Discard();
        AnnexLog.Info($"[update] installing {Available.Version}: fetching the newest release first");
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
            AnnexLog.Warn($"[update] couldn't refresh the release list, installing the earlier offer: {e.Message}");
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
            AnnexLog.Info($"[update] downloading {name} ({u.Version}) to {dir}");
            await Task.WhenAll(Download(u.AssetUrl, file), Download(u.SumsUrl, sums), Download(u.SigUrl, sig)).ConfigureAwait(false);
            AnnexLog.Info($"[update] downloaded {new FileInfo(file).Length} bytes, checking the signature");
            AnnexNative.VerifyUpdate(sums, sig, file, name);
            AnnexLog.Info("[update] signature good");

            Set(Stage.Installing);
            // Same install mode and folder as this copy, so Setup updates it in place.
            var folder = Path.TrimEndingDirectorySeparator(AppContext.BaseDirectory);
            var args = "/SILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /UPDATE=1 "
                + (InstalledForAllUsers ? "/ALLUSERS" : "/CURRENTUSER") + $" /DIR=\"{folder}\"";
            AnnexLog.Info($"[update] starting Setup: {args}");
            Process.Start(new ProcessStartInfo(file, args) { UseShellExecute = true });
            return true;
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or IOException or AnnexException
                                      or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            AnnexLog.Error($"[update] install failed: {e.GetType().Name}: {e.Message}");
            Error = e is AnnexException ? e.Message : $"The update couldn't be installed: {e.Message}";
            Set(Stage.Failed);
            try { Directory.Delete(dir, recursive: true); }
            catch (Exception d) when (d is IOException or UnauthorizedAccessException)
            {
                AnnexLog.Warn($"[update] couldn't remove {dir}: {d.Message}");
            }
            return false;
        }
    }

    // ---- No one at this screen ------------------------------------------------------------

    /// <summary>After the daily check: with No one at this screen on, downloads and verifies the
    /// update now; <see cref="InstallIfIdle"/> installs it once no one is connected.</summary>
    public async Task PrepareUnattendedAsync()
    {
        if (State != Stage.Available || Available is not { Downgrade: false } u || !Unattended()) return;
        if (ReadMarker(FailedFile) == u.Version)
        {
            AnnexLog.Warn($"[update] {u.Version} didn't start here before: not installing it unattended");
            return;
        }
        if (InstalledForAllUsers)
        {
            AnnexLog.Warn($"[update] {u.Version} can't install unattended: Annex is installed for all users, and Setup would ask an administrator");
            return;
        }
        AnnexLog.Info($"[update] No one at this screen: downloading {u.Version} to install once no one is connected");
        Set(Stage.Downloading);
        var dir = Path.Combine(Path.GetTempPath(), "Annex-Update-" + Guid.NewGuid().ToString("N"));
        try
        {
            var file = await FetchAsync(u, dir).ConfigureAwait(false);
            _staged = (u, file, dir);
            _idleSince = null;
            Set(Stage.Ready);
        }
        catch (Exception e) when (e is HttpRequestException or TaskCanceledException or IOException or AnnexException
                                      or UnauthorizedAccessException)
        {
            AnnexLog.Warn($"[update] couldn't prepare {u.Version}: {e.GetType().Name}: {e.Message}");
            TryDelete(dir);
            Set(Stage.Available);
        }
    }

    /// <summary>Called every few seconds: installs a waiting update once no one has been connected
    /// for <see cref="IdleBeforeInstall"/>. Returns true when Setup is starting (Annex will close).</summary>
    public bool InstallIfIdle()
    {
        if (State != Stage.Ready || _staged is not { } waiting || !Unattended()) return false;
        if (InSession())
        {
            _idleSince = null;
            return false;
        }
        _idleSince ??= DateTimeOffset.Now;
        if (DateTimeOffset.Now - _idleSince < IdleBeforeInstall) return false;
        AnnexLog.Info($"[update] no one connected for {IdleBeforeInstall.TotalSeconds:0} s: installing {waiting.Info.Version} unattended");
        Set(Stage.Installing);
        try
        {
            WillQuitToUpdate();
            StartWatchdog(waiting.Info.Version, waiting.File, waiting.Dir);
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or System.ComponentModel.Win32Exception)
        {
            AnnexLog.Error($"[update] couldn't start the unattended install: {e.GetType().Name}: {e.Message}");
            Error = $"The update couldn't be installed: {e.Message}";
            Discard();
            Set(Stage.Failed);
            return false;
        }
    }

    /// <summary>Copies this version aside, then starts a hidden PowerShell script that runs Setup,
    /// waits up to <see cref="ConfirmWithinSeconds"/> for the new version to write
    /// <see cref="OkFile"/>, and otherwise closes it, puts the copy back and starts it again.</summary>
    private static void StartWatchdog(string version, string setup, string dir)
    {
        var folder = Path.TrimEndingDirectorySeparator(AppContext.BaseDirectory);
        var previous = Path.Combine(dir, "previous");
        CopyFolder(folder, previous);
        File.Delete(OkFile);
        File.WriteAllText(PendingFile, version);
        var args = $"/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /CLOSEAPPLICATIONS /UPDATE=1 /CURRENTUSER /DIR=\"{folder}\"";
        var exe = Environment.ProcessPath ?? Path.Combine(folder, "Annex.exe");
        // Every value goes in as a single-quoted PowerShell string, so no argument parsing is involved.
        static string Q(string v) => "'" + v.Replace("'", "''", StringComparison.Ordinal) + "'";
        var script = Path.Combine(dir, "watchdog.ps1");
        File.WriteAllText(script, $$"""
            $Setup = {{Q(setup)}}; $SetupArgs = {{Q(args)}}; $Folder = {{Q(folder)}}; $Previous = {{Q(previous)}}
            $Version = {{Q(version)}}; $Ok = {{Q(OkFile)}}; $Failed = {{Q(FailedFile)}}; $Share = {{Q(ShareAfterUpdateFile)}}; $Exe = {{Q(exe)}}
            Start-Process -FilePath $Setup -ArgumentList $SetupArgs -Wait
            for ($i = 0; $i -lt {{ConfirmWithinSeconds}}; $i++) {
              if ((Test-Path $Ok) -and ((Get-Content $Ok -Raw).Trim() -eq $Version)) { Remove-Item $Ok; exit 0 }
              Start-Sleep -Seconds 1
            }
            # The new version isn't running: put the old one back.
            Get-Process | Where-Object { $_.Path -and $_.Path.StartsWith($Folder + '\', 'OrdinalIgnoreCase') } | Stop-Process -Force
            Start-Sleep -Seconds 2
            robocopy $Previous $Folder /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
            Set-Content -Path $Failed -Value $Version
            Set-Content -Path $Share -Value 1
            Start-Process -FilePath $Exe
            """);
        var psi = new ProcessStartInfo("powershell.exe") { UseShellExecute = false, CreateNoWindow = true };
        foreach (var a in new[] { "-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File", script })
            psi.ArgumentList.Add(a);
        AnnexLog.Info($"[update] starting Setup unattended, the previous version is kept in {previous} until this one confirms");
        Process.Start(psi);
    }

    private static void CopyFolder(string from, string to)
    {
        foreach (var d in Directory.GetDirectories(from, "*", SearchOption.AllDirectories))
            Directory.CreateDirectory(Path.Combine(to, Path.GetRelativePath(from, d)));
        Directory.CreateDirectory(to);
        foreach (var f in Directory.GetFiles(from, "*", SearchOption.AllDirectories))
            File.Copy(f, Path.Combine(to, Path.GetRelativePath(from, f)), overwrite: true);
    }

    /// <summary>At launch, once sharing is back (if it was on): tells a waiting watchdog this version
    /// runs. After a put-back, logs which version failed.</summary>
    public static void ConfirmUpdate()
    {
        var pending = ReadMarker(PendingFile);
        if (pending is null) return;
        TryDeleteFile(PendingFile);
        if (pending != AnnexNative.Version)
        {
            AnnexLog.Warn($"[update] {pending} didn't start in {ConfirmWithinSeconds} s: back on {AnnexNative.Version}");
            return;
        }
        try
        {
            File.WriteAllText(OkFile, pending);
            AnnexLog.Info($"[update] updated to {pending} unattended, and it runs");
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AnnexLog.Warn($"[update] couldn't confirm the update: {e.Message}");
        }
    }

    private async Task<string> FetchAsync(UpdateInfo u, string dir)
    {
        // The name comes from GitHub; it must be a plain file name (and it's checked against the
        // signed checksums below).
        var name = Path.GetFileName(u.AssetName);
        if (name != u.AssetName || name.Length == 0) throw new AnnexException("The update's file name isn't valid.");
        Directory.CreateDirectory(dir);
        var file = Path.Combine(dir, name);
        var sums = Path.Combine(dir, "SHA256SUMS.txt");
        var sig = Path.Combine(dir, "SHA256SUMS.txt.sig");
        AnnexLog.Info($"[update] downloading {name} ({u.Version}) to {dir}");
        await Task.WhenAll(Download(u.AssetUrl, file), Download(u.SumsUrl, sums), Download(u.SigUrl, sig)).ConfigureAwait(false);
        AnnexLog.Info($"[update] downloaded {new FileInfo(file).Length} bytes, checking the signature");
        AnnexNative.VerifyUpdate(sums, sig, file, name);
        AnnexLog.Info("[update] signature good");
        return file;
    }

    private void Discard()
    {
        if (_staged is { } s) TryDelete(s.Dir);
        _staged = null;
        _idleSince = null;
    }

    private static void TryDelete(string dir)
    {
        try { if (Directory.Exists(dir)) Directory.Delete(dir, recursive: true); }
        catch (Exception d) when (d is IOException or UnauthorizedAccessException)
        {
            AnnexLog.Warn($"[update] couldn't remove {dir}: {d.Message}");
        }
    }

    private static void TryDeleteFile(string file)
    {
        try { File.Delete(file); }
        catch (Exception d) when (d is IOException or UnauthorizedAccessException)
        {
            AnnexLog.Warn($"[update] couldn't remove {file}: {d.Message}");
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

using System.Net.NetworkInformation;
using System.Net.Sockets;
using Annex.Interop;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics;

namespace Annex.App;

/// <summary>
/// The Windows app: a Settings-style window (Mica, sidebar, setting cards) to share this PC or
/// connect to another computer. Streams are decoded and rendered by the core in its own window
/// (annex_viewer_connect_windowed). Annex lives in the system tray: closing the window hides
/// it; "Quit" in the tray menu exits.
/// </summary>
public sealed partial class MainWindow : Window
{
    private const ushort DefaultPort = 47800;

    private readonly DispatcherQueue _ui;
    private readonly AppSettings _settings = AppSettings.Load();
    private readonly List<PeerInfo> _hosts = [];
    private List<TrustedPeer> _trusted = [];
    private CoreClient? _core;
    private DiscoveryHandle? _discovery;
    private ViewerHandle? _viewer;
    private HostHandle? _host;
    private readonly ClipboardSync _clipboard = new();
    /// <summary>Viewer: something was copied while the viewer window wasn't in front.</summary>
    private bool _copiedWhileAway;
    private DispatcherQueueTimer? _clipboardTimer;
    private DropTray? _drop;
    private IReadOnlyList<MonitorInfo> _monitors = [];
    private int _monitorIndex;
    private ContentDialog? _pinDialog;
    private ulong _pinRequest;
    private TrayIcon? _tray;
    private bool _quitting;
    private string? _startupError;

    // Host state.
    private readonly Dictionary<ulong, (string Name, string Mode, bool Input, bool Relayed)> _sessions = [];
    private StreamConfig? _hostStream;
    private string? _hostError;
    private ExtendStatus? _extendStatus;
    /// <summary>null = not checked yet.</summary>
    private bool? _firewallAllowed;

    // Viewer state.
    private bool _extend;
    /// <summary>Hosts and viewers being freed, one after another (see FreeLater).</summary>
    private Task _teardown = Task.CompletedTask;
    /// <summary>Settings → Advanced is open. Closed each time Annex starts.</summary>
    private bool _showAdvanced;
    private string _viewingName = "";
    private string? _viewerStatus;
    private LatencyStats? _lat;
    private bool _controlling;
    private string? _viewerFingerprint;
    private bool _viewerExtend;
    /// <summary>The viewer's session started (its mode and the other name are known).</summary>
    private bool _viewerLive;
    /// <summary>Connected over this PC's hotspot right now.</summary>
    private bool _onDirectWifi;

    // Direct Wi-Fi link: this PC's hotspot, and a reconnect we're waiting for after switching.
    private readonly Hotspot _hotspot = new();
    private (string Fingerprint, string Name, bool Extend, DateTimeOffset Until)? _directPending;
    /// <summary>SwitchToDirectWifiAsync is waiting for the hotspot (before _directPending is set).</summary>
    private bool _directStarting;
    private string? _directStatus;
    /// <summary>Identifies the current viewer, so events still queued from a replaced one are ignored.</summary>
    private object? _viewerToken;
    /// <summary>Viewing through a code (failures then mean the internet, not the local network).</summary>
    private bool _viewingByCode;

    /// <summary>Where sharing with a code is: null until asked for.</summary>
    private InternetShare? _internet;
    /// <summary>The code's "works for 9:46" line, ticked every second without rebuilding the page.</summary>
    private TextBlock? _codeCountdown;
    private DispatcherQueueTimer? _codeTimer;

    // Updates.
    private readonly Updater _updater = new();

    // UI.
    private readonly StackPanel _sidebarItems = new() { Spacing = 2 };
    private readonly StackPanel _sidebarFooter = new() { Spacing = 2 };
    private readonly ContentControl _page = new()
    {
        HorizontalContentAlignment = HorizontalAlignment.Stretch,
        VerticalContentAlignment = VerticalAlignment.Stretch,
    };
    private string _pane = "home";
    /// <summary>Nearby: the computer whose card is open (its instance).</summary>
    private string? _selected;
    private bool _showNotSharing;
    // Text boxes survive page rebuilds so typing isn't interrupted by status updates.
    private readonly TextBox _addressBox = new() { PlaceholderText = "192.168.1.20 or host:port" };
    private readonly TextBox _codeBox = new() { PlaceholderText = "K7P-4QX", CharacterCasing = CharacterCasing.Upper, MaxLength = 7 };
    private readonly TextBox _serverBox = new() { PlaceholderText = AppSettings.AnnexServer, Width = 240 };
    private readonly TextBox _relayUrlBox = new() { PlaceholderText = "turn:host:3478", Width = 240 };
    private readonly TextBox _relayUserBox = new() { Width = 240 };
    private readonly PasswordBox _relayPasswordBox = new() { Width = 240 };
    private readonly TextBox _nameBox = new() { Width = 240 };

    public static string DataDirectory =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Annex");

    public MainWindow()
    {
        InitializeComponent();
        _ui = DispatcherQueue.GetForCurrentThread();
        _extend = _settings.ExtendByDefault;
        SystemBackdrop = new DesktopAcrylicBackdrop();
        ExtendsContentIntoTitleBar = true;
        StyleCaptionButtons();
        SizeWindow();
        AppWindow.SetIcon(Ui.AssetPath("Annex.ico"));
        Closed += (_, _) => Shutdown();
        // Closing the window keeps Annex running in the tray.
        AppWindow.Closing += (_, args) =>
        {
            if (_quitting) return;
            args.Cancel = true;
            AppWindow.Hide();
            AnnexLog.Info("[app] window closed: Annex stays in the tray");
        };
        Activated += (_, e) => AnnexLog.Debug($"[app] window {(e.WindowActivationState == WindowActivationState.Deactivated ? "lost focus" : "focused")}");
        _tray = new TrayIcon("Annex", Ui.AssetPath("TrayOff.ico"));
        _tray.LeftClick += ShowFromTray;
        _tray.RightClick += ShowTrayMenu;

        _addressBox.KeyDown += (_, e) =>
        {
            if (e.Key == Windows.System.VirtualKey.Enter) ConnectManual();
        };
        _nameBox.KeyDown += (_, e) =>
        {
            if (e.Key == Windows.System.VirtualKey.Enter) Rename(_nameBox.Text);
        };
        _nameBox.LostFocus += (_, _) => Rename(_nameBox.Text);
        _codeBox.KeyDown += (_, e) =>
        {
            if (e.Key == Windows.System.VirtualKey.Enter) { _extend = false; ConnectByCode(); }
        };
        _serverBox.Text = _settings.RendezvousServer;
        _serverBox.LostFocus += (_, _) => SaveInternet();
        _relayUrlBox.Text = _settings.RelayUrl;
        _relayUrlBox.LostFocus += (_, _) => SaveInternet();
        _relayUserBox.Text = _settings.RelayUsername;
        _relayUserBox.LostFocus += (_, _) => SaveInternet();
        _relayPasswordBox.Password = _settings.RelayPassword;
        _relayPasswordBox.LostFocus += (_, _) =>
        {
            if (_relayPasswordBox.Password == _settings.RelayPassword) return;
            _settings.RelayPassword = _relayPasswordBox.Password;
            Setting("relay password", _relayPasswordBox.Password.Length == 0 ? "cleared" : "changed");
        };

        try
        {
            // Before the core opens, so its "logging started" line says so.
            if (_settings.VerboseLogging) AnnexLog.SetVerbose(true);
            _core = CoreClient.Open(DataDirectory, _settings.DeviceName ?? Environment.MachineName);
            _discovery = new DiscoveryHandle(_core, e => _ui.TryEnqueue(() => OnDiscovery(e)));
            _trusted = [.. _core.TrustedPeers];
            RefreshMonitors();
            _extendStatus = TryExtendStatus();
            LogBoot();
            // Show up under Nearby on other computers as soon as Annex is open.
            _core.SetPresence(true);
            _core.SetHeadless(_settings.Headless);
            _core.SetDownloads(_settings.ReceiveFiles ? DownloadsFolder : null);
            ApplyAudio();
            _clipboard.Copied += OnCopied;
            _drop = new DropTray(_ui);
            _drop.Dropped += SendFiles;
            _drop.PauseRequested += PauseTransfer;
            _drop.CancelRequested += CancelTransfer;
            if (_settings.ShareClipboard) _clipboard.Start();
            _ = CheckFirewallAsync();
            StartUpdateChecks();
            AppSettings.RefreshStartWithWindows();
        }
        catch (Exception ex)
        {
            _startupError = ex.Message;
            AnnexLog.Error($"[app] couldn't start: {ex}");
        }
        _nameBox.Text = _settings.DeviceName ?? "";
        _nameBox.PlaceholderText = Environment.MachineName;
        BuildShell();
        ScheduleWhatsNew();
        if (_settings.StartSharingAtLaunch)
        {
            AnnexLog.Info("[host] sharing at launch (setting)");
            SetHosting(true);
        }
        if (Environment.GetEnvironmentVariable("ANNEX_SELFTEST") == "1") _ui.TryEnqueue(SelfTest);
    }

    /// <summary>CI smoke test: opens every page twice (rebuilds reuse long-lived controls), saves
    /// pictures of the main screens to shots\, then writes selftest-ok to the data folder. Any
    /// exception lands in crash.log instead.</summary>
    private async void SelfTest()
    {
        foreach (var pane in new[] { "home", "share", "settings", "devices", "about", "home", "settings", "about", "share" })
            Go(pane);
        // Settings again with technical details flipped, then back as it was.
        var tech = _settings.ShowTechnicalDetails;
        _settings.ShowTechnicalDetails = !tech;
        Go("settings");
        _settings.ShowTechnicalDetails = tech;
        Go("home");
        ShowTrayMenu();
        _trayMenu?.Hide();
        await SaveScreens();
        var x = TryExtendStatus();
        File.WriteAllText(Path.Combine(DataDirectory, "selftest-extend.json"),
            x is null ? "null" : $"{{\"state\":\"{x.State}\",\"problem\":{x.Problem},\"test_signing\":{(x.TestSigning ? "true" : "false")}}}");
        // The relay password's encryption (DPAPI) round-trips.
        var secret = Path.Combine(DataDirectory, "selftest-secret.dat");
        Secret.Write(secret, "relay-password");
        File.WriteAllText(Path.Combine(DataDirectory, "selftest-secret"), Secret.Read(secret) == "relay-password" ? "ok" : "fail");
        Secret.Write(secret, "");
        File.WriteAllText(Path.Combine(DataDirectory, "selftest-ok"), DateTimeOffset.Now.ToString("O"));
    }

    /// <summary>Pictures of This PC with a code, Settings → Advanced and Nearby while sharing, for
    /// reviewing the design from CI. Settings are changed in memory only and put back.</summary>
    private async Task SaveScreens()
    {
        var dir = Path.Combine(DataDirectory, "shots");
        Directory.CreateDirectory(dir);
        var server = _settings.RendezvousServer;
        _settings.RendezvousServer = "wss://rendezvous.example";
        _serverBox.Text = _settings.RendezvousServer;
        try
        {
            await Shot(dir, "nearby-idle");
            SetHosting(true);
            _internet = new InternetShare.Code("K7P-4QX", DateTimeOffset.UtcNow.AddSeconds(586));
            Go("share");
            await Shot(dir, "this-pc-code");
            Go("settings");
            await Shot(dir, "settings");
            _showAdvanced = true;
            Go("settings");
            await Shot(dir, "settings-advanced");
            Go("home");
            await Shot(dir, "nearby-sharing");
        }
        catch (Exception e)
        {
            File.WriteAllText(Path.Combine(dir, "error.txt"), e.ToString());
        }
        finally
        {
            _internet = null;
            SetHosting(false);
            _showAdvanced = false;
            _settings.RendezvousServer = server;
            _serverBox.Text = server;
            Go("home");
        }
    }

    private async Task Shot(string dir, string name)
    {
        await Task.Delay(600); // layout, then a frame
        var bitmap = new Microsoft.UI.Xaml.Media.Imaging.RenderTargetBitmap();
        await bitmap.RenderAsync(RootGrid);
        var pixels = System.Runtime.InteropServices.WindowsRuntime.WindowsRuntimeBufferExtensions.ToArray(await bitmap.GetPixelsAsync());
        // The acrylic backdrop isn't captured; show the see-through parts over black, as on screen.
        for (var i = 3; i < pixels.Length; i += 4) pixels[i] = 255;
        using var file = File.Create(Path.Combine(dir, name + ".png"));
        var encoder = await Windows.Graphics.Imaging.BitmapEncoder.CreateAsync(Windows.Graphics.Imaging.BitmapEncoder.PngEncoderId,
            file.AsRandomAccessStream());
        encoder.SetPixelData(Windows.Graphics.Imaging.BitmapPixelFormat.Bgra8, Windows.Graphics.Imaging.BitmapAlphaMode.Premultiplied,
            (uint)bitmap.PixelWidth, (uint)bitmap.PixelHeight, 96, 96, pixels);
        await encoder.FlushAsync();
    }

    /// <summary>The first lines of each run: what this PC is and how Annex is set up, so a log explains
    /// itself. Nothing secret (no relay credentials).</summary>
    private void LogBoot()
    {
        var app = typeof(MainWindow).Assembly.GetName().Version;
        var background = Environment.GetCommandLineArgs().Contains(AppSettings.BackgroundArg);
        AnnexLog.Info($"[app] Annex {AnnexNative.Version} (app {app}) starting{(background ? " in the tray (Start with Windows)" : "")}");
        AnnexLog.Info($"[app] {System.Runtime.InteropServices.RuntimeInformation.OSDescription} ({Environment.OSVersion.Version}), "
            + $"{System.Runtime.InteropServices.RuntimeInformation.OSArchitecture}, process {System.Runtime.InteropServices.RuntimeInformation.ProcessArchitecture}, "
            + $".NET {Environment.Version}, WinUI {typeof(Window).Assembly.GetName().Version}");
        AnnexLog.Info($"[app] session {System.Diagnostics.Process.GetCurrentProcess().SessionId}, "
            + $"{(GetSystemMetrics(SmRemoteSession) != 0 ? "Remote Desktop" : "console")}, admin {Environment.IsPrivilegedProcess}, "
            + $"user {Environment.UserName}, machine {Environment.MachineName}");
        AnnexLog.Info($"[display] {_monitors.Count} screen(s): "
            + string.Join("; ", _monitors.Select(m => $"{m.Name.TrimStart('\\', '.')} {m.Width}x{m.Height} {m.RefreshHz} Hz{(m.Primary ? " main" : "")}"))
            + $"; window scale {GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this)) / 96.0:0.##}");
        var gpus = TryGpus();
        AnnexLog.Info($"[display] graphics: {(gpus.Count == 0 ? "none listed" : string.Join("; ", gpus.Select(g => $"{g.Name} ({g.Luid:x})")))}");
        AnnexLog.Info(_extendStatus is { } x
            ? $"[display] extend driver: {x.State}, problem {x.Problem}, test signing {x.TestSigning}"
            : "[display] extend driver: unknown");
        AnnexLog.Info($"[app] data folder {DataDirectory}, device \"{_core?.DeviceName}\" {Fingerprint}, {_trusted.Count} paired device(s)");
        AnnexLog.Info($"[settings] {_settings.Describe()}, start with Windows {AppSettings.StartWithWindows}");
        if (_settings.LoadProblem is { } problem) AnnexLog.Warn($"[settings] couldn't read settings.json, using defaults: {problem}");
    }

    private const int SmRemoteSession = 0x1000;

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial int GetSystemMetrics(int index);

    /// <summary>A setting the user changed, for the log.</summary>
    private static void Setting(string name, object value) => AnnexLog.Info($"[settings] {name}: {value}");

    /// <summary>Opens at a comfortable size in effective pixels (so display scaling doesn't shrink it),
    /// centred, never larger than the screen, with a minimum size.</summary>
    private void SizeWindow()
    {
        var scale = GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(this)) / 96.0;
        var work = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(AppWindow.Id, Microsoft.UI.Windowing.DisplayAreaFallback.Primary).WorkArea;
        var w = Math.Min((int)(1240 * scale), work.Width - 40);
        var h = Math.Min((int)(840 * scale), work.Height - 40);
        AppWindow.MoveAndResize(new Windows.Graphics.RectInt32(work.X + (work.Width - w) / 2, work.Y + (work.Height - h) / 2, w, h));
        if (AppWindow.Presenter is Microsoft.UI.Windowing.OverlappedPresenter p)
        {
            p.PreferredMinimumWidth = (int)(960 * scale);
            p.PreferredMinimumHeight = (int)(620 * scale);
        }
    }

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial uint GetDpiForWindow(nint hwnd);

    [System.Runtime.InteropServices.LibraryImport("kernel32.dll")]
    private static partial uint SetThreadExecutionState(uint flags);

    /// <summary>The tray icon, one look per state, so a glance says whether anyone can see this screen:
    /// faded when not sharing, normal while sharing, with a green dot while another computer shows this
    /// one, and a green ▶ while this PC shows another computer.</summary>
    private void UpdateTrayIcon()
    {
        _tray?.SetIcon(Ui.AssetPath(_sessions.Count > 0 ? "TrayLive.ico" : _viewer is not null ? "TrayView.ico"
            : _host is not null ? "Annex.ico" : "TrayOff.ico"));
    }

    /// <summary>Another computer's name as Annex shows it. Two PCs of one owner can have the same name;
    /// then "(other PC)" says which one is meant.</summary>
    private string Label(string name) => HasThisName(name) ? $"{name} (other PC)" : name;

    private bool HasThisName(string name)
    {
        static string Key(string s) => s.Replace('’', '\'').Trim().ToLowerInvariant();
        var mine = _core?.DeviceName;
        return !string.IsNullOrEmpty(mine) && Key(name) == Key(mine);
    }

    /// <summary>This PC's part in the live session, said from this PC's side: "This PC is sharing ·
    /// Extend", and which way the picture goes.</summary>
    private (string Headline, string From, string To, bool ThisIsFrom)? Role()
    {
        if (_viewer is not null && _viewerLive)
            return ($"This PC is viewing · {(_viewerExtend ? "Extend" : "Mirror")}", Label(_viewingName), "This PC", false);
        if (_sessions.Count == 0) return null;
        var s = _sessions.Values.OrderByDescending(x => x.Mode == "extend").First();
        return ($"This PC is sharing · {(s.Mode == "extend" ? "Extend" : "Mirror")}", "This PC", Label(s.Name), true);
    }

    /// <summary>A computer nearby or in the session with this PC's name, for the notice that asks to rename one.</summary>
    private string? SameNamePeer() =>
        (_viewerLive ? [_viewingName] : Array.Empty<string>())
            .Concat(_sessions.Values.Select(s => s.Name)).Concat(_hosts.Select(h => h.Name))
            .FirstOrDefault(HasThisName);

    /// <summary>Both computers have this PC's name, so nothing on screen can say which is which.</summary>
    private void SameNameNotice(Panel p)
    {
        if (SameNamePeer() is not { } name) return;
        p.Children.Add(Ui.Card("\uE7BA", $"Both computers are called “{name}”.", "Give this PC its own name.",
            Ui.Button("Rename…", () => Go("settings")), glyphBrush: new SolidColorBrush(Microsoft.UI.Colors.Orange)));
    }

    private const uint EsContinuous = 0x80000000, EsSystemRequired = 0x1, EsDisplayRequired = 0x2;
    private bool _keepingAwake;

    /// While a session runs, neither screen sleeps: the viewer gets no local input, and the host's
    /// screen (or its Annex display) is what the viewer is watching. Always called on the UI
    /// thread, which lives as long as the app, so the continuous state holds.
    private void UpdateKeepAwake()
    {
        UpdateTrayIcon();
        bool active = _viewer is not null || _sessions.Count > 0;
        if (active == _keepingAwake) return;
        _keepingAwake = active;
        SetThreadExecutionState(active ? EsContinuous | EsSystemRequired | EsDisplayRequired : EsContinuous);
    }

    // ---- Shell: title bar + sidebar ---------------------------------------------------

    private void BuildShell()
    {
        // Black and glass, like the Mac app: the window is desktop acrylic under a dark layer. The
        // sidebar lets a little of it through; the page sits on a near-solid layer with a rounded
        // corner (Windows Settings' two-tone layout, in black).
        RootGrid.RequestedTheme = ElementTheme.Dark;
        RootGrid.Background = Ui.Black(0.6);
        Ui.PointingHand(RootGrid);
        RootGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(236) });
        RootGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        RootGrid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(48) });
        RootGrid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });

        var brandName = Ui.Text("Annex", wrap: false);
        brandName.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        brandName.VerticalAlignment = VerticalAlignment.Center;
        var brand = Ui.Row(Ui.AppIcon(22), brandName);
        brand.Spacing = 10;
        brand.Margin = new Thickness(20, 0, 0, 0);
        brand.VerticalAlignment = VerticalAlignment.Center;
        var titleBar = new Grid { Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent) };
        Grid.SetColumnSpan(titleBar, 2);
        titleBar.Children.Add(brand);
        RootGrid.Children.Add(titleBar);
        SetTitleBar(titleBar);

        var sidebar = new Grid { Padding = new Thickness(10, 6, 10, 12) };
        sidebar.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        sidebar.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        sidebar.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        Grid.SetRow(_sidebarFooter, 2);
        sidebar.Children.Add(_sidebarItems);
        sidebar.Children.Add(_sidebarFooter);
        Grid.SetRow(sidebar, 1);
        RootGrid.Children.Add(sidebar);

        var content = new Border
        {
            Background = Ui.Black(0.88),
            BorderBrush = Ui.White(0.07),
            BorderThickness = new Thickness(1, 1, 0, 0),
            CornerRadius = new CornerRadius(12, 0, 0, 0),
            Child = _page,
        };
        Grid.SetRow(content, 1);
        Grid.SetColumn(content, 1);
        RootGrid.Children.Add(content);
        BuildNav();
        Render();
    }

    /// <summary>The window buttons (minimise, maximise, close): white on the black title bar.</summary>
    private void StyleCaptionButtons()
    {
        // Windows 10 can't recolour them; it keeps its own.
        if (!Microsoft.UI.Windowing.AppWindowTitleBar.IsCustomizationSupported()) return;
        var bar = AppWindow.TitleBar;
        bar.PreferredHeightOption = Microsoft.UI.Windowing.TitleBarHeightOption.Tall;
        bar.ButtonBackgroundColor = Microsoft.UI.Colors.Transparent;
        bar.ButtonInactiveBackgroundColor = Microsoft.UI.Colors.Transparent;
        bar.ButtonForegroundColor = Microsoft.UI.Colors.White;
        bar.ButtonInactiveForegroundColor = Windows.UI.Color.FromArgb(140, 255, 255, 255);
        bar.ButtonHoverBackgroundColor = Windows.UI.Color.FromArgb(28, 255, 255, 255);
        bar.ButtonHoverForegroundColor = Microsoft.UI.Colors.White;
        bar.ButtonPressedBackgroundColor = Windows.UI.Color.FromArgb(46, 255, 255, 255);
        bar.ButtonPressedForegroundColor = Microsoft.UI.Colors.White;
    }

    /// <summary>The sidebar shows which page is open and whether this PC is sharing.</summary>
    private void UpdateDeviceCard() => BuildNav();

    /// <summary>A sidebar entry: white with black text when it's the open page.</summary>
    private Button NavItem(string text, string glyph, string tag, int? badge = null, bool live = false)
    {
        var selected = _pane == tag;
        var grid = new Grid { ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var icon = Ui.Glyph(glyph, 15);
        icon.VerticalAlignment = VerticalAlignment.Center;
        grid.Children.Add(icon);
        var label = Ui.Text(text, wrap: false);
        label.VerticalAlignment = VerticalAlignment.Center;
        if (selected) label.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        Grid.SetColumn(label, 1);
        grid.Children.Add(label);
        FrameworkElement? trailing = live ? Ui.LiveDot(7)
            : badge is { } n ? Ui.Text(n.ToString(), "CaptionTextBlockStyle", selected ? Ui.Black(0.55) : Ui.Secondary, wrap: false)
            : null;
        if (trailing is not null)
        {
            trailing.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(trailing, 2);
            grid.Children.Add(trailing);
        }
        var b = Ui.Flat(grid,
            selected ? Ui.White(0.92) : new SolidColorBrush(Microsoft.UI.Colors.Transparent),
            selected ? Ui.White(0.92) : Ui.White(0.06),
            selected ? Ui.Black(1) : Ui.White(0.92),
            () => Go(tag));
        b.HorizontalAlignment = HorizontalAlignment.Stretch;
        b.HorizontalContentAlignment = HorizontalAlignment.Stretch;
        b.Padding = new Thickness(12, 9, 12, 9);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, text);
        return b;
    }

    /// <summary>Three places (Nearby, This PC, Paired devices), then Settings and About at the bottom.</summary>
    private void BuildNav()
    {
        var sharing = _hosts.Count(h => h.Sharing);
        _sidebarItems.Children.Clear();
        _sidebarItems.Children.Add(NavItem("Nearby", "", "home", badge: sharing > 0 ? sharing : null));
        _sidebarItems.Children.Add(NavItem("This PC", "", "share", live: _host is not null));
        _sidebarItems.Children.Add(NavItem("Paired devices", "", "devices"));
        _sidebarFooter.Children.Clear();
        _sidebarFooter.Children.Add(NavItem("Settings", "", "settings"));
        _sidebarFooter.Children.Add(NavItem("About", "", "about"));
    }

    private void Go(string pane)
    {
        // The old Connect and per-computer pages are now Nearby.
        _pane = pane is "connect" || pane.StartsWith("peer:", StringComparison.Ordinal) ? "home" : pane;
        AnnexLog.Debug($"[app] page: {_pane}");
        BuildNav();
        Render();
    }

    /// <summary>Rebuilds the current page from state.</summary>
    private void Render()
    {
        BuildNav();
        if (_pane == "home")
        {
            _page.Content = HomePage();
            return;
        }
        var stack = Ui.Page(out var scroller);
        if (_startupError is not null)
            stack.Children.Add(Ui.Info("Annex couldn't start", _startupError, InfoBarSeverity.Error));
        switch (_pane)
        {
            case "share": SharePage(stack); break;
            case "settings": SettingsPage(stack); break;
            case "devices": DevicesPage(stack); break;
            case "about": AboutPage(stack); break;
        }
        Ui.JoinRows(stack);
        _page.Content = scroller;
    }

    private void RenderIf(params string[] panes)
    {
        UpdateKeepAwake();
        // Viewer and nearby state used to live on Connect and the per-computer pages; it's on Nearby now.
        var home = panes.Contains("home") || panes.Contains("connect") || panes.Contains("peer");
        if (panes.Contains(_pane) || (home && _pane == "home")) Render();
        else UpdateDeviceCard();
    }

    // ---- Pages ------------------------------------------------------------------------

    private void SharePage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "This PC",
            "Let another computer show this screen, or use this PC as its extra display."));
        UpdateBanner(p);
        p.Children.Add(Ui.Card("", "Share this PC", ShareStatus(), Ui.Switch(_host is not null, on => { AnnexLog.Info($"[host] Share this PC switched {(on ? "on" : "off")}"); SetHosting(on); })));
        if (_monitors.Count > 1)
        {
            var box = new ComboBox { ItemsSource = _monitors.Select(m => m.Label).ToList(), SelectedIndex = _monitorIndex, MinWidth = 220, IsEnabled = _host is null };
            box.SelectionChanged += (_, _) =>
            {
                _monitorIndex = Math.Max(0, box.SelectedIndex);
                Setting("screen to share", _monitors[_monitorIndex].Label);
            };
            p.Children.Add(Ui.Card("", "Screen to share", _host is null ? null : "Stop sharing to pick another screen.", box));
        }
        SameNameNotice(p);
        if (_hostError is not null)
            p.Children.Add(Ui.Info("Sharing problem", _hostError, InfoBarSeverity.Warning));

        if (_sessions.Count > 0)
        {
            p.Children.Add(Ui.Section("Connected now"));
            foreach (var (id, s) in _sessions)
            {
                var title = s.Mode == "extend" ? $"This PC is extending onto {Label(s.Name)}" : $"This PC's screen is on {Label(s.Name)}";
                var detail = s.Mode == "extend" ? "Arrange it in Settings → System → Display" :
                    ShowTech && _hostStream is { } st ? $"{st.Width}×{st.Height} at {st.Fps} fps" : null;
                detail = (detail is null ? "" : detail + " · ") + (s.Input ? "can use your mouse and keyboard" : "view only")
                    + (s.Relayed ? " · Relay" : "");
                p.Children.Add(Ui.Card("", title, detail, Ui.Button("Switch to direct Wi-Fi", () => _ = OfferDirectWifiToViewerAsync(id, s.Name))));
            }
        }

        if (_host is null)
        {
            p.Children.Add(Ui.Section("How it works"));
            p.Children.Add(Ui.Card("", "Turn on sharing, then pick this PC in Annex on the other computer.",
                "The first time, it asks for a PIN shown here. After that, it just connects."));
        }
        else
        {
            InternetSection(p);
            p.Children.Add(Ui.Section("On the same network"));
            var addresses = LocalAddresses();
            p.Children.Add(Ui.Card("", $"Pick “{_core?.DeviceName}” in Annex on the other computer.",
                addresses.Count == 0 ? "This PC isn't on a network. Connect it to Wi-Fi, Ethernet or a cable."
                    : "The first time, type the PIN this PC shows. Not listed? Connect by one of these addresses."
                      + (ShowTech ? $" Identity code: {Fingerprint}" : "")));
            foreach (var (ip, direct) in addresses)
            {
                var text = _host.Port == DefaultPort ? ip : $"{ip}:{_host.Port}";
                p.Children.Add(Ui.Card(direct ? "" : "", text, direct ? "Cable" : "Network", CopyButton(text)));
            }
        }

        ExtendCard(p);
        p.Children.Add(Ui.Card("", "Direct Wi-Fi",
            _hotspot.IsOn
                ? $"Network “{_hotspot.Ssid}”, password {_hotspot.Passphrase}. Other computers can join it for less lag."
                : "Runs a hotspot on this PC so another computer connects straight to it. Less lag.",
            Ui.Switch(_hotspot.IsOn, on => { Setting("direct Wi-Fi hotspot", on); _ = SetHotspotAsync(on); })));
    }

    private static Button CopyButton(string text)
    {
        var copy = new Button { Content = "Copy" };
        copy.Click += (_, _) =>
        {
            var data = new DataPackage();
            data.SetText(text);
            Clipboard.SetContent(data);
            AnnexLog.Debug("[app] Copy pressed (an address or code)");
            copy.Content = "Copied";
        };
        return copy;
    }

    // ---- Internet ---------------------------------------------------------------------

    /// <summary>Where sharing with a code is.</summary>
    private abstract record InternetShare
    {
        public sealed record Starting : InternetShare;
        public sealed record Code(string Value, DateTimeOffset Expires) : InternetShare;
        public sealed record Joining(string Name) : InternetShare;
        /// <summary>The viewer couldn't connect directly. <c>Relayed</c>: it got a relay (<c>Limited</c>: ours).</summary>
        public sealed record Relay(string Name, bool Relayed, bool Limited) : InternetShare;
        /// <summary>The code no longer works: why (null when it was used).</summary>
        public sealed record Ended(string? Why) : InternetShare;
    }

    /// <summary>This PC → Over the internet: a one-time code for a computer on another network.</summary>
    private void InternetSection(StackPanel p)
    {
        _codeCountdown = null;
        p.Children.Add(Ui.Section("Over the internet"));
        switch (_internet)
        {
            case null:
                p.Children.Add(Ui.Card("", "Share with a code", "For a computer on another network. Works once, for 10 minutes.",
                    Ui.Button("Get a code", ShareCode, accent: true)));
                break;
            case InternetShare.Starting:
                p.Children.Add(Ui.Card("", "Getting a code…", null, Ui.Spinner()));
                break;
            case InternetShare.Code c:
                p.Children.Add(CodeCard(c));
                break;
            case InternetShare.Joining j:
                p.Children.Add(Ui.Card("", $"{j.Name} is connecting…", "Typed the code. The PIN comes next if it's new.", Ui.Spinner()));
                break;
            case InternetShare.Relay { Relayed: true } r:
                p.Children.Add(Ui.Card("", $"Connecting {r.Name} through the relay…",
                    r.Limited ? "Their network blocks direct connections. Our relay passes it on, still end to end encrypted."
                        : "Their network blocks direct connections. Using your relay.", Ui.Spinner()));
                break;
            case InternetShare.Relay r:
                p.Children.Add(Ui.Card("", $"{r.Name} couldn't connect directly", "No relay is available right now."));
                break;
            case InternetShare.Ended { Why: null }:
                p.Children.Add(Ui.Card("", "Code used", "Get another one for the next computer.",
                    Ui.Button("New code", ShareCode, accent: true)));
                break;
            case InternetShare.Ended e:
                p.Children.Add(Ui.Card("", e.Why!, null, Ui.Button("Try again", ShareCode, accent: true)));
                break;
        }
        p.Children.Add(Ui.Text("The server only introduces the two computers. The screen goes straight between them, encrypted, "
            + "or through a relay when a network blocks that.", "CaptionTextBlockStyle", Ui.Secondary));
    }

    /// <summary>The code itself: large, easy to read aloud, with how long it still works.</summary>
    private Border CodeCard(InternetShare.Code c)
    {
        var grid = new Grid { ColumnSpacing = 16 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var text = new StackPanel { Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        var code = Ui.Text(c.Value, wrap: false);
        code.FontFamily = new FontFamily("Consolas");
        code.FontSize = 34;
        code.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        code.CharacterSpacing = 160;
        code.IsTextSelectionEnabled = true;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(code, "Code " + string.Join(" ", c.Value.ToCharArray()));
        text.Children.Add(code);
        _codeCountdown = Ui.Text(Countdown(c.Expires), "BodyTextBlockStyle", Ui.Secondary);
        text.Children.Add(_codeCountdown);
        grid.Children.Add(text);
        var buttons = new StackPanel { Spacing = 8, VerticalAlignment = VerticalAlignment.Center, Width = 96 };
        foreach (var b in new[] { CopyButton(c.Value), Ui.Button("Stop", StopCode) })
        {
            b.HorizontalAlignment = HorizontalAlignment.Stretch;
            buttons.Children.Add(b);
        }
        Grid.SetColumn(buttons, 1);
        grid.Children.Add(buttons);
        return Ui.Panel(grid, radius: 8, padding: new Thickness(18, 14, 18, 14));
    }

    private static string Countdown(DateTimeOffset expires)
    {
        var left = Math.Max(0, (int)(expires - DateTimeOffset.UtcNow).TotalSeconds);
        return left > 0 ? $"On the other computer: Connect by code or address. Works for {left / 60}:{left % 60:00}." : "Expired.";
    }

    /// <summary>Host: get a code for a computer on another network to type.</summary>
    private void ShareCode()
    {
        if (_host is null) return;
        _internet = new InternetShare.Starting();
        try
        {
            var relay = _settings.RelayUrl.Trim();
            AnnexLog.Info($"[host] asking for a code ({(_settings.RendezvousServer.Trim().Length == 0 ? "Annex's server" : _settings.Server)}, own relay {(relay.Length == 0 ? "no" : "yes")})");
            _host.ShareCode(_settings.Server, relay.Length == 0 ? null : relay,
                relay.Length == 0 ? null : _settings.RelayUsername, relay.Length == 0 ? null : _settings.RelayPassword);
        }
        catch (AnnexException ex)
        {
            AnnexLog.Warn($"[host] couldn't ask for a code: {ex.Message}");
            _internet = new InternetShare.Ended(FriendlyInternetError(ex.Message));
        }
        RenderIf("share");
    }

    private void StopCode()
    {
        AnnexLog.Info("[host] code stopped by the user");
        _host?.StopCode();
        _internet = null;
        RenderIf("share");
    }

    private void OnInternetEvent(CoreEvent e)
    {
        switch (e)
        {
            case CoreEvent.InternetCode c:
                _internet = new InternetShare.Code(c.Code, DateTimeOffset.UtcNow.AddSeconds(c.ExpiresIn));
                StartCodeTimer();
                break;
            case CoreEvent.InternetJoined j:
                _internet = new InternetShare.Joining(j.PeerName);
                break;
            case CoreEvent.InternetRelay r when _internet is InternetShare.Joining j:
                _internet = new InternetShare.Relay(j.Name, r.Relayed, r.Limited);
                break;
            case CoreEvent.InternetCodeEnded ended:
                _internet = (ended.Reason, _internet) switch
                {
                    ("stopped", _) => null,
                    ("used", InternetShare.Relay { Relayed: false }) => new InternetShare.Ended(
                        "The other computer's network blocks direct connections, and no relay was available. "
                        + "Add your own relay in Settings → Advanced."),
                    ("used", _) => new InternetShare.Ended(null),
                    ("expired", _) => new InternetShare.Ended("The code expired."),
                    (var r, _) => new InternetShare.Ended(FriendlyInternetError(r)),
                };
                break;
            default:
                return;
        }
        RenderIf("share");
    }

    /// <summary>Ticks the code's countdown once a second while a code is showing.</summary>
    private void StartCodeTimer()
    {
        if (_codeTimer is not null) return;
        _codeTimer = _ui.CreateTimer();
        _codeTimer.Interval = TimeSpan.FromSeconds(1);
        _codeTimer.Tick += (t, _) =>
        {
            if (_internet is not InternetShare.Code c)
            {
                t.Stop();
                _codeTimer = null;
                return;
            }
            if (_codeCountdown is not null) _codeCountdown.Text = Countdown(c.Expires);
        };
        _codeTimer.Start();
    }

    private static string FriendlyInternetError(string r)
    {
        var l = r.ToLowerInvariant();
        if (l.Contains("stun") || l.Contains("didn't answer"))
            return "Couldn't find this PC's internet address. Check the internet connection.";
        if (l.Contains("rendezvous") || l.Contains("websocket") || l.Contains("i/o") || l.Contains("dns"))
            return "Couldn't reach the server. Check the internet connection, or the server address in Settings → Advanced.";
        return r;
    }

    private void SaveInternet()
    {
        var (server, relay, user) = (_serverBox.Text.Trim(), _relayUrlBox.Text.Trim(), _relayUserBox.Text.Trim());
        if (server == _settings.RendezvousServer && relay == _settings.RelayUrl && user == _settings.RelayUsername) return;
        if (server != _settings.RendezvousServer) Setting("server", server.Length == 0 ? "Annex's" : server);
        if (relay != _settings.RelayUrl || user != _settings.RelayUsername)
            Setting("own relay", relay.Length == 0 ? "none" : $"set (username {(user.Length == 0 ? "empty" : "set")})");
        _settings.RendezvousServer = server;
        _settings.RelayUrl = relay;
        _settings.RelayUsername = user;
        _settings.Save();
    }

    /// <summary>Settings → Advanced → Internet: the server, and optionally the user's own relay.</summary>
    private void InternetSettings(StackPanel p)
    {
        p.Children.Add(Ui.Card("", "Server", "Optional. Leave empty to use Annex's server.", Ui.Reuse(_serverBox)));
        p.Children.Add(Ui.Card("", "Your relay", "Optional. A TURN server, for networks that block direct connections.",
            Ui.Reuse(_relayUrlBox)));
        p.Children.Add(Ui.Card("", "Relay username", null, Ui.Reuse(_relayUserBox)));
        p.Children.Add(Ui.Card("", "Relay password", "Encrypted for your Windows account.", Ui.Reuse(_relayPasswordBox)));
        p.Children.Add(Ui.Text("Codes use Annex's server to swap the two computers' internet addresses; the screen goes straight "
            + "between them, encrypted. Some networks block that: then it goes through a relay, yours if you set one here.",
            "CaptionTextBlockStyle", Ui.Secondary));
    }

    private static List<GpuInfo> TryGpus()
    {
        try { return [.. AnnexNative.Gpus()]; }
        catch (Exception e) when (e is AnnexException or EntryPointNotFoundException)
        {
            AnnexLog.Warn($"[display] couldn't list graphics cards: {e.Message}");
            return [];
        }
    }

    private static ExtendStatus? TryExtendStatus()
    {
        try { return AnnexNative.ExtendStatus(); }
        catch (Exception e) when (e is AnnexException or EntryPointNotFoundException)
        {
            AnnexLog.Warn($"[display] couldn't check the extend driver: {e.Message}");
            return null;
        }
    }

    /// <summary>Whether other computers can be an extra display for this PC, and what to do if not.</summary>
    private void ExtendCard(StackPanel p)
    {
        if (_extendStatus is not { } x) return;
        switch (x.State)
        {
            case "ready":
                p.Children.Add(Ui.Card("", "Extend display is ready",
                    "Other computers can be a second screen for this PC."));
                break;
            case "not_installed":
                p.Children.Add(Ui.Card("", "Extend display isn't set up",
                    "Run the installer again and tick the Extend-mode display driver."));
                break;
            default:
                var why = x.Problem == 52 && !x.TestSigning
                    ? "Windows rejected the driver's signature. Run the installer again with the driver ticked."
                    : $"The display driver isn't running (code {x.Problem}). Restart the PC or reinstall the driver.";
                var copy = new Button { Content = "Copy command" };
                copy.Click += (_, _) =>
                {
                    var data = new DataPackage();
                    data.SetText("bcdedit /set testsigning on");
                    Clipboard.SetContent(data);
                    copy.Content = "Copied";
                };
                p.Children.Add(Ui.Card("", "Extend display isn't working", why));
                break;
        }
    }

    private string ShareStatus()
    {
        if (_host is null) return "Other computers can't see this PC.";
        if (_sessions.Count > 0) return $"Connected to {string.Join(", ", _sessions.Values.Select(s => s.Name).Distinct())}.";
        return "Waiting for a connection.";
    }

    /// <summary>Shown while nothing (or nothing sharing) is found: what usually blocks discovery.</summary>
    private void NotShowingUp(StackPanel p)
    {
        p.Children.Add(Ui.Section("Can't find it?"));
        p.Children.Add(Ui.Card("", "Use the same network", "Guest Wi-Fi and VPNs can hide computers."));
        if (_firewallAllowed != true)
        {
            var allow = Ui.Button("Allow", () => _ = AllowThroughFirewallAsync());
            p.Children.Add(Ui.Card("", "Allow Annex through the firewall",
                _firewallAllowed == false ? "The firewall is blocking Annex." : null, allow));
        }
        var network = Ui.Button("Open", () => _ = Windows.System.Launcher.LaunchUriAsync(new Uri("ms-settings:network-status")));
        p.Children.Add(Ui.Card("", "Set the network to Private", "Public networks hide this PC.", network));
    }

    private const string FirewallRule = "Annex";

    private static async Task<bool> RunNetshAsync(string args, bool elevated)
    {
        try
        {
            var psi = new System.Diagnostics.ProcessStartInfo("netsh", args)
            {
                UseShellExecute = elevated,
                Verb = elevated ? "runas" : "",
                CreateNoWindow = true,
                WindowStyle = System.Diagnostics.ProcessWindowStyle.Hidden,
            };
            using var proc = System.Diagnostics.Process.Start(psi);
            if (proc is null)
            {
                AnnexLog.Warn($"[permissions] netsh didn't start ({args})");
                return false;
            }
            await proc.WaitForExitAsync();
            AnnexLog.Debug($"[permissions] netsh {args}{(elevated ? " (as admin)" : "")}: exit {proc.ExitCode}");
            return proc.ExitCode == 0;
        }
        catch (System.ComponentModel.Win32Exception e)
        {
            AnnexLog.Warn($"[permissions] netsh {(elevated ? "as admin " : "")}didn't run (UAC prompt declined?): {e.Message}");
            return false; // UAC prompt declined
        }
    }

    private async Task CheckFirewallAsync()
    {
        var ok = await RunNetshAsync($"advfirewall firewall show rule name=\"{FirewallRule}\"", elevated: false);
        AnnexLog.Info($"[permissions] firewall rule \"{FirewallRule}\": {(ok ? "present" : "missing")}");
        _ui.TryEnqueue(() =>
        {
            _firewallAllowed = ok;
            RenderIf("connect", "share");
        });
    }

    private async Task AllowThroughFirewallAsync()
    {
        var exe = Environment.ProcessPath ?? "";
        AnnexLog.Info("[permissions] Allow through the firewall pressed: asking for admin");
        await RunNetshAsync($"advfirewall firewall add rule name=\"{FirewallRule}\" dir=in action=allow program=\"{exe}\" profile=private,domain enable=yes", elevated: true);
        await CheckFirewallAsync();
    }

    /// <summary>Session-end codes from the core, in words.</summary>
    internal static string FriendlyReason(string reason, string peer, bool byCode = false) => reason switch
    {
        "unknown_code" => "That code doesn't work. Codes work once, for 10 minutes: ask for a new one.",
        "too_many_attempts" => "Too many codes tried. Wait 10 minutes, then try again.",
        "code_expired" => "The code expired. Ask for a new one.",
        _ when byCode && (reason.Contains("timed out", StringComparison.OrdinalIgnoreCase) || reason.Contains("timeout", StringComparison.OrdinalIgnoreCase))
            => "Couldn't connect across the internet. One of the networks blocks direct connections and no relay was available. "
               + "Add your own relay in Settings → Advanced on the sharing computer.",
        _ when byCode && (reason.Contains("rendezvous", StringComparison.OrdinalIgnoreCase) || reason.Contains("i/o", StringComparison.OrdinalIgnoreCase)
                          || reason.Contains("websocket", StringComparison.OrdinalIgnoreCase))
            => FriendlyInternetError(reason),
        "extend_not_supported" => $"{peer} can't extend its display. Set up Extend on that computer, or use Show its screen.",
        "busy" => $"{peer} is already sharing with 4 computers.",
        "pairing_failed" => "Wrong PIN. Try again.",
        "pairing_cancelled" => "Pairing was cancelled on the other computer.",
        "pairing_locked" => "Too many wrong PINs. Try again in 10 minutes.",
        "host_stopped" => $"{peer} stopped sharing.",
        "host_ended_session" => $"{peer} ended the session.",
        "closed" or "connection closed" => $"Disconnected from {peer}.",
        _ when reason.Contains("refused", StringComparison.OrdinalIgnoreCase) || reason.Contains("timed out", StringComparison.OrdinalIgnoreCase)
            => $"Couldn't reach {peer}. Check that it's sharing and on the same network.",
        _ => reason,
    };

    // ---- Nearby -----------------------------------------------------------------------

    /// <summary>Nearby: the computers around this PC, as a list. Click one for its card; double-click
    /// uses the default action (Settings → Viewing → Extend display by default). While nothing is
    /// found, a waiting panel says what to do on the other computer.</summary>
    private UIElement HomePage()
    {
        var root = new Grid { Padding = new Thickness(32, 22, 32, 24), RowSpacing = 16 };
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });

        // Header: a plain title; Connect by code or address on its right.
        var header = new Grid { ColumnSpacing = 12 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var titles = new StackPanel { Spacing = 4 };
        titles.Children.Add(Ui.Title("Nearby"));
        var anyFound = _hosts.Any(h => h.Sharing) || (_showNotSharing && _hosts.Count > 0);
        titles.Children.Add(Ui.Text(anyFound ? "Click a computer for options. Double-click to connect."
            : "Computers sharing on this network show up here by themselves.", "BodyTextBlockStyle", Ui.Secondary));
        header.Children.Add(titles);
        var connectOther = Ui.Flat(Ui.Text("Connect by code or address…", wrap: false), Ui.White(0.1), Ui.White(0.16), Ui.White(1),
            () => _ = ShowConnectDialogAsync(), radius: 18);
        connectOther.Padding = new Thickness(16, 7, 16, 8);
        connectOther.IsEnabled = _viewer is null;
        connectOther.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(connectOther, 1);
        header.Children.Add(connectOther);
        root.Children.Add(header);

        var notes = new StackPanel { Spacing = 8 };
        if (_startupError is not null)
            notes.Children.Add(Ui.Info("Annex couldn't start", _startupError, InfoBarSeverity.Error));
        UpdateBanner(notes);
        if (_directStatus is not null)
            notes.Children.Add(Ui.Card("", "Direct Wi-Fi", _directStatus));
        if (_viewer is null && _viewerStatus is not null)
            notes.Children.Add(Ui.Card("", _viewerStatus));
        // What's live comes first: it's what this PC is doing right now.
        var live = _viewer is not null || _sessions.Count > 0;
        if (SessionStrip() is { } strip) notes.Children.Add(strip);
        SameNameNotice(notes);
        Grid.SetRow(notes, 1);
        root.Children.Add(notes);

        var visible = _showNotSharing ? _hosts : [.. _hosts.Where(h => h.Sharing)];
        var notSharing = _hosts.Count(h => !h.Sharing);
        if (_selected is not null && !_hosts.Any(h => h.Instance == _selected)) _selected = null;

        var body = new Grid { ColumnSpacing = 18 };
        body.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        body.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var left = new Grid { RowSpacing = 10 };
        left.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        left.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        left.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        if (visible.Count > 0) left.Children.Add(SearchStatus(visible.Count));
        UIElement list;
        if (visible.Count == 0)
        {
            list = new ScrollViewer { Content = live ? null : Waiting(), VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        }
        else
        {
            var rows = new StackPanel { Spacing = 6 };
            foreach (var h in visible) rows.Children.Add(PeerRow(h));
            list = new ScrollViewer { Content = rows, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        }
        Grid.SetRow((FrameworkElement)list, 1);
        left.Children.Add(list);
        if (notSharing > 0)
        {
            var toggle = Ui.Pill(_showNotSharing ? "Hide computers that aren't sharing" : $"+{notSharing} not sharing",
                () => { _showNotSharing = !_showNotSharing; Render(); }, prominent: false);
            toggle.HorizontalAlignment = HorizontalAlignment.Left;
            Grid.SetRow(toggle, 2);
            left.Children.Add(toggle);
        }
        body.Children.Add(left);
        if (_selected is not null && _hosts.FirstOrDefault(h => h.Instance == _selected) is { } peer)
        {
            var card = PeerCard(peer);
            Grid.SetColumn(card, 1);
            body.Children.Add(card);
        }
        Grid.SetRow(body, 2);
        root.Children.Add(body);
        return root;
    }

    /// <summary>One line over the list: Annex is always looking, so say so, and say where.</summary>
    private StackPanel SearchStatus(int found)
    {
        var ring = Ui.Spinner();
        var text = found switch
        {
            0 => "Waiting for computers · looking on your network and over cables",
            1 => "1 computer · still looking",
            _ => $"{found} computers · still looking",
        };
        var row = Ui.Row(ring, Ui.Text(text, "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        row.Spacing = 10;
        row.Margin = new Thickness(4, 0, 0, 0);
        return row;
    }

    /// <summary>Nothing found yet: what to do on the other computer, and what usually hides it.</summary>
    private StackPanel Waiting()
    {
        var art = new Grid { Width = 168, Height = 168, HorizontalAlignment = HorizontalAlignment.Center };
        foreach (var (size, alpha) in new[] { (168.0, 0.06), (128.0, 0.1), (92.0, 0.16) })
            art.Children.Add(new Microsoft.UI.Xaml.Shapes.Ellipse
            {
                Width = size,
                Height = size,
                Stroke = Ui.White(alpha),
                StrokeThickness = 1,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            });
        var icon = Ui.AppIcon(60);
        icon.HorizontalAlignment = HorizontalAlignment.Center;
        icon.VerticalAlignment = VerticalAlignment.Center;
        art.Children.Add(icon);

        var panel = new StackPanel { Spacing = 8, HorizontalAlignment = HorizontalAlignment.Stretch };
        panel.Children.Add(art);
        var title = Ui.Text("Waiting for a computer…", wrap: false);
        title.FontSize = 18;
        title.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        title.HorizontalAlignment = HorizontalAlignment.Center;
        panel.Children.Add(title);
        var hint = Ui.Text("Open Annex on the other computer and turn on sharing.\nOn another network? Use Connect by code or address.",
            "BodyTextBlockStyle", Ui.Secondary);
        hint.TextAlignment = TextAlignment.Center;
        hint.HorizontalAlignment = HorizontalAlignment.Center;
        panel.Children.Add(hint);

        var outer = new StackPanel { Spacing = 0 };
        outer.Children.Add(Ui.Panel(panel, radius: 18, fill: 0.03, padding: new Thickness(24, 28, 24, 28)));
        // As wide as the panel above it, like the groups on every other page.
        var help = new StackPanel { HorizontalAlignment = HorizontalAlignment.Stretch };
        NotShowingUp(help);
        Ui.JoinRows(help);
        outer.Children.Add(help);
        return outer;
    }

    /// <summary>How this PC would reach a computer, from the addresses it announced.</summary>
    internal static (string Label, string Glyph) LinkKind(PeerInfo h) =>
        h.Mesh ? ("Private network", "\uE705")
        : h.Addresses.Any(a => a.StartsWith("169.254.", StringComparison.Ordinal) || a.StartsWith("fe80", StringComparison.OrdinalIgnoreCase))
            ? ("Cable", "")
            : h.Addresses.Any(a => a.StartsWith("192.168.137.", StringComparison.Ordinal))
                ? ("Direct Wi-Fi", "")
                : ("Network", "");

    private static string PeerGlyph(PeerInfo h)
    {
        var n = h.Name.ToLowerInvariant();
        return n.Contains("book") || n.Contains("laptop") ? "" : "";
    }

    private string PeerStatus(PeerInfo h) =>
        !h.Sharing ? "Not sharing"
        : (IsPaired(h) ? "Paired" : "New · PIN needed")
          + (h.Extend == false ? " · Extend unavailable" : "");

    /// <summary>A small chip: how the computer is reached.</summary>
    private static Border LinkChip(PeerInfo h, bool onWhite = false)
    {
        var (label, glyph) = LinkKind(h);
        var icon = Ui.Glyph(glyph, 10);
        var text = Ui.Text(label, wrap: false);
        text.FontSize = 11;
        var brush = onWhite ? Ui.Black(0.6) : Ui.Secondary;
        icon.Foreground = brush;
        text.Foreground = brush;
        var row = Ui.Row(icon, text);
        row.Spacing = 5;
        return new Border
        {
            Child = row,
            Padding = new Thickness(8, 2, 8, 3),
            CornerRadius = new CornerRadius(9),
            Background = onWhite ? Ui.Black(0.06) : Ui.White(0.07),
            VerticalAlignment = VerticalAlignment.Center,
        };
    }

    /// <summary>One computer. Click selects (its card opens), double-click connects.</summary>
    private Border PeerRow(PeerInfo h)
    {
        var selected = _selected == h.Instance;
        var grid = new Grid { ColumnSpacing = 14 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var glyph = Ui.Glyph(PeerGlyph(h), 17);
        glyph.Foreground = selected ? Ui.Black(1) : h.Sharing ? Ui.White(0.95) : Ui.Secondary;
        grid.Children.Add(new Border
        {
            Width = 38,
            Height = 38,
            CornerRadius = new CornerRadius(10),
            Background = selected ? Ui.Black(0.07) : Ui.White(0.07),
            Child = glyph,
        });
        var text = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        var name = Ui.Text(Label(h.Name), wrap: false);
        name.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        name.Foreground = selected ? Ui.Black(1) : Ui.White(0.95);
        text.Children.Add(name);
        text.Children.Add(Ui.Text(PeerStatus(h), "CaptionTextBlockStyle", selected ? Ui.Black(0.6) : Ui.Secondary, wrap: false));
        Grid.SetColumn(text, 1);
        grid.Children.Add(text);
        if (h.Sharing)
        {
            var trailing = Ui.Row(Ui.LiveDot(7), LinkChip(h, onWhite: selected));
            trailing.Spacing = 10;
            trailing.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(trailing, 2);
            grid.Children.Add(trailing);
        }
        var idle = selected ? Ui.White(0.92) : Ui.White(0.035);
        var row = new Border
        {
            Child = grid,
            Padding = new Thickness(12, 9, 14, 9),
            CornerRadius = new CornerRadius(14),
            Background = idle,
        };
        row.PointerEntered += (_, _) => { if (!selected) row.Background = Ui.White(0.07); };
        row.PointerExited += (_, _) => row.Background = idle;
        // A click rebuilds the page, so WinUI's DoubleTapped never reaches the new row: two clicks on
        // the same computer within the double-click time count as one double-click.
        Ui.Clickable(row);
        row.Tapped += (_, _) =>
        {
            var now = Environment.TickCount64;
            if (_lastTap.Instance == h.Instance && now - _lastTap.At <= GetDoubleClickTime())
            {
                _lastTap = (null, 0);
                _selected = h.Instance;
                ConnectDefault(h);
                return;
            }
            _lastTap = (h.Instance, now);
            _selected = selected ? null : h.Instance;
            Render();
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, $"{Label(h.Name)}, {PeerStatus(h)}");
        return row;
    }

    private (string? Instance, long At) _lastTap;

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial uint GetDoubleClickTime();

    /// <summary>Double-click: the default action, straight away.</summary>
    private void ConnectDefault(PeerInfo h)
    {
        if (!h.Sharing || _viewer is not null)
        {
            _selected = h.Instance;
            Render();
            return;
        }
        _extend = _settings.ExtendByDefault && h.Extend != false;
        ConnectToPeer(h);
    }

    /// <summary>The selected computer: its two actions, and its settings. Floats beside the list.</summary>
    private Border PeerCard(PeerInfo peer)
    {
        var stack = new StackPanel { Spacing = 14 };

        var top = new Grid();
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        top.Children.Add(new Border
        {
            Width = 48,
            Height = 48,
            CornerRadius = new CornerRadius(14),
            Background = Ui.White(0.08),
            HorizontalAlignment = HorizontalAlignment.Left,
            Child = Ui.Glyph(PeerGlyph(peer), 22),
        });
        var close = Ui.Flat(Ui.Glyph("", 10), Ui.White(0.08), Ui.White(0.14), Ui.White(0.9), () => { _selected = null; Render(); }, radius: 13);
        close.Width = 26;
        close.Height = 26;
        close.Padding = new Thickness(0);
        close.HorizontalContentAlignment = HorizontalAlignment.Center;
        close.VerticalAlignment = VerticalAlignment.Top;
        ToolTipService.SetToolTip(close, "Close");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(close, "Close");
        Grid.SetColumn(close, 1);
        top.Children.Add(close);
        stack.Children.Add(top);

        var heading = new StackPanel { Spacing = 6 };
        var name = Ui.Text(Label(peer.Name));
        name.FontSize = 22;
        name.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        heading.Children.Add(name);
        var status = Ui.Row(Ui.Text(IsPaired(peer) ? "Paired" : "New · asks for a PIN the first time", "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        if (peer.Sharing) status.Children.Add(LinkChip(peer));
        heading.Children.Add(status);
        stack.Children.Add(heading);

        if (!peer.Sharing)
            stack.Children.Add(Ui.Text($"Turn on sharing on {peer.Name} to connect.", "CaptionTextBlockStyle", Ui.Secondary));

        var extend = ActionButton("Extend", "This PC becomes a second screen for it.", "", prominent: true,
            () => { _extend = true; ConnectToPeer(peer); });
        extend.IsEnabled = _viewer is null && peer.Sharing && peer.Extend != false;
        var show = ActionButton("Show its screen", "Use its screen here, in a window.", "", prominent: false,
            () => { _extend = false; ConnectToPeer(peer); });
        show.IsEnabled = _viewer is null && peer.Sharing;
        var buttons = new StackPanel { Spacing = 10 };
        buttons.Children.Add(extend);
        buttons.Children.Add(show);
        stack.Children.Add(buttons);

        stack.Children.Add(new Border { Height = 1, Background = Ui.White(0.08) });
        stack.Children.Add(SmallSwitch("Direct Wi-Fi first", "This PC runs a hotspot and the other computer joins it. Less lag.",
            _settings.ConnectOverDirectWifi, on => { Setting("direct Wi-Fi first", on); _settings.ConnectOverDirectWifi = on; _settings.Save(); }));
        if (peer.Fingerprint is { } fp && _trusted.FirstOrDefault(t => t.Fingerprint == fp) is { } trusted)
        {
            // Headless lets every paired device control this PC.
            var control = SmallSwitch("Can use this PC's mouse and keyboard",
                _settings.Headless ? "Allowed while No one at this screen is on." : "When it shows this PC's screen.",
                _settings.Headless || trusted.AllowInput, on =>
                {
                    AnnexLog.Info($"[pairing] {trusted.Name} may use the mouse and keyboard: {on}");
                    _core?.SetAllowInput(trusted.Fingerprint, on);
                    RefreshTrusted();
                });
            control.IsHitTestVisible = !_settings.Headless;
            control.Opacity = _settings.Headless ? 0.6 : 1;
            stack.Children.Add(control);
            stack.Children.Add(SmallSwitch("Can hear this PC's sound", "Its own switch for each device.", trusted.AllowSound, on =>
            {
                AnnexLog.Info($"[audio] {trusted.Name} may hear this PC's sound: {on}");
                _core?.SetAllowSound(trusted.Fingerprint, on);
                RefreshTrusted();
            }));
            stack.Children.Add(SmallSwitch("Its microphone plays here", "For calls on this PC.", trusted.AllowMic, on =>
            {
                AnnexLog.Info($"[audio] {trusted.Name}'s microphone plays here: {on}");
                _core?.SetAllowMic(trusted.Fingerprint, on);
                RefreshTrusted();
            }));
            var foot = new Grid();
            foot.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            foot.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var seen = Ui.Text(LastSeen(trusted), "CaptionTextBlockStyle", Ui.Secondary);
            seen.VerticalAlignment = VerticalAlignment.Center;
            foot.Children.Add(seen);
            var forget = Ui.Pill("Forget…", () => _ = ConfirmRemove(trusted), prominent: false);
            Grid.SetColumn(forget, 1);
            foot.Children.Add(forget);
            stack.Children.Add(foot);
        }
        if (ShowTech && peer.Addresses.Length > 0)
        {
            var addresses = Ui.Text(string.Join("\n", peer.Addresses.Select(a => $"{a}:{peer.Port}")), "CaptionTextBlockStyle", Ui.Secondary);
            addresses.FontFamily = new FontFamily("Consolas");
            addresses.IsTextSelectionEnabled = true;
            stack.Children.Add(addresses);
        }

        var card = Ui.Panel(stack, radius: 22, fill: 0.07, padding: new Thickness(20));
        card.Width = 300;
        card.VerticalAlignment = VerticalAlignment.Top;
        return card;
    }

    /// <summary>A big two-line button: what it does, and what happens.</summary>
    private static Button ActionButton(string title, string detail, string glyph, bool prominent, Action click)
    {
        var grid = new Grid { ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var icon = Ui.Glyph(glyph, 17);
        icon.VerticalAlignment = VerticalAlignment.Center;
        grid.Children.Add(icon);
        var text = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        var t = Ui.Text(title, wrap: false);
        t.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        text.Children.Add(t);
        text.Children.Add(Ui.Text(detail, "CaptionTextBlockStyle", prominent ? Ui.Black(0.65) : Ui.Secondary));
        Grid.SetColumn(text, 1);
        grid.Children.Add(text);
        var arrow = Ui.Glyph("", 12);
        arrow.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(arrow, 2);
        grid.Children.Add(arrow);
        var b = Ui.Flat(grid, prominent ? Ui.White(0.94) : Ui.White(0.07), prominent ? Ui.White(1) : Ui.White(0.12),
            prominent ? Ui.Black(1) : Ui.White(0.95), click, radius: 16);
        b.HorizontalAlignment = HorizontalAlignment.Stretch;
        b.HorizontalContentAlignment = HorizontalAlignment.Stretch;
        b.Padding = new Thickness(14, 10, 14, 10);
        b.MinHeight = 60;
        return b;
    }

    /// <summary>A title and a short line on the left, a switch on the right.</summary>
    private static Grid SmallSwitch(string title, string detail, bool on, Action<bool> changed)
    {
        var grid = new Grid { ColumnSpacing = 10 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var text = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        var t = Ui.Text(title);
        t.FontSize = 13;
        text.Children.Add(t);
        text.Children.Add(Ui.Text(detail, "CaptionTextBlockStyle", Ui.Secondary));
        grid.Children.Add(text);
        var toggle = new ToggleSwitch { IsOn = on, OnContent = "", OffContent = "", MinWidth = 0, VerticalAlignment = VerticalAlignment.Center };
        toggle.Toggled += (_, _) => changed(toggle.IsOn);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(toggle, title);
        Grid.SetColumn(toggle, 1);
        grid.Children.Add(toggle);
        return grid;
    }

    /// <summary>What's live, in numbers big enough to read from across the room.</summary>
    private Border? SessionStrip()
    {
        if (_viewer is null && _sessions.Count == 0) return null;
        var grid = new Grid { ColumnSpacing = 28 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var head = new StackPanel { Spacing = 4, VerticalAlignment = VerticalAlignment.Center };
        var role = Role();
        var liveLabel = Ui.Text(role is { } r ? $"LIVE · {r.Headline.ToUpperInvariant()}" : "LIVE", "CaptionTextBlockStyle", Ui.Live, wrap: false);
        liveLabel.FontWeight = Microsoft.UI.Text.FontWeights.Bold;
        liveLabel.CharacterSpacing = 140;
        var live = Ui.Row(Ui.LiveDot(8), liveLabel);
        head.Children.Add(live);
        var first = _sessions.Values.FirstOrDefault();
        if (role is { } flow)
        {
            var arrow = Ui.Flow(flow.From, flow.To, flow.ThisIsFrom, size: 14);
            arrow.Margin = new Thickness(0, 2, 0, 2);
            head.Children.Add(arrow);
        }
        else
        {
            var title = Ui.Text(_viewerStatus ?? "Connecting…", wrap: false);
            title.FontSize = 18;
            title.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
            head.Children.Add(title);
        }
        string subtitle = _viewer is not null
            ? _controlling ? "You're in control · Ctrl+Alt+Shift+Q to leave"
                : _onDirectWifi ? $"On direct Wi-Fi “{_hotspot.Ssid}”"
                : "Press Esc to leave the viewer · encrypted end to end"
            : first.Mode == "extend" ? "Arrange it in Settings → System → Display"
            : first.Input ? $"{Label(first.Name)} can use your mouse and keyboard" : "View only: it can't use your mouse or keyboard";
        head.Children.Add(Ui.Text(subtitle, "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        grid.Children.Add(head);

        var stats = new List<(string Value, string Unit, string Label)>();
        if (_viewer is not null && _lat is { } l)
        {
            stats.Add(($"{l.P50Ms:F0}", "ms", ShowTech ? $"Delay (p95 {l.P95Ms:F0} ms)" : "Delay"));
            stats.Add(($"{l.Fps:F0}", "fps", "Smoothness"));
        }
        var column = 1;
        foreach (var (value, unit, label) in stats)
        {
            var big = Ui.Text(value, wrap: false);
            big.FontSize = 38;
            big.FontWeight = Microsoft.UI.Text.FontWeights.Light;
            var u = Ui.Text(unit, "CaptionTextBlockStyle", Ui.Secondary, wrap: false);
            u.VerticalAlignment = VerticalAlignment.Bottom;
            u.Margin = new Thickness(0, 0, 0, 8);
            var number = Ui.Row(big, u);
            number.Spacing = 4;
            var cell = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
            cell.Children.Add(number);
            cell.Children.Add(Ui.Text(label, "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            Grid.SetColumn(cell, column++);
            grid.Children.Add(cell);
        }

        var buttons = new StackPanel { Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
        buttons.Children.Add(_viewer is not null
            ? Ui.Pill("Disconnect", Disconnect, prominent: true)
            : Ui.Pill("Stop sharing", () => SetHosting(false), prominent: true));
        if (_viewer is not null && !_onDirectWifi && _directPending is null)
            buttons.Children.Add(Ui.Pill("Switch to direct Wi-Fi", () => _ = SwitchToDirectWifiAsync(), prominent: false));
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(buttons, column);
        grid.Children.Add(buttons);

        var strip = Ui.Panel(grid, radius: 22, fill: 0.07, padding: new Thickness(22, 16, 22, 16));
        strip.BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(90, 61, 209, 133));
        return strip;
    }

    private ContentDialog? _connectDialog;

    /// <summary>Connect by code or address: for a computer that isn't listed. A code (from a computer
    /// sharing over the internet) wins when both are filled in.</summary>
    private async Task ShowConnectDialogAsync()
    {
        if (_connectDialog is not null) return;
        // The boxes outlive the dialog: start empty each time.
        ResetConnectDialog();
        var panel = new StackPanel { Spacing = 10, MinWidth = 420 };
        panel.Children.Add(Ui.Text("For a computer that isn't listed: on another network, or over Tailscale or ZeroTier.",
            "BodyTextBlockStyle", Ui.Secondary));
        panel.Children.Add(Ui.Eyebrow("Code"));
        var code = Ui.Reuse(_codeBox);
        code.HorizontalAlignment = HorizontalAlignment.Stretch;
        code.FontFamily = new FontFamily("Consolas");
        panel.Children.Add(code);
        panel.Children.Add(Ui.Text("The other computer shows it under Over the internet. It works once.", "CaptionTextBlockStyle", Ui.Secondary));
        panel.Children.Add(Ui.Eyebrow("Address"));
        var box = Ui.Reuse(_addressBox);
        box.HorizontalAlignment = HorizontalAlignment.Stretch;
        panel.Children.Add(box);
        panel.Children.Add(SmallSwitch("Direct Wi-Fi first", "This PC runs a hotspot and the other computer joins it. Less lag.",
            _settings.ConnectOverDirectWifi, on => { Setting("direct Wi-Fi first", on); _settings.ConnectOverDirectWifi = on; _settings.Save(); }));
        _connectDialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            RequestedTheme = ElementTheme.Dark,
            Title = DialogTitle("Connect by code or address", () => _connectDialog?.Hide()),
            Content = panel,
            PrimaryButtonText = "Show its screen",
            SecondaryButtonText = "Extend display",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        _connectDialog.Opened += (_, _) => _codeBox.Focus(FocusState.Programmatic);
        try
        {
            var result = await Ui.PointingHand(_connectDialog).ShowAsync();
            AnnexLog.Debug($"[viewer] Connect by code or address: {result}");
            if (result == ContentDialogResult.Primary || result == ContentDialogResult.Secondary)
            {
                _extend = result == ContentDialogResult.Secondary;
                if (_codeBox.Text.Trim().Length > 0) ConnectByCode();
                else ConnectManual();
            }
        }
        catch (Exception e)
        {
            // Another dialog is open (a PIN prompt).
            AnnexLog.Warn($"[viewer] couldn't show Connect by code or address: {e.Message}");
        }
        finally
        {
            _connectDialog = null;
            // Connected, cancelled or failed: what was typed has been used.
            ResetConnectDialog();
        }
    }

    private void ResetConnectDialog()
    {
        _codeBox.Text = "";
        _addressBox.Text = "";
    }

    /// <summary>A dialog title with a close (X) button at its right end. The title area doesn't stretch,
    /// so it takes the content's width (<paramref name="width"/>) to put the X at the edge.</summary>
    private static Grid DialogTitle(string title, Action close, double width = 420)
    {
        var grid = new Grid { MinWidth = width };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var text = Ui.Text(title, wrap: false);
        text.VerticalAlignment = VerticalAlignment.Center;
        grid.Children.Add(text);
        var x = Ui.IconButton("\uE711", "Close", close);
        x.VerticalAlignment = VerticalAlignment.Center;
        x.Margin = new Thickness(12, 0, -6, 0);
        Grid.SetColumn(x, 1);
        grid.Children.Add(x);
        return grid;
    }

    // ---- What's new ---------------------------------------------------------------------

    /// <summary>Once the window is first on screen: the notes for what changed since the version that last ran,
    /// then remember this version. A first install shows nothing. 0.3.0 didn't record a version, so a PC
    /// that ran Annex before without one is taken to be upgrading from 0.3.0.</summary>
    private void ScheduleWhatsNew()
    {
        string current;
        try { current = AnnexNative.Version; }
        catch (Exception e) when (e is DllNotFoundException or EntryPointNotFoundException or BadImageFormatException) { return; }
        if (current.Length == 0 || _settings.LastSeenVersion == current) return;
        var lastSeen = _settings.LastSeenVersion ?? (_settings.UsedBefore ? "0.3.0" : null);
        IReadOnlyList<WhatsNewRelease> notes = [];
        if (lastSeen is not null)
        {
            try { notes = AnnexNative.WhatsNew(lastSeen); }
            catch (Exception e) when (e is System.Text.Json.JsonException or DllNotFoundException or EntryPointNotFoundException or BadImageFormatException)
            {
                AnnexLog.Warn($"[update] couldn't read what's new: {e.Message}");
            }
        }
        if (notes.Count == 0 || Environment.GetEnvironmentVariable("ANNEX_SELFTEST") == "1")
        {
            AnnexLog.Info(lastSeen is null ? $"[update] first run of {current}: no what's new" : $"[update] nothing new to show since {lastSeen}");
            MarkWhatsNewSeen(current);
            return;
        }
        // Started in the tray: wait until the window is opened.
        RoutedEventHandler loaded = null!;
        loaded = (_, _) =>
        {
            RootGrid.Loaded -= loaded;
            _ui.TryEnqueue(async () =>
            {
                AnnexLog.Info($"[update] showing what's new for {current} (since {lastSeen})");
                if (await ShowWhatsNewAsync(notes)) MarkWhatsNewSeen(current);
            });
        };
        RootGrid.Loaded += loaded;
    }

    private void MarkWhatsNewSeen(string version)
    {
        _settings.LastSeenVersion = version;
        _settings.Save();
    }

    /// <summary>The release notes, grouped New / Improved / Fixed under each version, newest first.
    /// False if it couldn't be shown (another dialog is open).</summary>
    private async Task<bool> ShowWhatsNewAsync(IReadOnlyList<WhatsNewRelease> releases)
    {
        if (releases.Count == 0) return false;
        var panel = new StackPanel { Spacing = 4, MinWidth = 420, MaxWidth = 520 };
        foreach (var release in releases)
        {
            if (releases.Count > 1)
            {
                var heading = Ui.Text($"Annex {release.Version}", "SubtitleTextBlockStyle", wrap: false);
                heading.Margin = new Thickness(0, panel.Children.Count == 0 ? 0 : 18, 0, 0);
                panel.Children.Add(heading);
            }
            foreach (var (kind, label) in new[] { ("new", "New"), ("improved", "Improved"), ("fixed", "Fixed") })
            {
                var items = release.Items.Where(i => i.Kind == kind).ToList();
                if (items.Count == 0) continue;
                var eyebrow = Ui.Eyebrow(label);
                eyebrow.Margin = new Thickness(0, 14, 0, 4);
                panel.Children.Add(eyebrow);
                foreach (var item in items)
                {
                    var entry = new StackPanel { Spacing = 2, Margin = new Thickness(0, 0, 0, 10) };
                    entry.Children.Add(Ui.Bold(item.Title));
                    entry.Children.Add(Ui.Text(item.Detail, "BodyTextBlockStyle", Ui.Secondary));
                    panel.Children.Add(entry);
                }
            }
        }
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            RequestedTheme = ElementTheme.Dark,
            Title = releases.Count == 1 ? $"What's new in Annex {releases[0].Version}" : "What's new in Annex",
            Content = panel,
            CloseButtonText = "Got it",
            DefaultButton = ContentDialogButton.Close,
        };
        try
        {
            await Ui.PointingHand(dialog).ShowAsync();
            AnnexLog.Info("[update] what's new seen");
            return true;
        }
        catch (Exception e)
        {
            AnnexLog.Warn($"[update] couldn't show what's new: {e.Message}");
            return false;
        }
    }

    /// <summary>Viewer: start this PC's hotspot, hand its name and passphrase to the host inside the
    /// encrypted session, then reconnect once the host appears on the hotspot.</summary>
    private async Task SwitchToDirectWifiAsync()
    {
        // SessionStarted and DirectLinkRequested can both ask for the switch: one offer is enough.
        if (_viewer is null || _viewerFingerprint is null || _directStarting) return;
        _directStarting = true;
        AnnexLog.Info($"[viewer] switching {_viewingName} to direct Wi-Fi");
        _directStatus = "Starting this PC's hotspot…";
        RenderIf("connect", "peer");
        string? error;
        try
        {
            error = await _hotspot.StartAsync();
        }
        finally
        {
            _directStarting = false;
        }
        if (error is not null || _viewer is null)
        {
            AnnexLog.Warn($"[viewer] direct Wi-Fi switch stopped: {error ?? "disconnected before the switch"}");
            _directStatus = error ?? "Disconnected before the switch.";
            RenderIf("connect", "peer");
            return;
        }
        _directPending = (_viewerFingerprint, _viewingName, _viewerExtend, DateTimeOffset.UtcNow.AddSeconds(60));
        _viewer.OfferDirectLink(_hotspot.Ssid!, _hotspot.Passphrase!);
        AnnexLog.Info($"[viewer] asked {_viewingName} to join \"{_hotspot.Ssid}\", waiting up to 60 s for it on the hotspot");
        _directStatus = $"Asking {_viewingName} to join “{_hotspot.Ssid}”. The picture pauses for a moment.";
        RenderIf("connect", "peer");
        var timer = _ui.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(61);
        timer.IsRepeating = false;
        timer.Tick += (_, _) =>
        {
            if (_directPending is { } d && DateTimeOffset.UtcNow >= d.Until)
            {
                _directPending = null;
                AnnexLog.Warn($"[viewer] {d.Name} didn't come back on direct Wi-Fi within 60 s");
                _directStatus = $"{d.Name} didn't come back on direct Wi-Fi. Reconnect from Nearby.";
                _ = _hotspot.StopAsync();
                RenderIf("connect", "peer");
            }
        };
        timer.Start();
    }

    /// <summary>Connect page: move new sessions onto this PC's hotspot right after they connect.</summary>
    private void DirectWifiOption(StackPanel p)
    {
        p.Children.Add(Ui.Card("", "Connect over direct Wi-Fi",
            "After connecting, this PC starts a hotspot and the other computer joins it. Less lag.",
            Ui.Switch(_settings.ConnectOverDirectWifi, on =>
            {
                Setting("connect over direct Wi-Fi", on);
                _settings.ConnectOverDirectWifi = on;
                _settings.Save();
            })));
    }

    /// <summary>The Direct Wi-Fi switch on the Share page.</summary>
    private async Task SetHotspotAsync(bool on)
    {
        _hostError = on ? await _hotspot.StartAsync() : null;
        if (!on) await _hotspot.StopAsync();
        RenderIf("share");
    }

    /// <summary>Host: move one viewer onto this PC's hotspot (it joins, then reconnects there).</summary>
    private async Task OfferDirectWifiToViewerAsync(ulong sessionId, string name)
    {
        AnnexLog.Info($"[host] Switch to direct Wi-Fi pressed for {name}");
        var error = await _hotspot.StartAsync();
        if (error is null && _host?.OfferDirectLink(sessionId, _hotspot.Ssid!, _hotspot.Passphrase!) == true)
        {
            AnnexLog.Info($"[host] offered \"{_hotspot.Ssid}\" to {name}");
            _hostError = null;
        }
        else
        {
            AnnexLog.Warn($"[host] direct Wi-Fi for {name} didn't start: {error ?? "not connected any more"}");
            _hostError = error ?? $"{name} isn't connected any more.";
        }
        RenderIf("share");
    }

    /// <summary>Discovery found the host on the hotspot: reconnect there. The old connection is
    /// replaced right away instead of waiting for it to time out on the network it left.</summary>
    private void TryDirectReconnect(PeerInfo peer)
    {
        if (_directPending is not { } d || peer.Fingerprint != d.Fingerprint) return;
        var direct = peer.Addresses.FirstOrDefault(a => a.StartsWith("192.168.137.", StringComparison.Ordinal));
        if (direct is null) return;
        _directPending = null;
        _directStatus = null;
        _extend = d.Extend;
        _onDirectWifi = true;
        AnnexLog.Info($"[viewer] {d.Name} is on the hotspot at {direct}, reconnecting there");
        Connect($"{direct}:{peer.Port}", d.Name, d.Fingerprint);
    }

    /// <summary>The five settings most people change, then everything else under Advanced, closed by default.</summary>
    private void SettingsPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("\uE713", "Settings", "How Annex starts, shares and shows other computers."));
        GeneralSection(p);
        p.Children.Add(AdvancedHeader());
        if (!_showAdvanced) return;
        p.Children.Add(Ui.Section("Sharing"));
        SharingSection(p);
        p.Children.Add(Ui.Section("Viewing"));
        ViewingSection(p);
        p.Children.Add(Ui.Section("Sound"));
        SoundSection(p);
        p.Children.Add(Ui.Section("Internet"));
        InternetSettings(p);
        p.Children.Add(Ui.Section("Developer"));
        DeveloperSection(p);
    }

    /// <summary>Opens and closes Settings → Advanced.</summary>
    private Button AdvancedHeader()
    {
        var header = new Grid { ColumnSpacing = 8 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var chevron = Ui.Glyph(_showAdvanced ? "\uE70D" : "\uE76C", 12);
        chevron.VerticalAlignment = VerticalAlignment.Center;
        var title = Ui.Text("Advanced", "BodyStrongTextBlockStyle", wrap: false);
        Grid.SetColumn(title, 1);
        header.Children.Add(chevron);
        header.Children.Add(title);
        if (!_showAdvanced)
        {
            var hint = Ui.Text("You don't need to change these.", "CaptionTextBlockStyle", Ui.Secondary, wrap: false);
            hint.HorizontalAlignment = HorizontalAlignment.Right;
            hint.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(hint, 2);
            header.Children.Add(hint);
        }
        var button = new Button
        {
            Content = header,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
            BorderThickness = new Thickness(0),
            Padding = new Thickness(4, 8, 4, 8),
            Margin = new Thickness(0, 12, 0, 0),
        };
        button.Click += (_, _) => { _showAdvanced = !_showAdvanced; Render(); };
        return button;
    }

    /// <summary>Identity codes, addresses, stream and latency figures (Settings → Developer).</summary>
    private bool ShowTech => _settings.ShowTechnicalDetails;

    private void GeneralSection(StackPanel p)
    {
        var quality = new ComboBox
        {
            ItemsSource = new[] { "Best", "Balanced", "Fast (Wi-Fi)" },
            SelectedIndex = (int)_settings.Quality,
            MinWidth = 220,
        };
        quality.SelectionChanged += (_, _) =>
        {
            _settings.Quality = (Quality)Math.Max(0, quality.SelectedIndex);
            Setting("quality", _settings.Quality);
            _settings.Save();
        };
        p.Children.Add(Ui.Card("", "Device name",
            "Shown to other computers. Leave empty to use this PC's name.", Ui.Reuse(_nameBox)));
        p.Children.Add(Ui.Card("", "Start with Windows", "Opens in the tray when you sign in.",
            Ui.Switch(AppSettings.StartWithWindows, on =>
            {
                Setting("start with Windows", on);
                try { AppSettings.StartWithWindows = on; }
                catch (Exception e) when (e is UnauthorizedAccessException or System.Security.SecurityException or IOException)
                {
                    AnnexLog.Warn($"[settings] couldn't change Start with Windows: {e.Message}");
                }
            })));
        p.Children.Add(Ui.Card("", "Start sharing when Annex opens", null,
            Ui.Switch(_settings.StartSharingAtLaunch, on => { Setting("start sharing at launch", on); _settings.StartSharingAtLaunch = on; _settings.Save(); })));
        p.Children.Add(Ui.Card("", "No one at this screen",
            "For a PC reached remotely: pairing PINs also go to pairing-pin.txt in Annex's data folder, and paired devices can control it without asking.",
            Ui.Switch(_settings.Headless, on => { Setting("no one at this screen", on); _settings.Headless = on; _settings.Save(); _core?.SetHeadless(on); RefreshTrusted(); })));
        p.Children.Add(Ui.Card("", "Quality",
            "Use Fast if the picture stutters on Wi-Fi.", quality));
        p.Children.Add(Ui.Card("", "Control the other computer",
            "When it allows it. Press Ctrl+Alt+Shift+Q to leave.",
            Ui.Switch(_settings.UseMouseAndKeyboard, on => { Setting("control the other computer", on); _settings.UseMouseAndKeyboard = on; _settings.Save(); })));
    }

    private void SharingSection(StackPanel p)
    {
        var gpus = TryGpus();
        if (gpus.Count > 1)
        {
            var names = new List<string> { "Automatic" };
            names.AddRange(gpus.Select(g => g.Name));
            var selected = gpus.FindIndex(g => g.Luid == _settings.GpuLuid) + 1;
            var gpuBox = new ComboBox { ItemsSource = names, SelectedIndex = selected, MinWidth = 220, IsEnabled = _host is null };
            gpuBox.SelectionChanged += (_, _) =>
            {
                _settings.GpuLuid = gpuBox.SelectedIndex <= 0 ? 0 : gpus[gpuBox.SelectedIndex - 1].Luid;
                Setting("graphics card", gpuBox.SelectedIndex <= 0 ? "automatic" : $"{gpus[gpuBox.SelectedIndex - 1].Name} ({_settings.GpuLuid:x})");
                _settings.Save();
            };
            p.Children.Add(Ui.Card("", "Graphics card",
                _host is null ? "Automatic uses the one driving this screen." : "Stop sharing to change this.", gpuBox));
        }
        p.Children.Add(Ui.Card("", "Hardware video encoder",
            _host is null ? "Recommended. Lowest delay." : "Stop sharing to change this.",
            Ui.Switch(_settings.PreferHardwareEncoder, on => { Setting("hardware video encoder", on); _settings.PreferHardwareEncoder = on; _settings.Save(); }, enabled: _host is null)));
    }

    private void ViewingSection(StackPanel p)
    {
        p.Children.Add(Ui.Card("", "Extend display by default", null,
            Ui.Switch(_settings.ExtendByDefault, on => { Setting("extend display by default", on); _settings.ExtendByDefault = on; _extend = on; _settings.Save(); })));
        DirectWifiOption(p);
        p.Children.Add(Ui.Card("", "Share the clipboard",
            "Copy on one computer, paste on the other, while one controls the other. Passwords copied from a password manager stay here.",
            Ui.Switch(_settings.ShareClipboard, on =>
            {
                Setting("share the clipboard", on);
                _settings.ShareClipboard = on;
                _settings.Save();
                if (on) _clipboard.Start(); else { _clipboard.Stop(); _copiedWhileAway = false; }
            })));
        p.Children.Add(Ui.Card("", "Drop files at the top of the screen",
            "While connected, drag files to the top of the screen to send them.",
            Ui.Switch(_settings.DropAtTop, on => { Setting("drop files at the top", on); _settings.DropAtTop = on; _settings.Save(); UpdateDropTarget(); })));
        p.Children.Add(Ui.Card("", "Receive files", "Files the other computer sends go to Downloads.",
            Ui.Switch(_settings.ReceiveFiles, on =>
            {
                Setting("receive files", on);
                _settings.ReceiveFiles = on;
                _settings.Save();
                _core?.SetDownloads(on ? DownloadsFolder : null);
            })));
    }

    private void SoundSection(StackPanel p)
    {
        p.Children.Add(Ui.Card("", "Share this computer's sound", "People viewing this computer hear what it plays.",
            Ui.Switch(_settings.ShareSound, on => { Setting("share this computer's sound", on); _settings.ShareSound = on; _settings.Save(); ApplyAudio(); })));
        p.Children.Add(Ui.Card("", "Hear the other computer", "Plays the shared computer's sound here, and a viewer's microphone.",
            Ui.Switch(_settings.HearSound, on => { Setting("hear the other computer", on); _settings.HearSound = on; _settings.Save(); ApplyAudio(); })));
        p.Children.Add(Ui.Card("", "Send my microphone", "Your voice plays on the computer you're viewing. Annex takes its sound back out, so neither side hears an echo.",
            Ui.Switch(_settings.SendMicrophone, on => { Setting("send my microphone", on); _settings.SendMicrophone = on; _settings.Save(); ApplyAudio(); })));
        if (CoreClient.VirtualMicrophoneSupported)
            p.Children.Add(Ui.Card("", "Use a viewer's microphone as Annex Microphone",
                "Apps here, like Zoom or Meet, can pick Annex Microphone. Off: it plays aloud.",
                Ui.Switch(_settings.VirtualMicrophone, on => { Setting("viewer's microphone as Annex Microphone", on); _settings.VirtualMicrophone = on; _settings.Save(); ApplyAudio(); })));
    }

    /// <summary>Hands the Sound settings to the core (at startup and on every change).</summary>
    private void ApplyAudio()
    {
        if (_core is null) return;
        try { _core.SetAudio(_settings.ShareSound, _settings.HearSound, _settings.SendMicrophone, _settings.VirtualMicrophone); }
        catch (AnnexException e) { AnnexLog.Warn($"[settings] couldn't apply sound settings: {e.Message}"); }
    }

    // Clipboard and files.

    /// <summary>Who dropped files go to: the host this PC controls, else a viewer controlling this PC.</summary>
    private void UpdateDropTarget()
    {
        string? peer = null;
        if (_settings.DropAtTop)
        {
            if (_viewer is not null && _controlling) peer = Label(_viewingName);
            else if (_sessions.Values.FirstOrDefault(s => s.Input) is { Name: { } name } && _sessions.Values.Any(s => s.Input))
                peer = Label(name);
        }
        _drop?.SetTarget(peer);
    }

    /// <summary>After the event has updated the session state.</summary>
    private void UpdateDropTargetSoon() => _ui.TryEnqueue(UpdateDropTarget);

    private void SendFiles(IReadOnlyList<string> files)
    {
        AnnexLog.Info(_viewer is not null && _controlling
            ? $"[transfer] sending {files.Count} file(s) to {_viewingName}"
            : $"[transfer] sending {files.Count} file(s) to {_sessions.Values.Count(s => s.Input)} viewer(s) controlling this PC");
        foreach (var file in files)
        {
            try
            {
                if (_viewer is not null && _controlling) _viewer.SendFile(file);
                else
                    foreach (var (id, s) in _sessions)
                        if (s.Input) _host?.SendFile(id, file);
            }
            catch (AnnexException e)
            {
                AnnexLog.Warn($"[transfer] couldn't send {file}: {e.Message}");
                _hostError = $"Couldn't send {Path.GetFileName(file)}: {e.Message}";
            }
        }
    }

    /// <summary><paramref name="viewer"/>: the event came from the viewer (this PC watching another),
    /// not from a host session, so the island's buttons go back to the right one.</summary>
    private void OnTransfer(CoreEvent e, bool viewer)
    {
        switch (e)
        {
            case CoreEvent.TransferStarted t: _drop?.Started(viewer, t.SessionId, t.Id, t.Incoming, t.Name, t.Size); break;
            case CoreEvent.TransferProgress p: _drop?.Progress(viewer, p.SessionId, p.Id, p.Incoming, p.Done); break;
            case CoreEvent.TransferEnded d: _drop?.Ended(viewer, d.SessionId, d.Id, d.Incoming, d.Ok, d.Path, d.Problem); break;
        }
    }

    /// <summary>Pause/Resume pressed on a file being sent in the island.</summary>
    private void PauseTransfer(bool viewer, ulong session, ulong id, bool paused)
    {
        try
        {
            if (viewer) _viewer?.PauseTransfer(id, paused);
            else _host?.PauseTransfer(session, id, paused);
        }
        catch (AnnexException e)
        {
            AnnexLog.Warn($"[transfer] couldn't {(paused ? "pause" : "resume")} file {id}: {e.Message}");
        }
    }

    /// <summary>Unsend (a file going) or Stop (a file arriving) pressed in the island.</summary>
    private void CancelTransfer(bool viewer, ulong session, ulong id, bool incoming)
    {
        if (viewer) _viewer?.CancelTransfer(incoming, id);
        else _host?.CancelTransfer(session, incoming, id);
    }

    private static string DownloadsFolder => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads");

    /// <summary>Copied on this PC: to the host this PC controls (if copied elsewhere, once its window
    /// comes to the front, and only what's on the clipboard then), and to the viewers controlling
    /// this PC.</summary>
    private void OnCopied(string mime, byte[] data)
    {
        try
        {
            var viewers = 0;
            foreach (var (id, s) in _sessions)
                if (s.Input && _host is not null)
                {
                    _host.SendClipboard(id, mime, data);
                    viewers++;
                }
            if (viewers > 0) AnnexLog.Debug($"[clipboard] sent {mime}, {data.Length} bytes to {viewers} viewer(s) controlling this PC");
            if (_viewer is not null && _controlling)
            {
                if (ViewerInFront())
                {
                    _viewer.SendClipboard(mime, data);
                    AnnexLog.Debug($"[clipboard] sent {mime}, {data.Length} bytes to {_viewingName}");
                }
                else
                {
                    AnnexLog.Debug("[clipboard] not sent yet: the viewer isn't in front, sending once it is");
                    WaitForViewer();
                }
            }
            else if (viewers == 0)
            {
                AnnexLog.Debug($"[clipboard] not sent: {(_viewer is not null ? "this PC isn't controlling the other computer" : "nothing connected with control")}");
            }
        }
        catch (AnnexException e)
        {
            // The session just ended.
            AnnexLog.Warn($"[clipboard] couldn't send: {e.Message}");
        }
    }

    /// <summary>Checks twice a second for the viewer window to come to the front, then sends what's on
    /// the clipboard then.</summary>
    private void WaitForViewer()
    {
        _copiedWhileAway = true;
        if (_clipboardTimer is null)
        {
            _clipboardTimer = _ui.CreateTimer();
            _clipboardTimer.Interval = TimeSpan.FromMilliseconds(500);
            _clipboardTimer.Tick += async (t, _) =>
            {
                if (!_copiedWhileAway || _viewer is null || !_controlling)
                {
                    if (_copiedWhileAway) AnnexLog.Debug("[clipboard] stopped waiting for the viewer: the session ended or control went");
                    _copiedWhileAway = false;
                    t.Stop();
                }
                else if (ViewerInFront())
                {
                    _copiedWhileAway = false;
                    t.Stop();
                    if (await _clipboard.ReadNowAsync("re-read as the viewer came to the front") is var (mime, data) && _viewer is { } v)
                    {
                        try
                        {
                            v.SendClipboard(mime, data);
                            AnnexLog.Debug($"[clipboard] sent {mime}, {data.Length} bytes to {_viewingName}");
                        }
                        catch (AnnexException e)
                        {
                            AnnexLog.Warn($"[clipboard] couldn't send: {e.Message}");
                        }
                    }
                }
            };
        }
        _clipboardTimer.Start();
    }

    /// <summary>The viewer window (the core's, in this process) is the one in front.</summary>
    private bool ViewerInFront()
    {
        var front = GetForegroundWindow();
        return front != 0 && front != WinRT.Interop.WindowNative.GetWindowHandle(this)
            && GetWindowThreadProcessId(front, out var pid) != 0 && pid == (uint)Environment.ProcessId;
    }

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial nint GetForegroundWindow();

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial uint GetWindowThreadProcessId(nint hwnd, out uint processId);

    private void DevicesPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("\uE716", "Paired devices", "Paired devices connect without a PIN."));
        DevicesSection(p);
    }

    private void DevicesSection(StackPanel p)
    {
        if (_trusted.Any(t => t.AllowInput))
            p.Children.Add(Ui.Info("Note", "Remote control can't type into administrator windows or UAC prompts.",
                InfoBarSeverity.Informational));
        if (_trusted.Count == 0)
            p.Children.Add(Ui.Card("", "No paired devices", "Pair by connecting from another computer."));
        foreach (var peer in _trusted)
        {
            var remove = new Button { Content = "Remove…" };
            remove.Click += async (_, _) => await ConfirmRemove(peer);
            p.Children.Add(Ui.Card("", peer.Name, LastSeen(peer), remove));
            p.Children.Add(Ui.Card("", "Can use this PC's mouse and keyboard",
                _settings.Headless ? "Allowed while No one at this screen is on." : null, Ui.Switch(_settings.Headless || peer.AllowInput, on =>
            {
                AnnexLog.Info($"[pairing] {peer.Name} may use the mouse and keyboard: {on}");
                _core?.SetAllowInput(peer.Fingerprint, on);
                RefreshTrusted();
            }, enabled: !_settings.Headless)));
            p.Children.Add(Ui.Card("", "Can hear this PC's sound",
                _settings.ShareSound ? null : "This PC's sound is off in Settings.", Ui.Switch(peer.AllowSound, on =>
            {
                AnnexLog.Info($"[audio] {peer.Name} may hear this PC's sound: {on}");
                _core?.SetAllowSound(peer.Fingerprint, on);
                RefreshTrusted();
            })));
            p.Children.Add(Ui.Card("", "Its microphone plays here", null, Ui.Switch(peer.AllowMic, on =>
            {
                AnnexLog.Info($"[audio] {peer.Name}'s microphone plays here: {on}");
                _core?.SetAllowMic(peer.Fingerprint, on);
                RefreshTrusted();
            })));
            // A gap keeps each device in its own group.
            p.Children.Add(new Border { Height = 10 });
        }
        if (ShowTech)
            p.Children.Add(Ui.Text($"This PC's identity code: {Fingerprint}", "CaptionTextBlockStyle", Ui.Secondary));
    }

    private const string RepoUrl = "https://github.com/NRanjan-17/Annex";

    private static Button LinkButton(string text, string url) =>
        Ui.Button(text, () => _ = Windows.System.Launcher.LaunchUriAsync(new Uri(url)));

    /// <summary>About: the app, then everything about updates in one group.</summary>
    private void AboutPage(StackPanel p)
    {
        var head = new StackPanel { HorizontalAlignment = HorizontalAlignment.Center, Spacing = 4, Padding = new Thickness(24, 28, 24, 22) };
        var icon = Ui.AppIcon(96);
        icon.HorizontalAlignment = HorizontalAlignment.Center;
        head.Children.Add(icon);
        var name = Ui.Text("Annex", "TitleTextBlockStyle", wrap: false);
        name.HorizontalAlignment = HorizontalAlignment.Center;
        name.Margin = new Thickness(0, 12, 0, 0);
        head.Children.Add(name);
        var version = Ui.Text($"Version {AnnexNative.Version}", "BodyTextBlockStyle", Ui.Secondary, wrap: false);
        version.HorizontalAlignment = HorizontalAlignment.Center;
        head.Children.Add(version);
        var description = Ui.Text("Use another computer as a second display, or mirror its screen.\nMac and Windows, over your network or a cable, encrypted end to end.",
            "BodyTextBlockStyle", Ui.Secondary);
        description.TextAlignment = TextAlignment.Center;
        description.HorizontalAlignment = HorizontalAlignment.Center;
        description.Margin = new Thickness(0, 12, 0, 0);
        head.Children.Add(description);
        var buttons = Ui.Row(Ui.Button("What's new", () => _ = ShowAllWhatsNewAsync()),
            LinkButton("Release notes", $"{RepoUrl}/releases/tag/v{AnnexNative.Version}"),
            LinkButton("Report an issue", $"{RepoUrl}/issues"),
            new HyperlinkButton { Content = "View on GitHub", NavigateUri = new Uri(RepoUrl) });
        buttons.HorizontalAlignment = HorizontalAlignment.Center;
        buttons.Margin = new Thickness(0, 16, 0, 0);
        head.Children.Add(buttons);
        var copyright = Ui.Text("MIT License \u00b7 \u00a9 2026 Nalinish Ranjan and ", "CaptionTextBlockStyle", Ui.Tertiary, wrap: false);
        var link = new Hyperlink { NavigateUri = new Uri("https://saidev.codes") };
        link.Inlines.Add(new Run { Text = "Saidev Dhal" });
        copyright.Inlines.Add(link);
        copyright.HorizontalAlignment = HorizontalAlignment.Center;
        copyright.Margin = new Thickness(0, 14, 0, 0);
        head.Children.Add(copyright);
        p.Children.Add(Ui.Group(head));

        p.Children.Add(Ui.Section("Updates"));
        var checkNow = Ui.Button("Check now", () =>
        {
            AnnexLog.Info("[update] Check now pressed");
            _ = _updater.CheckAsync(_settings.BetaUpdates);
        });
        checkNow.IsEnabled = !_updater.Busy;
        checkNow.HorizontalAlignment = HorizontalAlignment.Left;
        var last = _updater.LastChecked is { } when
            ? $"Last checked: {when.LocalDateTime:g}"
            : "Not checked yet.";
        p.Children.Add(Ui.Group(
            Ui.GroupRow("Check for updates automatically", "Once a day, from GitHub. Nothing about you is sent.",
                Ui.Switch(_settings.CheckForUpdates, on => { Setting("check for updates", on); _settings.CheckForUpdates = on; _settings.Save(); })),
            Ui.GroupRow("Receive beta updates", "Beta versions include features in development and may contain bugs.",
                Ui.Switch(_settings.BetaUpdates, on =>
                {
                    Setting("beta updates", on);
                    _settings.BetaUpdates = on;
                    _settings.BetaUpdatesChosen = true;
                    _settings.Save();
                    _ = _updater.BetaChangedAsync(on);
                })),
            UpdateStatusRow(),
            checkNow,
            Ui.Text(last, "CaptionTextBlockStyle", Ui.Tertiary)));
    }

    /// <summary>The update status line: up to date, checking, failed, or an update to install.</summary>
    private Grid UpdateStatusRow()
    {
        static FontIcon Mark(string glyph, string brush) => new() { Glyph = glyph, FontSize = 18, Foreground = Ui.Res<Brush>(brush) };
        static ProgressRing Spin() => new() { IsActive = true, Width = 18, Height = 18 };
        return _updater.State switch
        {
            Updater.Stage.Checking => Ui.GroupRow("Checking for updates…", leading: Spin()),
            Updater.Stage.Downloading => Ui.GroupRow("Downloading the update…", leading: Spin()),
            Updater.Stage.Installing => Ui.GroupRow("Installing… Annex opens again in a moment.", leading: Spin()),
            Updater.Stage.Available => Ui.GroupRow(UpdateText(), "Annex closes and opens again when it's done.", UpdateButtons(),
                Mark("\uE896", "AccentFillColorDefaultBrush")),
            Updater.Stage.Failed => Ui.GroupRow(_updater.Error ?? "Couldn't check for updates.", leading: Mark("\uE7BA", "SystemFillColorCautionBrush")),
            Updater.Stage.UpToDate => Ui.GroupRow("You're on the latest version.", leading: Mark("\uEC61", "SystemFillColorSuccessBrush")),
            _ => Ui.GroupRow(_updater.LastChecked is null ? "Not checked yet." : "You're on the latest version.",
                leading: Mark(_updater.LastChecked is null ? "\uE946" : "\uEC61",
                    _updater.LastChecked is null ? "TextFillColorSecondaryBrush" : "SystemFillColorSuccessBrush")),
        };
    }

    private void DeveloperSection(StackPanel p)
    {
        p.Children.Add(Ui.Card("\uEC7A", "Show technical details", "Identity codes, addresses and connection details.",
            Ui.Switch(_settings.ShowTechnicalDetails, on => { Setting("show technical details", on); _settings.ShowTechnicalDetails = on; _settings.Save(); Render(); })));
        p.Children.Add(Ui.Card("", "Log file", "Always on. Stays on this PC; Annex never sends it anywhere. Attach it to a bug report if something goes wrong.",
            Ui.Button("Show log files", ShowLogFiles)));
        p.Children.Add(Ui.Card("\uEBE8", "Verbose logging", "Also logs every input event and transfer step. Turn it on to chase a problem; the log fills faster.",
            Ui.Switch(_settings.VerboseLogging, on =>
            {
                Setting("verbose logging", on);
                _settings.VerboseLogging = on;
                _settings.Save();
                AnnexLog.SetVerbose(on);
            })));
    }

    /// <summary>Opens the folder holding annex.log (and the crashes folder: what led up to each error).</summary>
    private static void ShowLogFiles()
    {
        AnnexLog.Info("[app] showing the log files");
        try
        {
            System.Diagnostics.Process.Start("explorer.exe", $"\"{DataDirectory}\"");
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            AnnexLog.Warn($"[app] couldn't open the data folder: {e.Message}");
        }
    }

    // ---- Updates ----------------------------------------------------------------------

    private void StartUpdateChecks()
    {
        _updater.LastChecked = _settings.LastUpdateCheck;
        _updater.Changed += () => _ui.TryEnqueue(() =>
        {
            if (_updater.LastChecked != _settings.LastUpdateCheck)
            {
                _settings.LastUpdateCheck = _updater.LastChecked;
                _settings.Save();
            }
            RenderIf("about", "share", "connect");
        });
        // Shortly after launch, then once a day while Annex runs.
        var timer = _ui.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(10);
        timer.Tick += (_, _) =>
        {
            timer.Interval = TimeSpan.FromHours(24);
            if (_settings.CheckForUpdates) _ = _updater.CheckAsync(_settings.BetaUpdates, automatic: true);
        };
        timer.Start();
    }

    private string UpdateText() => _updater.Available is { } u
        ? $"Annex {u.Version} is available." + (u.Downgrade ? " It's the latest normal release." : u.Prerelease ? " It's a pre-release." : "")
        : "";


    private StackPanel UpdateButtons()
    {
        var install = Ui.Button("Install", () => _ = InstallUpdateAsync(), accent: true);
        if (_updater.Available is not { } u) return Ui.Row(install);
        return Ui.Row(new HyperlinkButton { Content = "What's new", NavigateUri = new Uri(u.PageUrl) }, install);
    }

    /// <summary>Top of Share and Connect: an automatic check found an update.</summary>
    private void UpdateBanner(StackPanel p)
    {
        if (_updater.State != Updater.Stage.Available || _updater.Available is not { } u || u.Version == _settings.DismissedUpdate) return;
        var notNow = Ui.Button("Not now", () =>
        {
            AnnexLog.Info($"[update] Not now for {u.Version}");
            _settings.DismissedUpdate = u.Version;
            _settings.Save();
            RenderIf("share", "connect");
        });
        p.Children.Add(Ui.Card("\uE777", UpdateText(), "Annex closes and opens again when it's done.",
            Ui.Row(notNow, Ui.Button("Install", () => _ = InstallUpdateAsync(), accent: true))));
    }

    private async Task ShowAllWhatsNewAsync()
    {
        AnnexLog.Info("[update] What's new pressed");
        IReadOnlyList<WhatsNewRelease> notes;
        try { notes = AnnexNative.WhatsNewAll(); }
        catch (Exception e) when (e is System.Text.Json.JsonException or DllNotFoundException or EntryPointNotFoundException or BadImageFormatException)
        {
            AnnexLog.Warn($"[update] couldn't read what's new: {e.Message}");
            return;
        }
        await ShowWhatsNewAsync(notes);
    }

    private async Task InstallUpdateAsync()
    {
        AnnexLog.Info($"[update] Install pressed ({_updater.Available?.Version})");
        if (_viewer is not null || _sessions.Count > 0)
        {
            var dialog = new ContentDialog
            {
                XamlRoot = Content.XamlRoot,
                Title = "Install the update now?",
                Content = "Annex closes to update, which ends the current session.",
                PrimaryButtonText = "Install",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            try
            {
                if (await Ui.PointingHand(dialog).ShowAsync() != ContentDialogResult.Primary)
                {
                    AnnexLog.Info("[update] install cancelled: a session is running");
                    return;
                }
            }
            catch (Exception e)
            {
                AnnexLog.Warn($"[update] couldn't ask before installing: {e.Message}");
                return; // another dialog is open
            }
        }
        if (await _updater.InstallAsync(_settings.BetaUpdates))
        {
            // Setup is running: get out of its way (it starts Annex again when done).
            Quit();
        }
    }

    // ---- Helpers ----------------------------------------------------------------------

    private string Fingerprint => _core is null ? "" : AnnexNative.DisplayFingerprint(_core.Fingerprint);

    private bool IsPaired(PeerInfo peer) => peer.Fingerprint is { } fp && _trusted.Any(t => t.Fingerprint == fp);

    private static string LastSeen(TrustedPeer peer)
    {
        var parts = new List<string>();
        if (peer.LastSeen > 0)
        {
            var ago = DateTimeOffset.UtcNow - DateTimeOffset.FromUnixTimeSeconds((long)peer.LastSeen);
            parts.Add("Last connected " + (ago.TotalMinutes < 1 ? "just now"
                : ago.TotalHours < 1 ? $"{(int)ago.TotalMinutes} min ago"
                : ago.TotalDays < 1 ? $"{(int)ago.TotalHours} h ago"
                : $"{(int)ago.TotalDays} days ago"));
        }
        if (peer.AllowInput) parts.Add("can control this PC");
        return parts.Count == 0 ? "Paired" : string.Join(" · ", parts);
    }

    /// <summary>IPv4 addresses of interfaces that are up; LAN first, direct-cable (169.254) last.</summary>
    private static List<(string Ip, bool Direct)> LocalAddresses()
    {
        var list = new List<(string, bool)>();
        try
        {
            foreach (var nic in NetworkInterface.GetAllNetworkInterfaces())
            {
                if (nic.OperationalStatus != OperationalStatus.Up
                    || nic.NetworkInterfaceType is NetworkInterfaceType.Loopback or NetworkInterfaceType.Tunnel)
                    continue;
                foreach (var a in nic.GetIPProperties().UnicastAddresses)
                {
                    if (a.Address.AddressFamily != AddressFamily.InterNetwork) continue;
                    var ip = a.Address.ToString();
                    list.Add((ip, ip.StartsWith("169.254.", StringComparison.Ordinal)));
                }
            }
        }
        catch (NetworkInformationException)
        {
        }
        return [.. list.Distinct().OrderBy(x => x.Item2)];
    }

    private async Task ConfirmRemove(TrustedPeer peer)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Remove {peer.Name}?",
            Content = "It will need a PIN to connect again.",
            PrimaryButtonText = "Remove",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await Ui.PointingHand(dialog).ShowAsync() == ContentDialogResult.Primary)
        {
            AnnexLog.Info($"[pairing] forgot {peer.Name} ({peer.Fingerprint})");
            _core?.Untrust(peer.Fingerprint);
            RefreshTrusted();
        }
    }

    private void Rename(string name)
    {
        if (_core is null) return;
        var trimmed = name.Trim();
        if (trimmed == (_settings.DeviceName ?? "")) return;
        var stored = _core.SetDeviceName(trimmed.Length == 0 ? Environment.MachineName : trimmed);
        Setting("device name", trimmed.Length == 0 ? $"this PC's ({stored})" : $"\"{stored}\"");
        _settings.DeviceName = trimmed.Length == 0 ? null : stored;
        _settings.Save();
        _core.SetPresence(true);
        _nameBox.Text = _settings.DeviceName ?? "";
        // Discovery advertises the name: re-advertise if sharing and nobody is connected.
        if (_host is not null && _sessions.Count == 0)
        {
            StopHosting();
            StartHosting();
        }
        UpdateDeviceCard();
    }

    private TrayMenu? _trayMenu;

    private void ShowTrayMenu()
    {
        _trayMenu ??= new TrayMenu();
        var status = Role()?.Headline ?? (_viewer is not null ? _viewerStatus ?? "Connecting…"
            : _host is null ? "Not sharing" : "Sharing");
        _trayMenu.ShowAtCursor(
            new TrayMenu.Model(_core?.DeviceName ?? "Annex", status, _host is not null,
                _viewer is not null ? $"Viewing {_viewingName}" : null, _hosts, TrayLive(), Label),
            new TrayMenu.Actions(
                SetSharing: on => { AnnexLog.Info($"[tray] Share this PC {(on ? "on" : "off")}"); SetHosting(on); },
                Connect: (peer, extend) => { AnnexLog.Info($"[tray] {(extend ? "Extend" : "View")} {peer.Name}"); _extend = extend; ConnectToPeer(peer); },
                Disconnect: () => { AnnexLog.Info("[tray] Disconnect"); Disconnect(); },
                ConnectOther: () => { AnnexLog.Info("[tray] Connect by code or address"); ShowFromTray(); Go("home"); _ = ShowConnectDialogAsync(); },
                Open: () => { AnnexLog.Info("[tray] Open Annex"); ShowFromTray(); },
                OpenSettings: () => { AnnexLog.Info("[tray] Settings"); ShowFromTray(); Go("settings"); },
                Quit: () => { AnnexLog.Info("[tray] Quit"); Quit(); }));
    }

    /// <summary>The tray menu's live block: the same title as Nearby's session bar, and its numbers.</summary>
    private (string Title, (string From, string To, bool ThisIsFrom)? Flow, string? Detail)? TrayLive()
    {
        var role = Role();
        var flow = role is { } r ? (r.From, r.To, r.ThisIsFrom) : ((string, string, bool)?)null;
        if (_viewer is not null)
            return (role?.Headline ?? _viewerStatus ?? "Connecting…", flow,
                _lat is { } l ? $"{l.P50Ms:F0} ms delay · {l.Fps:F0} fps" : null);
        if (_sessions.Count == 0) return null;
        var s = _sessions.Values.First();
        return (role!.Value.Headline, flow,
            s.Input ? $"{Label(s.Name)} can use your mouse and keyboard" : "View only: it can't use your mouse or keyboard");
    }

    private void ShowFromTray()
    {
        if (!AppWindow.IsVisible) AnnexLog.Debug("[app] window shown");
        AppWindow.Show();
        Activate();
    }

    private void Quit()
    {
        AnnexLog.Info("[app] quitting");
        _quitting = true;
        Close();
        Application.Current.Exit();
    }

    // ---- Hosting ----------------------------------------------------------------------

    private void RefreshMonitors()
    {
        try
        {
            _monitors = AnnexNative.Monitors();
        }
        catch (Exception ex)
        {
            AnnexLog.Error($"[display] can't list displays: {ex.GetType().Name}: {ex.Message}");
            _hostError = $"Can't list displays: {ex.Message}";
            _monitors = [];
        }
        _monitorIndex = Math.Clamp(_monitorIndex, 0, Math.Max(0, _monitors.Count - 1));
    }

    private void SetHosting(bool on)
    {
        if (on) StartHosting(); else StopHosting();
        RenderIf("share");
    }

    private void StartHosting()
    {
        if (_core is null || _host is not null) return;
        // A host that was just stopped may still hold the port.
        _teardown.Wait();
        _hostError = null;
        var hardware = _settings.PreferHardwareEncoder;
        var screen = _monitorIndex < _monitors.Count ? _monitors[_monitorIndex].Label : $"#{_monitorIndex}";
        AnnexLog.Info($"[host] starting: screen {screen}, {(hardware ? "hardware" : "software")} encoder, gpu {(_settings.GpuLuid == 0 ? "automatic" : _settings.GpuLuid.ToString("x"))}, "
            + $"port {DefaultPort}, no one at screen {_settings.Headless}, receive files {_settings.ReceiveFiles}");
        try
        {
            try
            {
                _host = HostHandle.StartScreen(_core, _monitorIndex, DefaultPort, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)), _settings.GpuLuid);
            }
            catch (AnnexException e)
            {
                // Default port busy: take any free port (discovery advertises it).
                AnnexLog.Warn($"[host] port {DefaultPort} didn't work ({e.Message}), taking any free port");
                _host = HostHandle.StartScreen(_core, _monitorIndex, 0, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)), _settings.GpuLuid);
            }
            AnnexLog.Info($"[host] sharing on port {_host?.Port}");
            if (_tray is not null) _tray.Tooltip = "Annex — sharing this screen";
        }
        catch (AnnexException ex)
        {
            AnnexLog.Error($"[host] couldn't start sharing: {ex.Message}");
            _hostError = $"Couldn't start sharing: {ex.Message}";
        }
    }

    private void StopHosting()
    {
        var h = _host;
        if (h is not null) AnnexLog.Info($"[host] stopping sharing ({_sessions.Count} session(s) open)");
        _host = null;
        FreeLater(h);
        _sessions.Clear();
        _hostStream = null;
        _internet = null;
        _ = _hotspot.StopAsync();
        if (_tray is not null) _tray.Tooltip = "Annex";
    }

    private async void OnHostEvent(CoreEvent e)
    {
        UpdateDropTargetSoon();
        switch (e)
        {
            case CoreEvent.ClipboardReceived c:
                if (_settings.ShareClipboard) _clipboard.Apply(c.Mime, c.Data);
                else AnnexLog.Debug($"[clipboard] received {c.Mime}, {c.Data.Length} bytes: not applied, Share the clipboard is off");
                return;
            case CoreEvent.TransferStarted or CoreEvent.TransferProgress or CoreEvent.TransferEnded:
                OnTransfer(e, viewer: false);
                return;
            case CoreEvent.PairingPin pin:
                AnnexLog.Info($"[pairing] showing a PIN for {pin.PeerName}");
                await ShowPin(pin);
                break;
            case CoreEvent.PairingFinished f when f.RequestId == _pinRequest:
                _pinDialog?.Hide();
                if (!f.Success) _hostError = $"Pairing with {f.PeerName} failed.";
                RefreshTrusted();
                break;
            case CoreEvent.SessionStarted s:
                _sessions[s.SessionId] = (s.PeerName, s.Mode, s.Input, s.Link == "relayed");
                if (s.Mode == "mirror") _hostStream = s.Stream;
                _hostError = null;
                RefreshTrusted();
                break;
            case CoreEvent.InputAllowed a when _sessions.TryGetValue(a.SessionId, out var s):
                _sessions[a.SessionId] = s with { Input = a.Allowed };
                RenderIf("share");
                break;
            case CoreEvent.SessionEnded ended:
                _sessions.Remove(ended.SessionId);
                RenderIf("share");
                break;
            case CoreEvent.InputRequested ask when _core is not null:
            {
                AnnexLog.Info($"[pairing] asking whether {ask.PeerName} may use the mouse and keyboard");
                ShowFromTray();
                var dialog = new ContentDialog
                {
                    XamlRoot = Content.XamlRoot,
                    Title = $"Let {ask.PeerName} control this PC?",
                    Content = $"{ask.PeerName} wants to use this PC's mouse and keyboard. You can change this in Paired devices.",
                    PrimaryButtonText = "Allow",
                    CloseButtonText = "Not now",
                    DefaultButton = ContentDialogButton.Primary,
                };
                try
                {
                    var allow = await Ui.PointingHand(dialog).ShowAsync() == ContentDialogResult.Primary;
                    AnnexLog.Info($"[pairing] {ask.PeerName} may use the mouse and keyboard: {(allow ? "allowed" : "not now")}");
                    if (allow)
                    {
                        _core.SetAllowInput(ask.Fingerprint, true);
                        RefreshTrusted();
                    }
                }
                catch (Exception ex)
                {
                    // Another dialog is open; the switch in Paired devices still works.
                    AnnexLog.Warn($"[pairing] couldn't ask about control for {ask.PeerName}: {ex.Message}");
                }
                break;
            }
            case CoreEvent.DirectLinkRequested req when _host is not null:
            {
                // The viewer (a Mac) asks for a direct link: run the hotspot here and hand it over.
                AnnexLog.Info("[host] the viewer asked for direct Wi-Fi: starting the hotspot");
                var error = await _hotspot.StartAsync();
                if (error is null) _host?.OfferDirectLink(req.SessionId, _hotspot.Ssid!, _hotspot.Passphrase!);
                else _host?.RefuseDirectLink(req.SessionId, error);
                AnnexLog.Info(error is null ? $"[host] offered \"{_hotspot.Ssid}\" to the viewer" : $"[host] refused direct Wi-Fi: {error}");
                RenderIf("share");
                break;
            }
            case CoreEvent.InternetCode or CoreEvent.InternetJoined or CoreEvent.InternetRelay or CoreEvent.InternetCodeEnded:
                OnInternetEvent(e);
                break;
            case CoreEvent.DirectLinkOffer offer when _host is not null:
                // Another Windows PC offers its hotspot; joining one from here isn't supported yet.
                AnnexLog.Info("[host] refused a direct Wi-Fi offer: joining one from Windows isn't supported");
                _host.RefuseDirectLink(offer.SessionId, "Joining a direct Wi-Fi link isn't supported on Windows yet. Run the switch from the other computer.");
                break;
        }
    }

    private async Task ShowPin(CoreEvent.PairingPin pin)
    {
        if (_core is null) return;
        ShowFromTray();
        _pinRequest = pin.RequestId;
        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(Ui.Text($"Enter this PIN on {pin.PeerName}:"));
        var code = Ui.Text(pin.Pin, "TitleLargeTextBlockStyle", wrap: false);
        code.FontFamily = new FontFamily("Consolas");
        code.HorizontalAlignment = HorizontalAlignment.Center;
        code.IsTextSelectionEnabled = true;
        panel.Children.Add(code);
        if (ShowTech)
            panel.Children.Add(Ui.Text($"Its identity code: {pin.FingerprintDisplay}", "CaptionTextBlockStyle", Ui.Secondary));
        _pinDialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"“{pin.PeerName}” wants to connect",
            Content = panel,
            CloseButtonText = "Don't allow",
        };
        try
        {
            var result = await Ui.PointingHand(_pinDialog).ShowAsync();
            // Closed by the user (not by pairing finishing): refuse the pairing.
            if (result == ContentDialogResult.None && _pinRequest == pin.RequestId && _host is not null)
            {
                AnnexLog.Info($"[pairing] Don't allow pressed for {pin.PeerName}");
                _core.CancelRequest(pin.RequestId);
            }
        }
        catch (Exception ex)
        {
            AnnexLog.Warn($"[pairing] couldn't show the PIN for {pin.PeerName} (another dialog is open?): {ex.Message}");
            // Another dialog is already open; the viewer side will time out.
            _hostError = $"PIN {pin.Pin} for {pin.PeerName} ({ex.Message})";
            RenderIf("share");
        }
        finally
        {
            _pinDialog = null;
        }
    }

    // ---- Viewing ----------------------------------------------------------------------

    private void OnDiscovery(CoreEvent e)
    {
        switch (e)
        {
            case CoreEvent.PeerFound(var peer):
                _hosts.RemoveAll(h => h.Instance == peer.Instance);
                _hosts.Add(peer);
                _hosts.Sort((a, b) => string.Compare(a.Name, b.Name, StringComparison.CurrentCultureIgnoreCase));
                TryDirectReconnect(peer);
                break;
            case CoreEvent.PeerLost(var instance):
                _hosts.RemoveAll(h => h.Instance == instance);
                if (_selected == instance) _selected = null;
                break;
            default:
                return;
        }
        BuildNav();
        RenderIf("connect", "peer");
    }

    private void ConnectToPeer(PeerInfo peer)
    {
        if (peer.ConnectAddress is not { } address)
        {
            AnnexLog.Warn($"[viewer] {peer.Name} has no address to connect to");
            return;
        }
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        var pinned = peer.Fingerprint is { } fp && _trusted.Any(t => t.Fingerprint == fp) ? fp : null;
        Connect(address, peer.Name, pinned);
    }

    private void ConnectManual()
    {
        var address = _addressBox.Text.Trim();
        if (address.Length == 0) return;
        _connectDialog?.Hide();
        Connect(address, address, null);
    }

    /// <summary>Viewer: connect by a code from a computer sharing over the internet.</summary>
    private void ConnectByCode()
    {
        var code = _codeBox.Text.Trim();
        if (_core is null || code.Length == 0) return;
        _connectDialog?.Hide();
        AnnexLog.Info($"[viewer] connecting by code ({(_settings.RendezvousServer.Trim().Length == 0 ? "Annex's server" : _settings.Server)}), "
            + $"{(_extend ? "extend" : "mirror")}, control {_settings.UseMouseAndKeyboard}, quality {_settings.Quality}");
        Disconnect();
        var token = new object();
        _viewerToken = token;
        try
        {
            _viewer = ViewerHandle.ConnectCodeWindowed(_core, _settings.Server, code,
                e => _ui.TryEnqueue(() => { if (ReferenceEquals(_viewerToken, token)) OnViewerEvent(e); }),
                extend: _extend, input: _settings.UseMouseAndKeyboard, quality: _settings.Quality);
            _viewingName = "the other computer";
            _viewingByCode = true;
            _viewerStatus = $"Connecting with code {code.ToUpperInvariant()}…";
            if (_tray is not null) _tray.Tooltip = "Annex — connecting";
        }
        catch (AnnexException ex)
        {
            AnnexLog.Error($"[viewer] couldn't connect by code: {ex.Message}");
            _viewerStatus = $"Couldn't connect: {ex.Message}";
        }
        RenderIf("connect", "peer");
    }

    private void Connect(string address, string title, string? pinnedFingerprint)
    {
        if (_core is null) return;
        AnnexLog.Info($"[viewer] connecting to {title} at {address}: {(_extend ? "extend" : "mirror")}, control {_settings.UseMouseAndKeyboard}, "
            + $"quality {_settings.Quality}, {(pinnedFingerprint is null ? "new (PIN if asked)" : "paired")}");
        Disconnect();
        var token = new object();
        _viewerToken = token;
        try
        {
            _viewer = ViewerHandle.ConnectWindowed(_core, address, pinnedFingerprint,
                e => _ui.TryEnqueue(() => { if (ReferenceEquals(_viewerToken, token)) OnViewerEvent(e); }),
                extend: _extend, input: _settings.UseMouseAndKeyboard, quality: _settings.Quality);
            _viewingName = title;
            _viewingByCode = false;
            _viewerStatus = $"Connecting to {title}…";
            if (_tray is not null) _tray.Tooltip = $"Annex — connecting to {title}";
        }
        catch (AnnexException ex)
        {
            AnnexLog.Error($"[viewer] couldn't connect to {title}: {ex.Message}");
            _viewerStatus = $"Couldn't connect: {ex.Message}";
        }
        RenderIf("connect", "peer");
    }

    private async void OnViewerEvent(CoreEvent e)
    {
        UpdateDropTargetSoon();
        switch (e)
        {
            case CoreEvent.ClipboardReceived c:
                if (_settings.ShareClipboard) _clipboard.Apply(c.Mime, c.Data);
                else AnnexLog.Debug($"[clipboard] received {c.Mime}, {c.Data.Length} bytes: not applied, Share the clipboard is off");
                return;
            case CoreEvent.TransferStarted or CoreEvent.TransferProgress or CoreEvent.TransferEnded:
                OnTransfer(e, viewer: true);
                return;
            case CoreEvent.PairingPinRequired req:
                await PromptForPin(req);
                return;
            case CoreEvent.PairingFinished { Success: false } f:
                _viewerStatus = $"Pairing with {f.PeerName} didn't finish. Check the PIN and try again.";
                break;
            case CoreEvent.SessionStarted s:
                _viewerStatus = (s.Mode == "extend" ? $"Extra display from {Label(s.PeerName)}" : $"Viewing {Label(s.PeerName)}")
                    + s.Link switch { "direct" => " · cable", "internet" => " · internet", "relayed" => " · relay", _ => "" };
                _viewingName = s.PeerName;
                _viewerFingerprint = s.Fingerprint;
                _viewerExtend = s.Mode == "extend";
                _viewerLive = true;
                _controlling = s.Input;
                if (_tray is not null) _tray.Tooltip = $"Annex — viewing {s.PeerName}";
                RefreshTrusted();
                if (_settings.ConnectOverDirectWifi && !_onDirectWifi && _directPending is null)
                {
                    AnnexLog.Info("[viewer] Direct Wi-Fi first is on: switching");
                    _ = SwitchToDirectWifiAsync();
                }
                break;
            case CoreEvent.DirectLinkRequested:
                // The host (e.g. a Mac, which can't run a hotspot) asks this PC to set up the link.
                if (_directPending is null && !_onDirectWifi) _ = SwitchToDirectWifiAsync();
                else AnnexLog.Debug("[viewer] direct Wi-Fi asked for: already on it or switching");
                break;
            case CoreEvent.InputAllowed a:
                _controlling = a.Allowed;
                break;
            case CoreEvent.DirectLinkUnavailable u:
                _directPending = null;
                _directStatus = u.Reason;
                _ = _hotspot.StopAsync();
                break;
            case CoreEvent.StreamChanged:
                // The title from SessionStarted keeps how it's reached (" · relay"); the size is a detail.
                return;
            case CoreEvent.Latency(var l):
                _lat = l;
                break;
            case CoreEvent.SessionEnded(_, var reason):
                // Switching to direct Wi-Fi drops the old connection on purpose.
                _viewerStatus = _directPending is not null ? null : reason == "user" ? null : FriendlyReason(reason, _viewingName, _viewingByCode);
                if (_directPending is not null) AnnexLog.Info("[viewer] session ended for the direct Wi-Fi switch, waiting to reconnect");
                if (reason == "user" && _onDirectWifi)
                {
                    AnnexLog.Info("[viewer] left a direct Wi-Fi session: turning the hotspot off");
                    _onDirectWifi = false;
                    _ = _hotspot.StopAsync();
                }
                _lat = null;
                _controlling = false;
                if (_tray is not null) _tray.Tooltip = "Annex";
                Disconnect();
                // Switching: if the host already shows up on the hotspot, reconnect now.
                if (_directPending is { } pending && _hosts.FirstOrDefault(h => h.Fingerprint == pending.Fingerprint) is { } back)
                    TryDirectReconnect(back);
                break;
            default:
                return;
        }
        RenderIf("connect", "peer");
    }

    private async Task PromptForPin(CoreEvent.PairingPinRequired req)
    {
        if (_core is null) return;
        AnnexLog.Info($"[pairing] asking for the PIN shown on {req.PeerName}");
        ShowFromTray(); // the dialog needs a visible window
        var box = new TextBox
        {
            PlaceholderText = "******",
            MaxLength = 7,
            FontSize = 24,
            TextAlignment = TextAlignment.Center,
            InputScope = new InputScope { Names = { new InputScopeName(InputScopeNameValue.Number) } },
        };
        var panel = new StackPanel { Spacing = 12 };
        panel.Children.Add(Ui.Text($"Enter the 6-digit PIN shown on {req.PeerName}."));
        panel.Children.Add(box);
        if (ShowTech)
            panel.Children.Add(Ui.Text($"Its identity code: {req.FingerprintDisplay}", "CaptionTextBlockStyle", Ui.Secondary));
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Pair with “{req.PeerName}”",
            Content = panel,
            PrimaryButtonText = "Pair",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        ContentDialogResult result;
        try
        {
            result = await Ui.PointingHand(dialog).ShowAsync();
        }
        catch (Exception e)
        {
            // Another dialog is already open: refuse the pairing rather than crash (it can be retried).
            AnnexLog.Warn($"[pairing] couldn't ask for the PIN (another dialog is open?): {e.Message}");
            result = ContentDialogResult.None;
        }
        // The PIN itself is never logged.
        AnnexLog.Info(result == ContentDialogResult.Primary ? $"[pairing] PIN entered for {req.PeerName}" : $"[pairing] pairing with {req.PeerName} cancelled");
        if (result == ContentDialogResult.Primary)
            _core.SubmitPin(req.RequestId, box.Text);
        else
            _core.CancelRequest(req.RequestId);
        RefreshTrusted();
    }

    private void RefreshTrusted()
    {
        if (_core is null) return;
        _trusted = [.. _core.TrustedPeers];
        RenderIf("devices", "connect", "peer", "share");
    }

    private void Disconnect()
    {
        var v = _viewer;
        if (v is not null) AnnexLog.Info($"[viewer] closing the viewer for {_viewingName}");
        _viewer = null;
        _viewerLive = false;
        _viewerToken = null;
        _copiedWhileAway = false;
        FreeLater(v);
    }

    /// <summary>Frees a host or viewer off the UI thread: it says goodbye and waits for the connection
    /// to close (half a second or more), which froze the window. In order, so a new host waits for
    /// the old one to let go of the port.</summary>
    private void FreeLater(IDisposable? handle)
    {
        if (handle is null) return;
        _teardown = _teardown.ContinueWith(_ =>
        {
            try { handle.Dispose(); }
            catch (Exception e)
            {
                AnnexLog.Error($"[app] closing a {handle.GetType().Name} failed: {e}");
            }
        }, TaskScheduler.Default);
    }

    private void Shutdown()
    {
        AnnexLog.Info("[app] shutting down");
        _tray?.Dispose();
        _tray = null;
        _trayMenu?.Close();
        _trayMenu = null;
        StopHosting();
        Disconnect();
        if (!_teardown.Wait(TimeSpan.FromSeconds(5))) AnnexLog.Warn("[app] sessions took over 5 s to close; quitting anyway");
        if (!_hotspot.StopAsync().Wait(TimeSpan.FromSeconds(3))) AnnexLog.Warn("[network] the hotspot took over 3 s to stop; quitting anyway");
        _discovery?.Dispose();
        _discovery = null;
        AnnexLog.Info("[app] bye");
        _core?.Dispose();
        _core = null;
    }
}

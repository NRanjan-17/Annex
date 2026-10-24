using System.Net.NetworkInformation;
using System.Net.Sockets;
using Annex.Interop;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
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
    private IReadOnlyList<MonitorInfo> _monitors = [];
    private int _monitorIndex;
    private ContentDialog? _pinDialog;
    private ulong _pinRequest;
    private TrayIcon? _tray;
    private bool _quitting;
    private string? _startupError;

    // Host state.
    private readonly Dictionary<ulong, (string Name, string Mode, bool Input)> _sessions = [];
    private StreamConfig? _hostStream;
    private string? _hostError;
    private ExtendStatus? _extendStatus;
    /// <summary>null = not checked yet.</summary>
    private bool? _firewallAllowed;

    // Viewer state.
    private bool _extend;
    private string _viewingName = "";
    private string? _viewerStatus;
    private LatencyStats? _lat;
    private bool _controlling;
    private string? _viewerFingerprint;
    private bool _viewerExtend;
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
    /// <summary>Nearby is being re-checked (the refresh button shows progress until then).</summary>
    private DateTimeOffset _refreshingUntil;

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
        };
        _tray = new TrayIcon("Annex", Ui.AssetPath("Annex.ico"));
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

        try
        {
            _core = CoreClient.Open(DataDirectory, _settings.DeviceName ?? Environment.MachineName);
            AnnexNative.SetLogEnabled(_settings.SaveLog);
            _discovery = new DiscoveryHandle(_core, e => _ui.TryEnqueue(() => OnDiscovery(e)));
            _trusted = [.. _core.TrustedPeers];
            RefreshMonitors();
            // Show up under Nearby on other computers as soon as Annex is open.
            _core.SetPresence(true);
            _extendStatus = TryExtendStatus();
            _ = CheckFirewallAsync();
            StartUpdateChecks();
            AppSettings.RefreshStartWithWindows();
        }
        catch (Exception ex)
        {
            _startupError = ex.Message;
        }
        _nameBox.Text = _settings.DeviceName ?? "";
        _nameBox.PlaceholderText = Environment.MachineName;
        BuildShell();
        if (_settings.StartSharingAtLaunch) SetHosting(true);
        if (Environment.GetEnvironmentVariable("ANNEX_SELFTEST") == "1") _ui.TryEnqueue(SelfTest);
    }

    /// <summary>CI smoke test: opens every page twice (rebuilds reuse long-lived controls), then
    /// writes selftest-ok to the data folder. Any exception lands in crash.log instead.</summary>
    private void SelfTest()
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
        var x = TryExtendStatus();
        File.WriteAllText(Path.Combine(DataDirectory, "selftest-extend.json"),
            x is null ? "null" : $"{{\"state\":\"{x.State}\",\"problem\":{x.Problem},\"test_signing\":{(x.TestSigning ? "true" : "false")}}}");
        File.WriteAllText(Path.Combine(DataDirectory, "selftest-ok"), DateTimeOffset.Now.ToString("O"));
    }

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

    private const uint EsContinuous = 0x80000000, EsSystemRequired = 0x1, EsDisplayRequired = 0x2;
    private bool _keepingAwake;

    /// While a session runs, neither screen sleeps: the viewer gets no local input, and the host's
    /// screen (or its Annex display) is what the viewer is watching. Always called on the UI
    /// thread, which lives as long as the app, so the continuous state holds.
    private void UpdateKeepAwake()
    {
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
        p.Children.Add(Ui.Card("", "Share this PC", ShareStatus(), Ui.Switch(_host is not null, on => SetHosting(on))));
        if (_monitors.Count > 1)
        {
            var box = new ComboBox { ItemsSource = _monitors.Select(m => m.Label).ToList(), SelectedIndex = _monitorIndex, MinWidth = 220, IsEnabled = _host is null };
            box.SelectionChanged += (_, _) => _monitorIndex = Math.Max(0, box.SelectedIndex);
            p.Children.Add(Ui.Card("", "Screen to share", _host is null ? null : "Stop sharing to pick another screen.", box));
        }
        if (_hostError is not null)
            p.Children.Add(Ui.Info("Sharing problem", _hostError, InfoBarSeverity.Warning));
        ExtendCard(p);
        p.Children.Add(Ui.Card("", "Direct Wi-Fi",
            _hotspot.IsOn
                ? $"Network “{_hotspot.Ssid}”, password {_hotspot.Passphrase}. Other computers can join it for less lag."
                : "Runs a hotspot on this PC so another computer connects straight to it. Less lag.",
            Ui.Switch(_hotspot.IsOn, on => _ = SetHotspotAsync(on))));

        if (_host is null)
        {
            p.Children.Add(Ui.Section("How it works"));
            p.Children.Add(Ui.Card("", "1. Turn on sharing here."));
            p.Children.Add(Ui.Card("", "2. Open Annex on the other computer.", "This PC shows up under Nearby there."));
            p.Children.Add(Ui.Card("", "3. Pick Extend or Show its screen.", "The first time, type the PIN this PC shows."));
        }
        else
        {
            p.Children.Add(Ui.Section("Connect from another computer"));
            p.Children.Add(Ui.Card("", $"1. Open Annex on the other computer and choose “{_core?.DeviceName}”."));
            p.Children.Add(Ui.Card("", "2. Pick Show its screen or Extend display."));
            p.Children.Add(Ui.Card("", "3. Enter the PIN shown here (first time only).", ShowTech ? $"Identity code: {Fingerprint}" : null));
            var addresses = LocalAddresses();
            p.Children.Add(Ui.Card("", addresses.Count == 0 ? "This PC isn't on a network." : "Not listed there? Connect by address:",
                addresses.Count == 0 ? "Connect it to Wi-Fi, Ethernet or a cable." : null));
            foreach (var (ip, direct) in addresses)
            {
                var text = _host.Port == DefaultPort ? ip : $"{ip}:{_host.Port}";
                var copy = new Button { Content = "Copy" };
                copy.Click += (_, _) =>
                {
                    var data = new DataPackage();
                    data.SetText(text);
                    Clipboard.SetContent(data);
                    copy.Content = "Copied";
                };
                var card = Ui.Card(direct ? "" : "", text, direct ? "Cable" : "Network", copy);
                p.Children.Add(card);
            }
        }

        if (_sessions.Count > 0)
        {
            p.Children.Add(Ui.Section("Connected now"));
            foreach (var (id, s) in _sessions)
            {
                var title = s.Mode == "extend" ? $"{s.Name} is extending this display" : $"{s.Name} is showing this screen";
                var detail = s.Mode == "extend" ? "Arrange it in Settings → System → Display" :
                    ShowTech && _hostStream is { } st ? $"{st.Width}×{st.Height} at {st.Fps} fps" : null;
                if (s.Input) detail = (detail is null ? "" : detail + " · ") + "using your mouse and keyboard";
                p.Children.Add(Ui.Card("", title, detail, Ui.Button("Switch to direct Wi-Fi", () => _ = OfferDirectWifiToViewerAsync(id, s.Name))));
            }
        }
    }

    private static List<GpuInfo> TryGpus()
    {
        try { return [.. AnnexNative.Gpus()]; }
        catch (Exception e) when (e is AnnexException or EntryPointNotFoundException) { return []; }
    }

    private static ExtendStatus? TryExtendStatus()
    {
        try { return AnnexNative.ExtendStatus(); }
        catch (Exception e) when (e is AnnexException or EntryPointNotFoundException) { return null; }
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

    /// <summary>Refresh button for Nearby, or a small spinner while the list is being re-checked.</summary>
    private FrameworkElement RefreshControl()
    {
        var refreshing = DateTimeOffset.UtcNow < _refreshingUntil;
        var content = refreshing
            ? (UIElement)new ProgressRing { IsActive = true, Width = 14, Height = 14, Foreground = Ui.White(0.8) }
            : Ui.Glyph("\uE72C", 14);
        var b = Ui.Flat(content, Ui.White(0.1), Ui.White(0.16), Ui.White(1), RefreshNearby, radius: 18);
        b.Width = 40;
        b.Height = 36;
        b.Padding = new Thickness(0);
        b.HorizontalContentAlignment = HorizontalAlignment.Center;
        b.IsEnabled = !refreshing;
        ToolTipService.SetToolTip(b, "Look again");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, "Refresh Nearby");
        return b;
    }

    private void RefreshNearby()
    {
        _discovery?.Refresh();
        _refreshingUntil = DateTimeOffset.UtcNow.AddSeconds(3);
        BuildNav();
        RenderIf("connect");
        var timer = _ui.CreateTimer();
        timer.Interval = TimeSpan.FromSeconds(3.1);
        timer.IsRepeating = false;
        timer.Tick += (_, _) =>
        {
            BuildNav();
            RenderIf("connect");
        };
        timer.Start();
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
            if (proc is null) return false;
            await proc.WaitForExitAsync();
            return proc.ExitCode == 0;
        }
        catch (System.ComponentModel.Win32Exception)
        {
            return false; // UAC prompt declined
        }
    }

    private async Task CheckFirewallAsync()
    {
        var ok = await RunNetshAsync($"advfirewall firewall show rule name=\"{FirewallRule}\"", elevated: false);
        _ui.TryEnqueue(() =>
        {
            _firewallAllowed = ok;
            RenderIf("connect", "share");
        });
    }

    private async Task AllowThroughFirewallAsync()
    {
        var exe = Environment.ProcessPath ?? "";
        await RunNetshAsync($"advfirewall firewall add rule name=\"{FirewallRule}\" dir=in action=allow program=\"{exe}\" profile=private,domain enable=yes", elevated: true);
        await CheckFirewallAsync();
    }

    /// <summary>Session-end codes from the core, in words.</summary>
    private static string FriendlyReason(string reason, string peer) => reason switch
    {
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
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        // Header: a plain title; the two controls on its right.
        var header = new Grid { ColumnSpacing = 12 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var titles = new StackPanel { Spacing = 4 };
        titles.Children.Add(Ui.Title("Nearby"));
        titles.Children.Add(Ui.Text("Click a computer for options. Double-click to connect.", "BodyTextBlockStyle", Ui.Secondary));
        header.Children.Add(titles);
        var connectByAddress = Ui.Flat(Ui.Text("Connect by address…", wrap: false), Ui.White(0.1), Ui.White(0.16), Ui.White(1),
            () => _ = ShowConnectDialogAsync(), radius: 18);
        connectByAddress.Padding = new Thickness(16, 7, 16, 8);
        var actions = Ui.Row(RefreshControl(), connectByAddress);
        actions.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(actions, 1);
        header.Children.Add(actions);
        root.Children.Add(header);

        var notes = new StackPanel { Spacing = 8 };
        if (_startupError is not null)
            notes.Children.Add(Ui.Info("Annex couldn't start", _startupError, InfoBarSeverity.Error));
        UpdateBanner(notes);
        if (_directStatus is not null)
            notes.Children.Add(Ui.Card("", "Direct Wi-Fi", _directStatus));
        if (_viewer is null && _viewerStatus is not null)
            notes.Children.Add(Ui.Card("", _viewerStatus));
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
        left.Children.Add(SearchStatus(visible.Count));
        UIElement list;
        if (visible.Count == 0)
        {
            list = new ScrollViewer { Content = Waiting(), VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
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

        if (SessionStrip() is { } strip)
        {
            Grid.SetRow(strip, 3);
            root.Children.Add(strip);
        }
        return root;
    }

    /// <summary>One line over the list: Annex is always looking, so say so, and say where.</summary>
    private StackPanel SearchStatus(int found)
    {
        var ring = new ProgressRing { IsActive = true, Width = 12, Height = 12, Foreground = Ui.White(0.7) };
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
        var hint = Ui.Text("Open Annex on the other computer and turn on sharing.\nIt shows up here by itself.", "BodyTextBlockStyle", Ui.Secondary);
        hint.TextAlignment = TextAlignment.Center;
        hint.HorizontalAlignment = HorizontalAlignment.Center;
        panel.Children.Add(hint);
        var connect = Ui.Pill("Connect by address…", () => _ = ShowConnectDialogAsync(), prominent: false);
        connect.HorizontalAlignment = HorizontalAlignment.Center;
        connect.Margin = new Thickness(0, 6, 0, 0);
        panel.Children.Add(connect);

        var outer = new StackPanel { Spacing = 0 };
        outer.Children.Add(Ui.Panel(panel, radius: 18, fill: 0.03, padding: new Thickness(24, 28, 24, 28)));
        var help = new StackPanel { MaxWidth = 720 };
        NotShowingUp(help);
        Ui.JoinRows(help);
        outer.Children.Add(help);
        return outer;
    }

    /// <summary>How this PC would reach a computer, from the addresses it announced.</summary>
    internal static (string Label, string Glyph) LinkKind(PeerInfo h) =>
        h.Addresses.Any(a => a.StartsWith("169.254.", StringComparison.Ordinal) || a.StartsWith("fe80", StringComparison.OrdinalIgnoreCase))
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
        var name = Ui.Text(h.Name, wrap: false);
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
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, $"{h.Name}, {PeerStatus(h)}");
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
        var name = Ui.Text(peer.Name);
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
            _settings.ConnectOverDirectWifi, on => { _settings.ConnectOverDirectWifi = on; _settings.Save(); }));
        if (peer.Fingerprint is { } fp && _trusted.FirstOrDefault(t => t.Fingerprint == fp) is { } trusted)
        {
            stack.Children.Add(SmallSwitch("Can use this PC's mouse and keyboard", "When it shows this PC's screen.",
                trusted.AllowInput, on => { _core?.SetAllowInput(trusted.Fingerprint, on); RefreshTrusted(); }));
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
        var liveLabel = Ui.Text("LIVE", "CaptionTextBlockStyle", Ui.Live, wrap: false);
        liveLabel.FontWeight = Microsoft.UI.Text.FontWeights.Bold;
        liveLabel.CharacterSpacing = 140;
        var live = Ui.Row(Ui.LiveDot(8), liveLabel);
        head.Children.Add(live);
        var first = _sessions.Values.FirstOrDefault();
        var title = Ui.Text(_viewer is not null ? _viewerStatus ?? "Connected"
            : first.Mode == "extend" ? $"Extending to {first.Name}" : $"Showing this screen on {first.Name}", wrap: false);
        title.FontSize = 18;
        title.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        head.Children.Add(title);
        string subtitle = _viewer is not null
            ? _controlling ? "You're in control · Ctrl+Alt+Shift+Q to leave"
                : _onDirectWifi ? $"On direct Wi-Fi “{_hotspot.Ssid}”"
                : "Press Esc to leave the viewer · encrypted end to end"
            : first.Mode == "extend" ? "Arrange it in Settings → System → Display"
            : first.Input ? $"{first.Name} can use your mouse and keyboard" : "Encrypted end to end";
        head.Children.Add(Ui.Text(subtitle, "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        grid.Children.Add(head);

        var stats = new List<(string Value, string Unit, string Label)>();
        if (_viewer is not null && _lat is { } l)
        {
            stats.Add(($"{l.P50Ms:F0}", "ms", ShowTech ? $"Delay (p95 {l.P95Ms:F0} ms)" : "Delay"));
            stats.Add(($"{l.Fps:F0}", "fps", "Smoothness"));
        }
        else if (_viewer is null && _hostStream is { } st)
        {
            stats.Add(($"{st.Fps}", "fps", "Smoothness"));
            stats.Add(($"{st.Width}×{st.Height}", "", "Picture"));
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

    /// <summary>Connect by address: for a computer that isn't listed. Replaces the old Connect page.</summary>
    private async Task ShowConnectDialogAsync()
    {
        if (_connectDialog is not null) return;
        var panel = new StackPanel { Spacing = 14, MinWidth = 420 };
        panel.Children.Add(Ui.Text("For a computer that isn't listed: on another network, or over Tailscale or ZeroTier.",
            "BodyTextBlockStyle", Ui.Secondary));
        var box = Ui.Reuse(_addressBox);
        box.HorizontalAlignment = HorizontalAlignment.Stretch;
        panel.Children.Add(box);
        panel.Children.Add(SmallSwitch("Direct Wi-Fi first", "This PC runs a hotspot and the other computer joins it. Less lag.",
            _settings.ConnectOverDirectWifi, on => { _settings.ConnectOverDirectWifi = on; _settings.Save(); }));
        _connectDialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            RequestedTheme = ElementTheme.Dark,
            Title = "Connect by address",
            Content = panel,
            PrimaryButtonText = "Show its screen",
            SecondaryButtonText = "Extend display",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        try
        {
            var result = await _connectDialog.ShowAsync();
            if (result == ContentDialogResult.Primary || result == ContentDialogResult.Secondary)
            {
                _extend = result == ContentDialogResult.Secondary;
                ConnectManual();
            }
        }
        catch (Exception)
        {
            // Another dialog is open (a PIN prompt); the address stays typed for next time.
        }
        finally
        {
            _connectDialog = null;
        }
    }

    /// <summary>Viewer: start this PC's hotspot, hand its name and passphrase to the host inside the
    /// encrypted session, then reconnect once the host appears on the hotspot.</summary>
    private async Task SwitchToDirectWifiAsync()
    {
        // SessionStarted and DirectLinkRequested can both ask for the switch: one offer is enough.
        if (_viewer is null || _viewerFingerprint is null || _directStarting) return;
        _directStarting = true;
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
            _directStatus = error ?? "Disconnected before the switch.";
            RenderIf("connect", "peer");
            return;
        }
        _directPending = (_viewerFingerprint, _viewingName, _viewerExtend, DateTimeOffset.UtcNow.AddSeconds(60));
        _viewer.OfferDirectLink(_hotspot.Ssid!, _hotspot.Passphrase!);
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
        var error = await _hotspot.StartAsync();
        if (error is null && _host?.OfferDirectLink(sessionId, _hotspot.Ssid!, _hotspot.Passphrase!) == true)
            _hostError = null;
        else
            _hostError = error ?? $"{name} isn't connected any more.";
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
        Connect($"{direct}:{peer.Port}", d.Name, d.Fingerprint);
    }

    private void SettingsPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("\uE713", "Settings", "How Annex starts, shares and shows other computers."));
        p.Children.Add(Ui.Section("General"));
        GeneralSection(p);
        p.Children.Add(Ui.Section("Sharing"));
        SharingSection(p);
        p.Children.Add(Ui.Section("Viewing"));
        ViewingSection(p);
        p.Children.Add(Ui.Section("Developer"));
        DeveloperSection(p);
    }

    /// <summary>Identity codes, addresses, stream and latency figures (Settings → Developer).</summary>
    private bool ShowTech => _settings.ShowTechnicalDetails;

    private void GeneralSection(StackPanel p)
    {
        p.Children.Add(Ui.Card("", "Device name",
            "Shown to other computers. Leave empty to use this PC's name.", Ui.Reuse(_nameBox)));
        p.Children.Add(Ui.Card("", "Start with Windows", "Opens in the tray when you sign in.",
            Ui.Switch(AppSettings.StartWithWindows, on => AppSettings.StartWithWindows = on)));
        p.Children.Add(Ui.Card("", "Start sharing when Annex opens", null,
            Ui.Switch(_settings.StartSharingAtLaunch, on => { _settings.StartSharingAtLaunch = on; _settings.Save(); })));
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
                _settings.Save();
            };
            p.Children.Add(Ui.Card("", "Graphics card",
                _host is null ? "Automatic uses the one driving this screen." : "Stop sharing to change this.", gpuBox));
        }
        p.Children.Add(Ui.Card("", "Hardware video encoder",
            _host is null ? "Recommended. Lowest delay." : "Stop sharing to change this.",
            Ui.Switch(_settings.PreferHardwareEncoder, on => { _settings.PreferHardwareEncoder = on; _settings.Save(); }, enabled: _host is null)));
    }

    private void ViewingSection(StackPanel p)
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
            _settings.Save();
        };
        p.Children.Add(Ui.Card("", "Quality",
            "Use Fast on Wi-Fi.", quality));
        p.Children.Add(Ui.Card("", "Extend display by default", null,
            Ui.Switch(_settings.ExtendByDefault, on => { _settings.ExtendByDefault = on; _extend = on; _settings.Save(); })));
        p.Children.Add(Ui.Card("", "Control the other computer",
            "When it allows it. Press Ctrl+Alt+Shift+Q to leave.",
            Ui.Switch(_settings.UseMouseAndKeyboard, on => { _settings.UseMouseAndKeyboard = on; _settings.Save(); })));
        DirectWifiOption(p);
    }

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
            p.Children.Add(Ui.Card("", "Can use this PC's mouse and keyboard", null, Ui.Switch(peer.AllowInput, on =>
            {
                _core?.SetAllowInput(peer.Fingerprint, on);
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
        var buttons = Ui.Row(LinkButton("Release notes", $"{RepoUrl}/releases/tag/v{AnnexNative.Version}"),
            LinkButton("Report an issue", $"{RepoUrl}/issues"),
            new HyperlinkButton { Content = "View on GitHub", NavigateUri = new Uri(RepoUrl) });
        buttons.HorizontalAlignment = HorizontalAlignment.Center;
        buttons.Margin = new Thickness(0, 16, 0, 0);
        head.Children.Add(buttons);
        var copyright = Ui.Text("MIT License · © 2026 Nalinish Ranjan", "CaptionTextBlockStyle", Ui.Tertiary, wrap: false);
        copyright.HorizontalAlignment = HorizontalAlignment.Center;
        copyright.Margin = new Thickness(0, 14, 0, 0);
        head.Children.Add(copyright);
        p.Children.Add(Ui.Group(head));

        p.Children.Add(Ui.Section("Updates"));
        var checkNow = Ui.Button("Check now", () => _ = _updater.CheckAsync(_settings.BetaUpdates));
        checkNow.IsEnabled = !_updater.Busy;
        checkNow.HorizontalAlignment = HorizontalAlignment.Left;
        var last = _updater.LastChecked is { } when
            ? $"Last checked: {when.LocalDateTime:g}"
            : "Not checked yet.";
        p.Children.Add(Ui.Group(
            Ui.GroupRow("Check for updates automatically", "Once a day, from GitHub. Nothing about you is sent.",
                Ui.Switch(_settings.CheckForUpdates, on => { _settings.CheckForUpdates = on; _settings.Save(); })),
            Ui.GroupRow("Receive beta updates", "Beta versions include features in development and may contain bugs.",
                Ui.Switch(_settings.BetaUpdates, on =>
                {
                    _settings.BetaUpdates = on;
                    _settings.Save();
                    _ = _updater.CheckAsync(on);
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
            Ui.Switch(_settings.ShowTechnicalDetails, on => { _settings.ShowTechnicalDetails = on; _settings.Save(); Render(); })));
        p.Children.Add(Ui.Card("", "Save a log file", "For bug reports. Stays on this PC. Off by default.",
            Ui.Switch(_settings.SaveLog, on => { _settings.SaveLog = on; _settings.Save(); AnnexNative.SetLogEnabled(on); })));
        if (_settings.SaveLog)
            p.Children.Add(Ui.Card("", "Log folder", null,
                Ui.Button("Open", () => System.Diagnostics.Process.Start("explorer.exe", DataDirectory))));
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
            if (_settings.CheckForUpdates) _ = _updater.CheckAsync(_settings.BetaUpdates);
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
            _settings.DismissedUpdate = u.Version;
            _settings.Save();
            RenderIf("share", "connect");
        });
        p.Children.Add(Ui.Card("\uE777", UpdateText(), "Annex closes and opens again when it's done.",
            Ui.Row(notNow, Ui.Button("Install", () => _ = InstallUpdateAsync(), accent: true))));
    }

    private async Task InstallUpdateAsync()
    {
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
                if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            }
            catch (Exception)
            {
                return; // another dialog is open
            }
        }
        if (await _updater.InstallAsync())
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
        if (await dialog.ShowAsync() == ContentDialogResult.Primary)
        {
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
        var status = _viewer is not null ? $"Connected to {_viewingName}"
            : _host is null ? "Not sharing"
            : _sessions.Count > 0 ? $"Sharing with {string.Join(", ", _sessions.Values.Select(s => s.Name).Distinct())}"
            : "Sharing · waiting for a connection";
        _trayMenu.ShowAtCursor(
            new TrayMenu.Model(_core?.DeviceName ?? "Annex", status, _host is not null,
                _viewer is not null ? $"Viewing {_viewingName}" : null, _hosts, TrayLive()),
            new TrayMenu.Actions(
                SetSharing: on => SetHosting(on),
                Connect: (peer, extend) => { _extend = extend; ConnectToPeer(peer); },
                Disconnect: Disconnect,
                Open: ShowFromTray,
                OpenSettings: () => { ShowFromTray(); Go("settings"); },
                Quit: Quit));
    }

    /// <summary>The tray menu's live block: the same title as Nearby's session bar, and its numbers.</summary>
    private (string Title, string? Detail)? TrayLive()
    {
        if (_viewer is not null)
            return (_viewerStatus ?? $"Connected to {_viewingName}",
                _lat is { } l ? $"{l.P50Ms:F0} ms delay · {l.Fps:F0} fps" : null);
        if (_sessions.Count == 0) return null;
        var s = _sessions.Values.First();
        return (s.Mode == "extend" ? $"Extending to {s.Name}" : $"Showing this screen on {s.Name}",
            _hostStream is { } st && s.Mode != "extend" ? $"{st.Width}×{st.Height} · {st.Fps} fps" : null);
    }

    private void ShowFromTray()
    {
        AppWindow.Show();
        Activate();
    }

    private void Quit()
    {
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
        _hostError = null;
        var hardware = _settings.PreferHardwareEncoder;
        try
        {
            try
            {
                _host = HostHandle.StartScreen(_core, _monitorIndex, DefaultPort, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)), _settings.GpuLuid);
            }
            catch (AnnexException)
            {
                // Default port busy: take any free port (discovery advertises it).
                _host = HostHandle.StartScreen(_core, _monitorIndex, 0, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)), _settings.GpuLuid);
            }
            if (_tray is not null) _tray.Tooltip = "Annex — sharing this screen";
        }
        catch (AnnexException ex)
        {
            _hostError = $"Couldn't start sharing: {ex.Message}";
        }
    }

    private void StopHosting()
    {
        var h = _host;
        _host = null;
        h?.Dispose();
        _sessions.Clear();
        _hostStream = null;
        _ = _hotspot.StopAsync();
        if (_tray is not null) _tray.Tooltip = "Annex";
    }

    private async void OnHostEvent(CoreEvent e)
    {
        switch (e)
        {
            case CoreEvent.PairingPin pin:
                await ShowPin(pin);
                break;
            case CoreEvent.PairingFinished f when f.RequestId == _pinRequest:
                _pinDialog?.Hide();
                if (!f.Success) _hostError = $"Pairing with {f.PeerName} failed.";
                RefreshTrusted();
                break;
            case CoreEvent.SessionStarted s:
                _sessions[s.SessionId] = (s.PeerName, s.Mode, s.Input);
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
                    if (await dialog.ShowAsync() == ContentDialogResult.Primary)
                    {
                        _core.SetAllowInput(ask.Fingerprint, true);
                        RefreshTrusted();
                    }
                }
                catch (Exception)
                {
                    // Another dialog is open; the switch in Paired devices still works.
                }
                break;
            }
            case CoreEvent.DirectLinkRequested req when _host is not null:
            {
                // The viewer (a Mac) asks for a direct link: run the hotspot here and hand it over.
                var error = await _hotspot.StartAsync();
                if (error is null) _host?.OfferDirectLink(req.SessionId, _hotspot.Ssid!, _hotspot.Passphrase!);
                else _host?.RefuseDirectLink(req.SessionId, error);
                RenderIf("share");
                break;
            }
            case CoreEvent.DirectLinkOffer offer when _host is not null:
                // Another Windows PC offers its hotspot; joining one from here isn't supported yet.
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
            var result = await _pinDialog.ShowAsync();
            // Closed by the user (not by pairing finishing): refuse the pairing.
            if (result == ContentDialogResult.None && _pinRequest == pin.RequestId && _host is not null)
                _core.CancelRequest(pin.RequestId);
        }
        catch (Exception ex)
        {
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
        if (peer.ConnectAddress is not { } address) return;
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

    private void Connect(string address, string title, string? pinnedFingerprint)
    {
        if (_core is null) return;
        Disconnect();
        var token = new object();
        _viewerToken = token;
        try
        {
            _viewer = ViewerHandle.ConnectWindowed(_core, address, pinnedFingerprint,
                e => _ui.TryEnqueue(() => { if (ReferenceEquals(_viewerToken, token)) OnViewerEvent(e); }),
                extend: _extend, input: _settings.UseMouseAndKeyboard, quality: _settings.Quality);
            _viewingName = title;
            _viewerStatus = $"Connecting to {title}…";
            if (_tray is not null) _tray.Tooltip = $"Annex — connecting to {title}";
        }
        catch (AnnexException ex)
        {
            _viewerStatus = $"Couldn't connect: {ex.Message}";
        }
        RenderIf("connect", "peer");
    }

    private async void OnViewerEvent(CoreEvent e)
    {
        switch (e)
        {
            case CoreEvent.PairingPinRequired req:
                await PromptForPin(req);
                return;
            case CoreEvent.PairingFinished { Success: false } f:
                _viewerStatus = $"Pairing with {f.PeerName} didn't finish. Check the PIN and try again.";
                break;
            case CoreEvent.SessionStarted s:
                _viewerStatus = (s.Mode == "extend" ? $"Extending {s.PeerName}'s display" : $"Viewing {s.PeerName}")
                    + (s.Link == "direct" ? " · cable" : "");
                _viewingName = s.PeerName;
                _viewerFingerprint = s.Fingerprint;
                _viewerExtend = s.Mode == "extend";
                _controlling = s.Input;
                if (_tray is not null) _tray.Tooltip = $"Annex — viewing {s.PeerName}";
                RefreshTrusted();
                if (_settings.ConnectOverDirectWifi && !_onDirectWifi && _directPending is null)
                    _ = SwitchToDirectWifiAsync();
                break;
            case CoreEvent.DirectLinkRequested:
                // The host (e.g. a Mac, which can't run a hotspot) asks this PC to set up the link.
                if (_directPending is null && !_onDirectWifi) _ = SwitchToDirectWifiAsync();
                break;
            case CoreEvent.InputAllowed a:
                _controlling = a.Allowed;
                break;
            case CoreEvent.DirectLinkUnavailable u:
                _directPending = null;
                _directStatus = u.Reason;
                _ = _hotspot.StopAsync();
                break;
            case CoreEvent.StreamChanged(var st):
                _viewerStatus = $"Viewing {_viewingName} · {st.Width}×{st.Height}";
                break;
            case CoreEvent.Latency(var l):
                _lat = l;
                break;
            case CoreEvent.SessionEnded(_, var reason):
                // Switching to direct Wi-Fi drops the old connection on purpose.
                _viewerStatus = _directPending is not null ? null : reason == "user" ? null : FriendlyReason(reason, _viewingName);
                if (reason == "user" && _onDirectWifi)
                {
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
        ShowFromTray(); // the dialog needs a visible window
        var box = new TextBox
        {
            PlaceholderText = "123 456",
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
            result = await dialog.ShowAsync();
        }
        catch (Exception)
        {
            // Another dialog is already open: refuse the pairing rather than crash (it can be retried).
            result = ContentDialogResult.None;
        }
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
        _viewer = null;
        _viewerToken = null;
        v?.Dispose();
    }

    private void Shutdown()
    {
        _tray?.Dispose();
        _tray = null;
        _trayMenu?.Close();
        _trayMenu = null;
        StopHosting();
        Disconnect();
        _hotspot.StopAsync().Wait(TimeSpan.FromSeconds(3));
        _discovery?.Dispose();
        _discovery = null;
        _core?.Dispose();
        _core = null;
    }
}

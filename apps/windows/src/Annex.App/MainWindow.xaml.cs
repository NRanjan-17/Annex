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
    private string? _latency;
    private bool _controlling;
    private string? _viewerFingerprint;
    private bool _viewerExtend;
    /// <summary>Connected over this PC's hotspot right now.</summary>
    private bool _onDirectWifi;

    // Direct Wi-Fi link: this PC's hotspot, and a reconnect we're waiting for after switching.
    private readonly Hotspot _hotspot = new();
    private (string Fingerprint, string Name, bool Extend, DateTimeOffset Until)? _directPending;
    private string? _directStatus;
    /// <summary>Identifies the current viewer, so events still queued from a replaced one are ignored.</summary>
    private object? _viewerToken;
    /// <summary>Nearby is being re-checked (the refresh button shows progress until then).</summary>
    private DateTimeOffset _refreshingUntil;

    // Updates.
    private readonly Updater _updater = new();

    // UI.
    private readonly NavigationView _nav = new();
    private readonly ContentControl _page = new()
    {
        HorizontalContentAlignment = HorizontalAlignment.Stretch,
        VerticalContentAlignment = VerticalAlignment.Stretch,
    };
    private readonly TextBlock _deviceTitle = new() { TextTrimming = TextTrimming.CharacterEllipsis };
    private readonly TextBlock _deviceSubtitle = new();
    private string _pane = "share";
    private bool _buildingNav;
    // Text boxes survive page rebuilds so typing isn't interrupted by status updates.
    private readonly TextBox _addressBox = new() { PlaceholderText = "192.168.1.20 or host:port", Width = 240 };
    private readonly TextBox _nameBox = new() { Width = 240 };

    public static string DataDirectory =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Annex");

    public MainWindow()
    {
        InitializeComponent();
        _ui = DispatcherQueue.GetForCurrentThread();
        _extend = _settings.ExtendByDefault;
        SystemBackdrop = new MicaBackdrop();
        ExtendsContentIntoTitleBar = true;
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
        foreach (var pane in new[] { "share", "connect", "devices", "general", "viewing", "about", "connect", "general", "share" })
            Go(pane);
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

    // ---- Shell: title bar + sidebar ---------------------------------------------------

    private void BuildShell()
    {
        RootGrid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(48) });
        RootGrid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });

        var title = Ui.Row(Ui.AppIcon(16), Ui.Text("Annex", "CaptionTextBlockStyle", wrap: false));
        title.Margin = new Thickness(16, 0, 0, 0);
        title.VerticalAlignment = VerticalAlignment.Center;
        var titleBar = new Grid { Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent) };
        titleBar.Children.Add(title);
        RootGrid.Children.Add(titleBar);
        SetTitleBar(titleBar);

        // One continuous Mica surface, like Settings: no lighter rounded "box" behind the pages.
        var clear = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        _nav.Resources["NavigationViewContentBackground"] = clear;
        _nav.Resources["NavigationViewContentGridBorderBrush"] = clear;
        _nav.Resources["NavigationViewDefaultPaneBackground"] = clear;
        _nav.Resources["NavigationViewExpandedPaneBackground"] = clear;
        _nav.PaneDisplayMode = NavigationViewPaneDisplayMode.Left;
        _nav.IsBackButtonVisible = NavigationViewBackButtonVisible.Collapsed;
        _nav.IsPaneToggleButtonVisible = false;
        _nav.IsSettingsVisible = false;
        _nav.OpenPaneLength = 300;
        _nav.Content = _page;
        _nav.PaneHeader = DeviceCard();
        _nav.SelectionChanged += (_, args) =>
        {
            if (_buildingNav || args.SelectedItemContainer?.Tag is not string tag) return;
            _pane = tag;
            Render();
        };
        Grid.SetRow(_nav, 1);
        RootGrid.Children.Add(_nav);
        BuildNav();
        Render();
    }

    private UIElement DeviceCard()
    {
        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 0 };
        _deviceTitle.Style = Ui.Res<Style>("BodyStrongTextBlockStyle");
        _deviceSubtitle.Style = Ui.Res<Style>("CaptionTextBlockStyle");
        _deviceSubtitle.Foreground = Ui.Secondary;
        text.Children.Add(_deviceTitle);
        text.Children.Add(_deviceSubtitle);
        var card = Ui.Row(Ui.AppIcon(40), text);
        card.Spacing = 12;
        card.Margin = new Thickness(16, 4, 16, 12);
        UpdateDeviceCard();
        return card;
    }

    private void UpdateDeviceCard()
    {
        _deviceTitle.Text = _core?.DeviceName ?? "Annex";
        _deviceSubtitle.Text = _host is not null ? "Sharing" : "This PC";
    }

    private static NavigationViewItem NavItem(string text, string glyph, string tag, string? info = null)
    {
        var item = new NavigationViewItem { Content = text, Icon = Ui.Glyph(glyph), Tag = tag };
        if (info is not null) item.InfoBadge = new InfoBadge { Value = int.TryParse(info, out var n) ? n : -1 };
        return item;
    }

    /// <summary>Rebuilds the sidebar (nearby computers come and go) and keeps the selection.</summary>
    private void BuildNav()
    {
        _buildingNav = true;
        var items = _nav.MenuItems;
        items.Clear();
        items.Add(NavItem("Share this PC", "", "share"));
        items.Add(NavItem("Connect", "", "connect"));
        items.Add(new NavigationViewItemHeader { Content = Ui.Section("Nearby", RefreshControl()) });
        if (_hosts.Count == 0)
            items.Add(new NavigationViewItem { Content = "Searching…", Icon = Ui.Glyph(""), IsEnabled = false, SelectsOnInvoked = false });
        foreach (var h in _hosts)
            items.Add(NavItem(h.Name, "", "peer:" + h.Instance));
        items.Add(new NavigationViewItemHeader { Content = "Settings" });
        items.Add(NavItem("Paired devices", "", "devices"));
        items.Add(NavItem("General", "", "general"));
        items.Add(NavItem("Viewing", "", "viewing"));
        items.Add(NavItem("About", "", "about"));
        if (_pane.StartsWith("peer:", StringComparison.Ordinal) && !_hosts.Any(h => "peer:" + h.Instance == _pane))
            _pane = "connect";
        _nav.SelectedItem = items.OfType<NavigationViewItem>().FirstOrDefault(i => (string?)i.Tag == _pane);
        _buildingNav = false;
    }

    private void Go(string pane)
    {
        _pane = pane;
        BuildNav();
        Render();
    }

    /// <summary>Rebuilds the current page from state.</summary>
    private void Render()
    {
        UpdateDeviceCard();
        var stack = Ui.Page(out var scroller);
        if (_startupError is not null)
            stack.Children.Add(Ui.Info("Annex couldn't start", _startupError, InfoBarSeverity.Error));
        switch (_pane)
        {
            case "share": SharePage(stack); break;
            case "connect": ConnectPage(stack); break;
            case "devices": DevicesPage(stack); break;
            case "general": GeneralPage(stack); break;
            case "viewing": ViewingPage(stack); break;
            case "about": AboutPage(stack); break;
            default:
                var peer = _hosts.FirstOrDefault(h => "peer:" + h.Instance == _pane);
                if (peer is not null) PeerPage(stack, peer); else ConnectPage(stack);
                break;
        }
        _page.Content = scroller;
    }

    private void RenderIf(params string[] panes)
    {
        if (panes.Contains(_pane) || (_pane.StartsWith("peer:", StringComparison.Ordinal) && panes.Contains("peer"))) Render();
        else UpdateDeviceCard();
    }

    // ---- Pages ------------------------------------------------------------------------

    private void SharePage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Share this PC",
            "Let another computer show this screen or use it as an extra display."));
        UpdateBanner(p);
        var action = _host is null
            ? Ui.Button("Start sharing", () => SetHosting(true), accent: true)
            : Ui.Button("Stop sharing", () => SetHosting(false));
        action.MinWidth = 140;
        p.Children.Add(Ui.Card(_host is null ? "" : "", _host is null ? "Sharing is off" : "Sharing is on", ShareStatus(), action));
        if (_monitors.Count > 1)
        {
            var box = new ComboBox { ItemsSource = _monitors.Select(m => m.Label).ToList(), SelectedIndex = _monitorIndex, MinWidth = 220, IsEnabled = _host is null };
            box.SelectionChanged += (_, _) => _monitorIndex = Math.Max(0, box.SelectedIndex);
            p.Children.Add(Ui.Card("", "Screen to share", _host is null ? null : "Stop sharing to pick another screen.", box));
        }
        if (_hostError is not null)
            p.Children.Add(Ui.Info("Sharing problem", _hostError, InfoBarSeverity.Warning));
        ExtendCard(p);
        p.Children.Add(Ui.Card("", "Direct Wi-Fi",
            _hotspot.IsOn
                ? $"Network “{_hotspot.Ssid}”, password {_hotspot.Passphrase}. Other computers can join it for less lag."
                : "Runs a hotspot on this PC so another computer connects straight to it. Less lag.",
            Ui.Switch(_hotspot.IsOn, on => _ = SetHotspotAsync(on))));
        if (_sessions.Count > 0)
        {
            p.Children.Add(Ui.Section("Connected now"));
            foreach (var (id, session) in _sessions)
                p.Children.Add(Ui.Card("", session.Mode == "extend" ? $"{session.Name} is an extra display" : $"Showing this screen on {session.Name}",
                    session.Input ? "Can use this PC's mouse and keyboard." : null,
                    Ui.Button("Switch to direct Wi-Fi", () => _ = OfferDirectWifiToViewerAsync(id, session.Name))));
        }

        if (_host is not null)
        {
            p.Children.Add(Ui.Section("Connect from another computer"));
            p.Children.Add(Ui.Card("", $"1. Open Annex on the other computer and choose “{_core?.DeviceName}”."));
            p.Children.Add(Ui.Card("", "2. Pick Show its screen or Be its extra display."));
            p.Children.Add(Ui.Card("", "3. Enter the PIN shown here (first time only).", $"Identity code: {Fingerprint}"));
            var addresses = LocalAddresses();
            p.Children.Add(Ui.Card("", addresses.Count == 0 ? "This PC isn't on a network." : "Not listed there? Connect by address:",
                addresses.Count == 0 ? "Connect it to Wi-Fi, Ethernet or a cable." : null));
            foreach (var (ip, direct) in addresses)
            {
                var text = _host.Port == DefaultPort ? ip : $"{ip}:{_host.Port}";
                var value = Ui.Text(text, "BodyStrongTextBlockStyle", wrap: false);
                value.FontFamily = new FontFamily("Consolas");
                value.FontSize = 16;
                value.IsTextSelectionEnabled = true;
                var copy = new Button { Content = "Copy" };
                copy.Click += (_, _) =>
                {
                    var data = new DataPackage();
                    data.SetText(text);
                    Clipboard.SetContent(data);
                    copy.Content = "Copied";
                };
                var card = Ui.Card(direct ? "" : "", text, direct ? "Cable" : "Network", copy);
                card.Margin = new Thickness(36, 0, 0, 2);
                p.Children.Add(card);
            }
        }

        if (_sessions.Count > 0)
        {
            p.Children.Add(Ui.Section("Connected now"));
            foreach (var (_, s) in _sessions)
            {
                var title = s.Mode == "extend" ? $"{s.Name} is an extra display for this PC" : $"{s.Name} is showing this screen";
                var detail = s.Mode == "extend" ? "Arrange it in Settings → System → Display" :
                    _hostStream is { } st ? $"{st.Width}×{st.Height} at {st.Fps} fps" : null;
                if (s.Input) detail = (detail is null ? "" : detail + " · ") + "using your mouse and keyboard";
                p.Children.Add(Ui.Card(s.Mode == "extend" ? "" : "", title, detail));
            }
        }

        p.Children.Add(Ui.Section("Video"));
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
                p.Children.Add(Ui.Card("", "Extra displays are ready",
                    "Other computers can be a second screen for this PC."));
                break;
            case "not_installed":
                p.Children.Add(Ui.Card("", "Extra displays aren't set up",
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
                p.Children.Add(Ui.Card("", "Extra displays aren't working", why));
                break;
        }
    }

    /// <summary>Refresh button for Nearby, or a small spinner while the list is being re-checked.</summary>
    private FrameworkElement RefreshControl()
    {
        if (DateTimeOffset.UtcNow < _refreshingUntil)
            return new ProgressRing { IsActive = true, Width = 16, Height = 16, Margin = new Thickness(6) };
        return Ui.IconButton("\uE72C", "Refresh", RefreshNearby);
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

    /// The two things you can do with another computer, as buttons that say what happens.
    private StackPanel ConnectButtons(Action<bool> connect, PeerInfo? peer = null)
    {
        var show = Ui.Button("Show its screen here", () => connect(false), accent: true);
        var extend = Ui.Button("Be its extra display", () => connect(true));
        ToolTipService.SetToolTip(show, "Use the other computer's screen on this PC.");
        ToolTipService.SetToolTip(extend, peer?.Extend == false
            ? "Extend isn't set up on that computer."
            : "This PC becomes a second screen for it.");
        var sharing = peer?.Sharing ?? true;
        show.IsEnabled = _viewer is null && sharing;
        extend.IsEnabled = _viewer is null && sharing && peer?.Extend != false;
        return Ui.Row(show, extend);
    }

    private string PeerSubtitle(PeerInfo h) =>
        !h.Sharing ? "Not sharing"
        : (IsPaired(h) ? "Paired" : "New · PIN needed")
          + (h.Extend == false ? " · Extend unavailable" : "");

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
        "extend_not_supported" => $"{peer} can't add an extra display. Set up Extend on that computer, or use Show its screen.",
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

    private void ConnectPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Connect",
            "Show another computer's screen here, or be its extra display."));
        UpdateBanner(p);
        ViewerStatus(p);

        p.Children.Add(Ui.Section("Nearby", RefreshControl()));
        if (_hosts.Count == 0)
            p.Children.Add(Ui.Card("", "Looking for computers…",
                "Start sharing on the other computer.",
                new ProgressRing { IsActive = true, Width = 20, Height = 20 }));
        foreach (var h in _hosts)
            p.Children.Add(Ui.Card("", h.Name, PeerSubtitle(h),
                ConnectButtons(extend => { _extend = extend; ConnectToPeer(h); }, h)));
        if (_hosts.Count == 0 || _hosts.All(h => !h.Sharing))
            NotShowingUp(p);
        DirectWifiOption(p);

        p.Children.Add(Ui.Section("Connect by address"));
        p.Children.Add(Ui.Card("", "Address", "Use this if the computer isn't listed.",
            Ui.Reuse(_addressBox)));
        var buttons = ConnectButtons(extend => { _extend = extend; ConnectManual(); });
        buttons.HorizontalAlignment = HorizontalAlignment.Right;
        buttons.Margin = new Thickness(0, 4, 0, 0);
        p.Children.Add(buttons);
    }

    private void ViewerStatus(StackPanel p)
    {
        if (_directStatus is not null)
            p.Children.Add(Ui.Card("", "Direct Wi-Fi", _directStatus));
        if (_viewerStatus is null) return;
        p.Children.Add(Ui.Card(_viewer is not null ? "" : "", _viewerStatus, _latency,
            _viewer is not null ? Ui.Button("Disconnect", Disconnect) : null));
        if (_viewer is not null && _controlling)
            p.Children.Add(Ui.Card("", "You're in control", "Press Ctrl+Alt+Shift+Q to leave."));
        else if (_viewer is not null)
            p.Children.Add(Ui.Card("", "Press Esc to leave the viewer."));
        if (_viewer is not null && _onDirectWifi)
            p.Children.Add(Ui.Card("", "On direct Wi-Fi", $"Connected through this PC's hotspot “{_hotspot.Ssid}”."));
        else if (_viewer is not null && _directPending is null)
            p.Children.Add(Ui.Card("", "Switch to direct Wi-Fi",
                "Less lag: the other computer joins this PC's hotspot.",
                Ui.Button("Switch", () => _ = SwitchToDirectWifiAsync())));
    }

    /// <summary>Viewer: start this PC's hotspot, hand its name and passphrase to the host inside the
    /// encrypted session, then reconnect once the host appears on the hotspot.</summary>
    private async Task SwitchToDirectWifiAsync()
    {
        if (_viewer is null || _viewerFingerprint is null) return;
        _directStatus = "Starting this PC's hotspot…";
        RenderIf("connect", "peer");
        var error = await _hotspot.StartAsync();
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

    private void PeerPage(StackPanel p, PeerInfo peer)
    {
        p.Children.Add(Ui.Hero("", peer.Name, IsPaired(peer)
            ? "Paired. Connects without a PIN."
            : "The first connection needs a PIN."));
        ViewerStatus(p);
        if (!peer.Sharing)
            p.Children.Add(Ui.Info("Not sharing", $"Start sharing on {peer.Name} first.", InfoBarSeverity.Informational));
        var show = Ui.Button("Show its screen", () => { _extend = false; ConnectToPeer(peer); }, accent: true);
        var extend = Ui.Button("Be its extra display", () => { _extend = true; ConnectToPeer(peer); });
        show.IsEnabled = _viewer is null && peer.Sharing;
        extend.IsEnabled = _viewer is null && peer.Sharing && peer.Extend != false;
        p.Children.Add(Ui.Card("", "Show its screen here", $"Use {peer.Name}'s screen on this PC.", show));
        p.Children.Add(Ui.Card("", "Be its extra display", $"This PC becomes a second screen for {peer.Name}.", extend));
        p.Children.Add(Ui.Section("Details"));
        p.Children.Add(Ui.Card("", "Addresses", $"{string.Join(", ", peer.Addresses)} · port {peer.Port}"));
        if (peer.Fingerprint is { } fp)
            p.Children.Add(Ui.Card("", "Identity code", AnnexNative.DisplayFingerprint(fp)));
    }

    private void DevicesPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Paired devices",
            "Paired devices connect without a PIN."));
        if (_trusted.Any(t => t.AllowInput))
            p.Children.Add(Ui.Info("Note", "Remote control can't type into administrator windows or UAC prompts.",
                InfoBarSeverity.Informational));
        if (_trusted.Count == 0)
            p.Children.Add(Ui.Card("", "No paired devices", "Pair by connecting from another computer."));
        foreach (var peer in _trusted)
        {
            var control = new ToggleSwitch { IsOn = peer.AllowInput, OnContent = "Can control", OffContent = "View only", MinWidth = 0 };
            control.Toggled += (_, _) =>
            {
                _core?.SetAllowInput(peer.Fingerprint, control.IsOn);
                RefreshTrusted();
            };
            var remove = new Button { Content = "Remove" };
            remove.Click += async (_, _) => await ConfirmRemove(peer);
            p.Children.Add(Ui.Card("", peer.Name, LastSeen(peer), Ui.Row(control, remove)));
        }
        p.Children.Add(Ui.Text($"This PC's identity code: {Fingerprint}", "CaptionTextBlockStyle", Ui.Secondary));
    }

    private void GeneralPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "General",
            "Startup, updates and this PC's name."));
        p.Children.Add(Ui.Card("", "Device name",
            "Shown to other computers. Leave empty to use this PC's name.", Ui.Reuse(_nameBox)));
        p.Children.Add(Ui.Card("", "Start with Windows", "Opens in the tray when you sign in.",
            Ui.Switch(AppSettings.StartWithWindows, on => AppSettings.StartWithWindows = on)));
        p.Children.Add(Ui.Card("", "Start sharing when Annex opens", null,
            Ui.Switch(_settings.StartSharingAtLaunch, on => { _settings.StartSharingAtLaunch = on; _settings.Save(); })));

        p.Children.Add(Ui.Section("Updates"));
        p.Children.Add(UpdateStatusCard());
        p.Children.Add(Ui.Card("\uE895", "Check for updates automatically", "Once a day, from GitHub. Nothing about you is sent.",
            Ui.Switch(_settings.CheckForUpdates, on => { _settings.CheckForUpdates = on; _settings.Save(); })));
        p.Children.Add(Ui.Card("\uE9D5", "Beta updates", "Get pre-release versions early. Turn off to go back to the latest normal release.",
            Ui.Switch(_settings.BetaUpdates, on =>
            {
                _settings.BetaUpdates = on;
                _settings.Save();
                _ = _updater.CheckAsync(on);
            })));
    }

    // ---- Updates ----------------------------------------------------------------------

    private void StartUpdateChecks()
    {
        _updater.Changed += () => _ui.TryEnqueue(() => RenderIf("general", "share", "connect"));
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

    private static ProgressRing Spinner() => new() { IsActive = true, Width = 20, Height = 20 };

    /// <summary>The status line on the General page: up to date, checking, or an update to install.</summary>
    private Border UpdateStatusCard()
    {
        var (title, detail, control) = _updater.State switch
        {
            Updater.Stage.Checking => ("Checking for updates…", (string?)null, (UIElement)Spinner()),
            Updater.Stage.Available => (UpdateText(), "Annex closes and opens again when it's done.", UpdateButtons()),
            Updater.Stage.Downloading => ("Downloading the update…", null, Spinner()),
            Updater.Stage.Installing => ("Installing… Annex opens again in a moment.", null, Spinner()),
            Updater.Stage.UpToDate => ("Annex is up to date.", $"Version {AnnexNative.Version}.", CheckNowButton()),
            Updater.Stage.Failed => (_updater.Error ?? "Couldn't check for updates.", null, CheckNowButton()),
            _ => ($"Version {AnnexNative.Version}", null, CheckNowButton()),
        };
        return Ui.Card("\uE777", title, detail, control);
    }

    private Button CheckNowButton() => Ui.Button("Check now", () => _ = _updater.CheckAsync(_settings.BetaUpdates));

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

    private void ViewingPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Viewing", "How other computers appear here."));
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
        p.Children.Add(Ui.Card("", "Use as an extra display by default", null,
            Ui.Switch(_settings.ExtendByDefault, on => { _settings.ExtendByDefault = on; _extend = on; _settings.Save(); })));
        p.Children.Add(Ui.Card("", "Control the other computer",
            "When it allows it. Press Ctrl+Alt+Shift+Q to leave.",
            Ui.Switch(_settings.UseMouseAndKeyboard, on => { _settings.UseMouseAndKeyboard = on; _settings.Save(); })));
    }

    private void AboutPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Annex", $"Version {AnnexNative.Version}. Encrypted, local, no accounts.",
            Ui.AppIcon(64)));
        p.Children.Add(Ui.Card("", "Website and source code", "github.com/NRanjan-17/Annex",
            new HyperlinkButton { Content = "Open", NavigateUri = new Uri("https://github.com/NRanjan-17/Annex") }));
        p.Children.Add(Ui.Card("", "Report an issue", null,
            new HyperlinkButton { Content = "Open", NavigateUri = new Uri("https://github.com/NRanjan-17/Annex/issues") }));
        p.Children.Add(Ui.Card("", "Save a log file", "For bug reports. Stays on this PC. Off by default.",
            Ui.Switch(_settings.SaveLog, on => { _settings.SaveLog = on; _settings.Save(); AnnexNative.SetLogEnabled(on); })));
        if (_settings.SaveLog)
            p.Children.Add(Ui.Card("", "Log folder", null,
                Ui.Button("Open", () => System.Diagnostics.Process.Start("explorer.exe", DataDirectory))));
        p.Children.Add(Ui.Text("MIT License · Copyright © 2026 Nalinish Ranjan", "CaptionTextBlockStyle", Ui.Secondary));
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
                _viewer is not null ? $"Viewing {_viewingName}" : null, _hosts),
            new TrayMenu.Actions(
                SetSharing: on => SetHosting(on),
                Connect: (peer, extend) => { _extend = extend; ConnectToPeer(peer); },
                Disconnect: Disconnect,
                Open: ShowFromTray,
                OpenSettings: () => { ShowFromTray(); Go("general"); },
                Quit: Quit));
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
        if (address.Length > 0) Connect(address, address, null);
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
                _viewerStatus = (s.Mode == "extend" ? $"Extra display for {s.PeerName}" : $"Viewing {s.PeerName}")
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
                _latency = $"Latency {l.P50Ms:F0} ms (p95 {l.P95Ms:F0} ms) · {l.Fps:F0} fps";
                break;
            case CoreEvent.SessionEnded(_, var reason):
                // Switching to direct Wi-Fi drops the old connection on purpose.
                _viewerStatus = _directPending is not null ? null : reason == "user" ? null : FriendlyReason(reason, _viewingName);
                if (reason == "user" && _onDirectWifi)
                {
                    _onDirectWifi = false;
                    _ = _hotspot.StopAsync();
                }
                _latency = null;
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
        var result = await dialog.ShowAsync();
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

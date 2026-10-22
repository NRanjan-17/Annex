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

    // Viewer state.
    private bool _extend;
    private string _viewingName = "";
    private string? _viewerStatus;
    private string? _latency;
    private bool _controlling;

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
        AppWindow.Resize(new SizeInt32(1040, 760));
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
            _discovery = new DiscoveryHandle(_core, e => _ui.TryEnqueue(() => OnDiscovery(e)));
            _trusted = [.. _core.TrustedPeers];
            RefreshMonitors();
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
        File.WriteAllText(Path.Combine(DataDirectory, "selftest-ok"), DateTimeOffset.Now.ToString("O"));
    }

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
        items.Add(new NavigationViewItemHeader { Content = "Nearby" });
        if (_hosts.Count == 0)
            items.Add(new NavigationViewItem { Content = "Searching…", Icon = Ui.Glyph(""), IsEnabled = false, SelectsOnInvoked = false });
        foreach (var h in _hosts)
            items.Add(NavItem(h.Name, "", "peer:" + h.Instance));
        items.Add(new NavigationViewItemHeader { Content = "Annex" });
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
            "Start sharing to let another computer show this PC's screen, or become an extra display for this PC."));
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

        if (_host is not null)
        {
            p.Children.Add(Ui.Section("How to connect from the other computer"));
            p.Children.Add(Ui.Card("", "1. Open Annex on the other computer",
                $"Go to Connect. “{_core?.DeviceName}” is listed under Nearby."));
            p.Children.Add(Ui.Card("", "2. Choose what you want",
                "“Show its screen here” puts this PC's screen on that computer. “Be its extra display” turns that computer into a second screen for this PC."));
            p.Children.Add(Ui.Card("", "3. First time only: enter the PIN",
                $"This PC shows a 6-digit PIN to type there. The identity codes should match: {Fingerprint}"));
            var addresses = LocalAddresses();
            p.Children.Add(Ui.Card("", "Not listed there?",
                addresses.Count == 0
                    ? "This PC isn't on a network right now. Connect it to Wi-Fi, Ethernet or a cable."
                    : "Use Connect by address on the other computer with one of these:"));
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
                var card = Ui.Card(direct ? "" : "", text, direct ? "Direct cable" : "Network", copy);
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
        p.Children.Add(Ui.Card("", "Hardware video encoder",
            _host is null ? "Recommended. Uses the graphics card's encoder (NVENC, Quick Sync, AMF)." : "Stop sharing to change this.",
            Ui.Switch(_settings.PreferHardwareEncoder, on => { _settings.PreferHardwareEncoder = on; _settings.Save(); }, enabled: _host is null)));
    }

    private string ShareStatus()
    {
        if (_host is null) return "Other computers can't see this PC.";
        if (_sessions.Count > 0) return $"Connected to {string.Join(", ", _sessions.Values.Select(s => s.Name).Distinct())}";
        return "Waiting for another computer to connect";
    }

    /// The two things you can do with another computer, as buttons that say what happens.
    private StackPanel ConnectButtons(Action<bool> connect)
    {
        var show = Ui.Button("Show its screen here", () => connect(false), accent: true);
        var extend = Ui.Button("Be its extra display", () => connect(true));
        ToolTipService.SetToolTip(show, "See and use the other computer's screen on this PC");
        ToolTipService.SetToolTip(extend, "This PC becomes a second screen for the other computer");
        show.IsEnabled = extend.IsEnabled = _viewer is null;
        return Ui.Row(show, extend);
    }

    private void ConnectPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Connect to another computer",
            "Show another computer's screen on this PC, or use this PC as an extra display for it. Turn on sharing in Annex on that computer first."));
        ViewerStatus(p);

        p.Children.Add(Ui.Section("Nearby"));
        if (_hosts.Count == 0)
            p.Children.Add(Ui.Card("", "Looking for computers…",
                "Open Annex on the other computer and click Start sharing. It appears here automatically.",
                new ProgressRing { IsActive = true, Width = 20, Height = 20 }));
        foreach (var h in _hosts)
            p.Children.Add(Ui.Card("", h.Name, IsPaired(h) ? "Paired" : "Not paired yet · you'll enter a PIN the first time",
                ConnectButtons(extend => { _extend = extend; ConnectToPeer(h); })));

        p.Children.Add(Ui.Section("Connect by address"));
        p.Children.Add(Ui.Card("", "Address", "Shown on the other computer under How to connect. Use it when the computer isn't listed under Nearby.",
            Ui.Reuse(_addressBox)));
        var buttons = ConnectButtons(extend => { _extend = extend; ConnectManual(); });
        buttons.HorizontalAlignment = HorizontalAlignment.Right;
        buttons.Margin = new Thickness(0, 4, 0, 0);
        p.Children.Add(buttons);
    }

    private void ViewerStatus(StackPanel p)
    {
        if (_viewerStatus is null) return;
        p.Children.Add(Ui.Card(_viewer is not null ? "" : "", _viewerStatus, _latency,
            _viewer is not null ? Ui.Button("Disconnect", Disconnect) : null));
        if (_viewer is not null && _controlling)
            p.Children.Add(Ui.Card("", "Your mouse and keyboard control it", "Press Ctrl+Alt+Shift+Q in the viewer to leave."));
        else if (_viewer is not null)
            p.Children.Add(Ui.Card("", "Leave the viewer", "Press Esc, or close its window."));
    }

    private void PeerPage(StackPanel p, PeerInfo peer)
    {
        p.Children.Add(Ui.Hero("", peer.Name, IsPaired(peer)
            ? "Paired with this PC. Connects without a PIN."
            : $"On your network. The first time, you'll enter the PIN shown on {peer.Name}."));
        ViewerStatus(p);
        var show = Ui.Button("Show its screen", () => { _extend = false; ConnectToPeer(peer); }, accent: true);
        var extend = Ui.Button("Be its extra display", () => { _extend = true; ConnectToPeer(peer); });
        show.IsEnabled = extend.IsEnabled = _viewer is null;
        p.Children.Add(Ui.Card("", "Show its screen here", $"See and use {peer.Name}'s screen on this PC.", show));
        p.Children.Add(Ui.Card("", "Be its extra display", $"This PC becomes a second screen for {peer.Name}. Drag windows onto it from there.", extend));
        p.Children.Add(Ui.Section("Details"));
        p.Children.Add(Ui.Card("", "Addresses", $"{string.Join(", ", peer.Addresses)} · port {peer.Port}"));
        if (peer.Fingerprint is { } fp)
            p.Children.Add(Ui.Card("", "Identity code", AnnexNative.DisplayFingerprint(fp)));
    }

    private void DevicesPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Paired devices",
            "Devices you've paired with a PIN connect without asking again. Choose which ones may use this PC's mouse and keyboard."));
        if (_trusted.Any(t => t.AllowInput))
            p.Children.Add(Ui.Info("About remote control",
                "Windows doesn't let other apps type into administrator windows or UAC prompts, so those ignore remote input.",
                InfoBarSeverity.Informational));
        if (_trusted.Count == 0)
            p.Children.Add(Ui.Card("", "No paired devices yet", "Connect from another computer and enter the PIN this PC shows."));
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
            "How Annex starts. It lives in the system tray and keeps everything on your own network: no accounts, no cloud."));
        p.Children.Add(Ui.Card("", "Device name",
            "What other computers see. Leave it empty to use this PC's name.", Ui.Reuse(_nameBox)));
        p.Children.Add(Ui.Card("", "Start with Windows", "Annex starts in the tray when you sign in.",
            Ui.Switch(AppSettings.StartWithWindows, on => AppSettings.StartWithWindows = on)));
        p.Children.Add(Ui.Card("", "Start sharing when Annex opens", null,
            Ui.Switch(_settings.StartSharingAtLaunch, on => { _settings.StartSharingAtLaunch = on; _settings.Save(); })));
    }

    private void ViewingPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Viewing", "How other computers appear on this PC."));
        p.Children.Add(Ui.Card("", "Use as an extra display by default", null,
            Ui.Switch(_settings.ExtendByDefault, on => { _settings.ExtendByDefault = on; _extend = on; _settings.Save(); })));
        p.Children.Add(Ui.Card("", "Control the other computer",
            "Use this PC's mouse and keyboard on it. It must allow this PC in its Paired devices. Press Ctrl+Alt+Shift+Q to leave.",
            Ui.Switch(_settings.ControlWhenViewing, on => { _settings.ControlWhenViewing = on; _settings.Save(); })));
    }

    private void AboutPage(StackPanel p)
    {
        p.Children.Add(Ui.Hero("", "Annex", $"Version {AnnexNative.Version}. Use another computer as a second display, or mirror its screen. Encrypted end to end.",
            Ui.AppIcon(64)));
        p.Children.Add(Ui.Card("", "Website and source code", "github.com/NRanjan-17/Annex",
            new HyperlinkButton { Content = "Open", NavigateUri = new Uri("https://github.com/NRanjan-17/Annex") }));
        p.Children.Add(Ui.Card("", "Report an issue", null,
            new HyperlinkButton { Content = "Open", NavigateUri = new Uri("https://github.com/NRanjan-17/Annex/issues") }));
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
                _host = HostHandle.StartScreen(_core, _monitorIndex, DefaultPort, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)));
            }
            catch (AnnexException)
            {
                // Default port busy: take any free port (discovery advertises it).
                _host = HostHandle.StartScreen(_core, _monitorIndex, 0, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)));
            }
            if (_tray is not null) _tray.Tooltip = "Annex — sharing this screen";
        }
        catch (AnnexException ex)
        {
            _hostError = $"Could not start sharing: {ex.Message}";
        }
    }

    private void StopHosting()
    {
        var h = _host;
        _host = null;
        h?.Dispose();
        _sessions.Clear();
        _hostStream = null;
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
        try
        {
            _viewer = ViewerHandle.ConnectWindowed(_core, address, pinnedFingerprint, e => _ui.TryEnqueue(() => OnViewerEvent(e)),
                extend: _extend, input: _settings.ControlWhenViewing);
            _viewingName = title;
            _viewerStatus = $"Connecting to {title}…";
            if (_tray is not null) _tray.Tooltip = $"Annex — connecting to {title}";
        }
        catch (AnnexException ex)
        {
            _viewerStatus = $"Connect failed: {ex.Message}";
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
                _viewerStatus = $"Pairing with {f.PeerName} failed: the PIN didn't match or pairing was cancelled.";
                break;
            case CoreEvent.SessionStarted s:
                _viewerStatus = (s.Mode == "extend" ? $"Extra display for {s.PeerName}" : $"Viewing {s.PeerName}")
                    + (s.ConnectionDescription.Length > 0 ? $" ({s.ConnectionDescription})" : "");
                _viewingName = s.PeerName;
                _controlling = s.Input;
                if (_tray is not null) _tray.Tooltip = $"Annex — viewing {s.PeerName}";
                RefreshTrusted();
                break;
            case CoreEvent.InputAllowed a:
                _controlling = a.Allowed;
                break;
            case CoreEvent.StreamChanged(var st):
                _viewerStatus = $"Viewing {_viewingName} — {st.Width}×{st.Height} at {st.Fps} fps";
                break;
            case CoreEvent.Latency(var l):
                _latency = $"Latency {l.P50Ms:F0} ms (p95 {l.P95Ms:F0} ms) · {l.Fps:F0} fps";
                break;
            case CoreEvent.SessionEnded(_, var reason):
                _viewerStatus = reason == "user" ? null : $"Disconnected: {reason}";
                _latency = null;
                _controlling = false;
                if (_tray is not null) _tray.Tooltip = "Annex";
                Disconnect();
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
        _discovery?.Dispose();
        _discovery = null;
        _core?.Dispose();
        _core = null;
    }
}

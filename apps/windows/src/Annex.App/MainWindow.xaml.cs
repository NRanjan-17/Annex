using System.Collections.ObjectModel;
using Annex.Interop;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.Graphics;

namespace Annex.App;

/// <summary>
/// Windows app: discovers hosts and views them full screen. The stream is decoded and rendered
/// by the core in its own window (annex_viewer_connect_windowed). The app lives in the system
/// tray: closing the window hides it; "Quit" in the tray menu exits.
/// </summary>
public sealed partial class MainWindow : Window
{
    private readonly DispatcherQueue _ui;
    private readonly ObservableCollection<PeerInfo> _hosts = [];
    private readonly ObservableCollection<TrustedPeer> _trusted = [];
    private CoreClient? _core;
    private DiscoveryHandle? _discovery;
    private ViewerHandle? _viewer;
    private HostHandle? _host;
    private IReadOnlyList<MonitorInfo> _monitors = [];
    private ContentDialog? _pinDialog;
    private ulong _pinRequest;
    private TrayIcon? _tray;
    private bool _quitting;
    private string _viewingName = "";

    public static string DataDirectory =>
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Annex");

    public MainWindow()
    {
        InitializeComponent();
        _ui = DispatcherQueue.GetForCurrentThread();
        AppWindow.Resize(new SizeInt32(520, 720));
        HostsList.ItemsSource = _hosts;
        TrustedList.ItemsSource = _trusted;
        Closed += (_, _) => Shutdown();
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Annex.ico"));
        // Closing the window keeps Annex running in the tray.
        AppWindow.Closing += (_, args) =>
        {
            if (_quitting) return;
            args.Cancel = true;
            AppWindow.Hide();
        };
        _tray = new TrayIcon("Annex", Path.Combine(AppContext.BaseDirectory, "Assets", "Annex.ico"))
        {
            MenuProvider = BuildTrayMenu,
        };
        _tray.LeftClick += ShowFromTray;

        try
        {
            _core = CoreClient.Open(DataDirectory, Environment.MachineName);
            DeviceNameText.Text = _core.DeviceName;
            FingerprintText.Text = AnnexNative.DisplayFingerprint(_core.Fingerprint);
            VersionText.Text = $"Annex {AnnexNative.Version}";
            _discovery = new DiscoveryHandle(_core, e => _ui.TryEnqueue(() => OnDiscovery(e)));
            RefreshTrusted();
            RefreshMonitors();
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Could not start: {ex.Message}";
        }
    }

    private IReadOnlyList<TrayIcon.MenuItem?> BuildTrayMenu()
    {
        var items = new List<TrayIcon.MenuItem?> { new("Open Annex", ShowFromTray), null };
        items.Add(_host is null
            ? new("Share this screen", () => HostToggle.IsOn = true)
            : new("Stop sharing", () => HostToggle.IsOn = false));
        items.Add(null);
        if (_viewer is not null)
        {
            items.Add(new($"Viewing {_viewingName}", () => { }, Enabled: false));
            items.Add(new("Disconnect", Disconnect));
        }
        else if (_hosts.Count == 0)
        {
            items.Add(new("No hosts found", () => { }, Enabled: false));
        }
        else
        {
            foreach (var host in _hosts.Take(10))
                items.Add(new($"View {host.Name}", () => ConnectToPeer(host)));
        }
        items.Add(null);
        items.Add(new("Quit Annex", Quit));
        return items;
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
            HostStatusText.Text = $"Can't list displays: {ex.Message}";
            _monitors = [];
        }
        MonitorBox.ItemsSource = _monitors.Select(m => m.Label).ToList();
        MonitorBox.SelectedIndex = _monitors.Count > 0 ? 0 : -1;
    }

    private void HostToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (HostToggle.IsOn) StartHosting(); else StopHosting();
    }

    private void StartHosting()
    {
        if (_core is null || _host is not null) return;
        var index = Math.Max(0, MonitorBox.SelectedIndex);
        var hardware = HardwareBox.IsChecked == true;
        try
        {
            try
            {
                _host = HostHandle.StartScreen(_core, index, 47800, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)));
            }
            catch (AnnexException)
            {
                // Default port busy: take any free port (discovery advertises it).
                _host = HostHandle.StartScreen(_core, index, 0, advertise: true, hardware, ev => _ui.TryEnqueue(() => OnHostEvent(ev)));
            }
            HostStatusText.Text = $"Waiting for a viewer on port {_host.Port}…";
            MonitorBox.IsEnabled = HardwareBox.IsEnabled = false;
            if (_tray is not null) _tray.Tooltip = "Annex — sharing this screen";
        }
        catch (AnnexException ex)
        {
            HostStatusText.Text = $"Could not start sharing: {ex.Message}";
            HostToggle.IsOn = false;
        }
    }

    private void StopHosting()
    {
        var h = _host;
        _host = null;
        h?.Dispose();
        HostStatusText.Text = "";
        MonitorBox.IsEnabled = HardwareBox.IsEnabled = true;
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
                if (!f.Success) HostStatusText.Text = $"Pairing with {f.PeerName} failed.";
                RefreshTrusted();
                break;
            case CoreEvent.SessionStarted s:
                HostStatusText.Text = $"Streaming to {s.PeerName} — {s.Stream.Width}×{s.Stream.Height} @ {s.Stream.Fps} fps";
                RefreshTrusted();
                break;
            case CoreEvent.SessionEnded:
                if (_host is not null) HostStatusText.Text = $"Waiting for a viewer on port {_host.Port}…";
                break;
        }
    }

    private async Task ShowPin(CoreEvent.PairingPin pin)
    {
        if (_core is null) return;
        ShowFromTray();
        _pinRequest = pin.RequestId;
        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(new TextBlock { Text = $"Enter this PIN on {pin.PeerName}:", TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(new TextBlock
        {
            Text = pin.Pin,
            FontSize = 40,
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas"),
            HorizontalAlignment = HorizontalAlignment.Center,
            IsTextSelectionEnabled = true,
        });
        panel.Children.Add(new TextBlock { Text = $"Its identity code: {pin.FingerprintDisplay}" });
        _pinDialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"“{pin.PeerName}” wants to view this PC",
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
            HostStatusText.Text = $"PIN {pin.Pin} for {pin.PeerName} ({ex.Message})";
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
                var existing = _hosts.FirstOrDefault(h => h.Instance == peer.Instance);
                if (existing is not null) _hosts.Remove(existing);
                _hosts.Add(peer);
                break;
            case CoreEvent.PeerLost(var instance):
                var gone = _hosts.FirstOrDefault(h => h.Instance == instance);
                if (gone is not null) _hosts.Remove(gone);
                break;
        }
        NoHostsText.Visibility = _hosts.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void HostsList_ItemClick(object sender, ItemClickEventArgs e)
    {
        if (e.ClickedItem is PeerInfo peer) ConnectToPeer(peer);
    }

    private void ConnectToPeer(PeerInfo peer)
    {
        if (peer.ConnectAddress is not { } address) return;
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        var pinned = peer.Fingerprint is { } fp && _trusted.Any(t => t.Fingerprint == fp) ? fp : null;
        Connect(address, peer.Name, pinned);
    }

    private void Connect_Click(object sender, RoutedEventArgs e) => ConnectManual();

    private void AddressBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter) ConnectManual();
    }

    private void ConnectManual()
    {
        var address = AddressBox.Text.Trim();
        if (address.Length > 0) Connect(address, address, null);
    }

    private void Connect(string address, string title, string? pinnedFingerprint)
    {
        if (_core is null) return;
        Disconnect();
        try
        {
            _viewer = ViewerHandle.ConnectWindowed(_core, address, pinnedFingerprint, e => _ui.TryEnqueue(() => OnViewerEvent(e)),
                extend: ExtendToggle.IsOn);
            _viewingName = title;
            StatusText.Text = $"Connecting to {title}…";
            if (_tray is not null) _tray.Tooltip = $"Annex — connecting to {title}";
        }
        catch (AnnexException ex)
        {
            StatusText.Text = $"Connect failed: {ex.Message}";
        }
    }

    private async void OnViewerEvent(CoreEvent e)
    {
        switch (e)
        {
            case CoreEvent.PairingPinRequired req:
                await PromptForPin(req);
                break;
            case CoreEvent.PairingFinished { Success: false } f:
                StatusText.Text = $"Pairing with {f.PeerName} failed: the PIN didn't match or pairing was cancelled.";
                break;
            case CoreEvent.SessionStarted s:
                StatusText.Text = s.Mode == "extend" ? $"Extra display for {s.PeerName}" : $"Viewing {s.PeerName}";
                _viewingName = s.PeerName;
                if (_tray is not null) _tray.Tooltip = $"Annex — viewing {s.PeerName}";
                RefreshTrusted();
                break;
            case CoreEvent.StreamChanged(var st):
                StatusText.Text = $"Viewing — {st.Width}×{st.Height} @ {st.Fps} fps";
                break;
            case CoreEvent.Latency(var l):
                LatencyText.Text = $"Latency p50 {l.P50Ms:F0} ms · p95 {l.P95Ms:F0} ms · {l.Fps:F0} fps";
                break;
            case CoreEvent.SessionEnded(_, var reason):
                StatusText.Text = reason == "user" ? "Disconnected." : $"Disconnected: {reason}";
                LatencyText.Text = "";
                if (_tray is not null) _tray.Tooltip = "Annex";
                Disconnect();
                break;
        }
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
        panel.Children.Add(new TextBlock { Text = $"Enter the 6-digit PIN shown on {req.PeerName}.", TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(box);
        panel.Children.Add(new TextBlock
        {
            Text = $"Its identity code: {req.FingerprintDisplay}",
            Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
        });
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

    private void RemoveTrusted_Click(object sender, RoutedEventArgs e)
    {
        if (_core is null || TrustedList.SelectedItem is not TrustedPeer peer) return;
        _core.Untrust(peer.Fingerprint);
        RefreshTrusted();
    }

    private void RefreshTrusted()
    {
        if (_core is null) return;
        _trusted.Clear();
        foreach (var p in _core.TrustedPeers) _trusted.Add(p);
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
        StopHosting();
        Disconnect();
        _discovery?.Dispose();
        _discovery = null;
        _core?.Dispose();
        _core = null;
    }
}

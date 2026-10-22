using System.Collections.ObjectModel;
using Annex.Interop;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.Graphics;
using Windows.System;

namespace Annex.App;

/// <summary>
/// M1 Windows app: discovers hosts and views them full screen. The stream is decoded and
/// rendered by the core in its own window (annex_viewer_connect_windowed). Hosting from
/// Windows arrives in M3; the tray icon and settings UI in M6.
/// </summary>
public sealed partial class MainWindow : Window
{
    private readonly DispatcherQueue _ui;
    private readonly ObservableCollection<PeerInfo> _hosts = [];
    private readonly ObservableCollection<TrustedPeer> _trusted = [];
    private CoreClient? _core;
    private DiscoveryHandle? _discovery;
    private ViewerHandle? _viewer;

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

        try
        {
            _core = CoreClient.Open(DataDirectory, Environment.MachineName);
            DeviceNameText.Text = _core.DeviceName;
            FingerprintText.Text = AnnexNative.DisplayFingerprint(_core.Fingerprint);
            VersionText.Text = $"Annex {AnnexNative.Version}";
            _discovery = new DiscoveryHandle(_core, e => _ui.TryEnqueue(() => OnDiscovery(e)));
            RefreshTrusted();
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Could not start: {ex.Message}";
        }
    }

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
        if (e.ClickedItem is not PeerInfo peer || peer.ConnectAddress is not { } address) return;
        // Pin the certificate only for hosts we already trust; unknown hosts get a prompt.
        var pinned = peer.Fingerprint is { } fp && _trusted.Any(t => t.Fingerprint == fp) ? fp : null;
        Connect(address, peer.Name, pinned);
    }

    private void Connect_Click(object sender, RoutedEventArgs e) => ConnectManual();

    private void AddressBox_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter) ConnectManual();
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
            _viewer = ViewerHandle.ConnectWindowed(_core, address, pinnedFingerprint, e => _ui.TryEnqueue(() => OnViewerEvent(e)));
            StatusText.Text = $"Connecting to {title}…";
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
            case CoreEvent.TrustRequest req:
                await PromptTrust(req);
                break;
            case CoreEvent.SessionStarted s:
                StatusText.Text = $"Viewing {s.PeerName}";
                RefreshTrusted();
                break;
            case CoreEvent.StreamChanged(var st):
                StatusText.Text = $"Viewing — {st.Width}×{st.Height} @ {st.Fps} fps";
                break;
            case CoreEvent.Latency(var l):
                LatencyText.Text = $"Latency p50 {l.P50Ms:F0} ms · p95 {l.P95Ms:F0} ms · {l.Fps:F0} fps";
                break;
            case CoreEvent.SessionEnded(var reason):
                StatusText.Text = reason == "user" ? "Disconnected." : $"Disconnected: {reason}";
                LatencyText.Text = "";
                Disconnect();
                break;
        }
    }

    private async Task PromptTrust(CoreEvent.TrustRequest req)
    {
        if (_core is null) return;
        var dialog = new ContentDialog
        {
            XamlRoot = Content.XamlRoot,
            Title = $"Trust “{req.PeerName}”?",
            Content = $"Make sure the other device shows the same code before trusting it:\n\n{req.FingerprintDisplay}\n\n" +
                      $"This PC's own code is {FingerprintText.Text}.",
            PrimaryButtonText = "Trust",
            CloseButtonText = "Reject",
            DefaultButton = ContentDialogButton.Close,
        };
        var result = await dialog.ShowAsync();
        _core.RespondTrust(req.RequestId, result == ContentDialogResult.Primary);
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
        Disconnect();
        _discovery?.Dispose();
        _discovery = null;
        _core?.Dispose();
        _core = null;
    }
}

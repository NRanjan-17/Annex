using System.Runtime.InteropServices;
using Annex.Interop;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Annex.App;

/// <summary>
/// The tray menu: a small borderless acrylic window in Annex's black-and-white look (the same as the
/// Mac menu bar popover): this PC and its sharing switch, what's live, nearby computers with Extend
/// and View, then open/settings/quit. Opens at the cursor, above the taskbar, and hides when it loses
/// focus.
/// </summary>
internal sealed partial class TrayMenu : Window
{
    /// <summary>What the menu shows; rebuilt every time it opens. <c>Live</c>: the session's title and
    /// one detail line, when one is running.</summary>
    public sealed record Model(
        string DeviceName,
        string Status,
        bool Sharing,
        string? Viewing,
        IReadOnlyList<PeerInfo> Nearby,
        (string Title, string? Detail)? Live = null);

    public sealed record Actions(
        Action<bool> SetSharing,
        Action<PeerInfo, bool> Connect,
        Action Disconnect,
        Action Open,
        Action OpenSettings,
        Action Quit);

    private const double WidthDip = 340;
    private readonly PopupBehavior _popup;
    private readonly Grid _root = new();
    private Windows.Graphics.PointInt32 _anchor;

    public TrayMenu()
    {
        Title = "Annex menu";
        SystemBackdrop = new DesktopAcrylicBackdrop();
        // Black over the acrylic, like the main window.
        _root.RequestedTheme = ElementTheme.Dark;
        _root.Background = Ui.Black(0.72);
        Content = _root;
        PopupBehavior.ConfigureChrome(this);
        _popup = new PopupBehavior(this);
    }

    public void ShowAtCursor(Model model, Actions actions)
    {
        if (_popup.IsVisible)
        {
            _popup.Hide();
            return;
        }
        Build(model, actions);
        GetCursorPos(out var cursor);
        _anchor = new Windows.Graphics.PointInt32(cursor.X, cursor.Y);
        _popup.Show(Fit());
        // The first measurement can run before styles apply; fit again once rendered.
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => AppWindow.MoveAndResize(Fit()));
    }

    public void Hide() => _popup.Hide();

    /// <summary>Window bounds that fit the content, up and to the left of the cursor, inside the work area.</summary>
    private Windows.Graphics.RectInt32 Fit()
    {
        _root.Measure(new Windows.Foundation.Size(WidthDip, double.PositiveInfinity));
        var scale = _root.XamlRoot?.RasterizationScale ?? GetDpiForSystem() / 96.0;
        var w = (int)Math.Ceiling(WidthDip * scale);
        var h = (int)Math.Ceiling((_root.DesiredSize.Height + 4) * scale);
        var work = DisplayArea.GetFromPoint(_anchor, DisplayAreaFallback.Primary).WorkArea;
        var x = Math.Clamp(_anchor.X - w, work.X, work.X + work.Width - w);
        var y = Math.Clamp(_anchor.Y - h, work.Y, work.Y + work.Height - h);
        return new Windows.Graphics.RectInt32(x, y, w, h);
    }

    private void Build(Model m, Actions a)
    {
        var stack = new StackPanel { Padding = new Thickness(12, 14, 12, 10), Spacing = 10 };

        // Header: the app icon, this computer's name and what Annex is doing; Open and Settings.
        var header = new Grid { ColumnSpacing = 10, Padding = new Thickness(4, 0, 0, 2) };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var icon = Ui.AppIcon(36);
        icon.VerticalAlignment = VerticalAlignment.Center;
        header.Children.Add(icon);
        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 1 };
        var name = Ui.Text(m.DeviceName, wrap: false);
        name.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        text.Children.Add(name);
        text.Children.Add(Ui.Text(m.Status, "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        Grid.SetColumn(text, 1);
        header.Children.Add(text);
        var round = Ui.Row(Round("\uE8A7", "Open Annex", () => { Hide(); a.Open(); }),
            Round("\uE713", "Settings", () => { Hide(); a.OpenSettings(); }));
        round.Spacing = 6;
        round.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(round, 2);
        header.Children.Add(round);
        stack.Children.Add(header);

        // One switch: share this PC.
        var share = new Grid { ColumnSpacing = 10 };
        share.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        share.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var shareText = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        var shareTitle = Ui.Text("Share this PC", wrap: false);
        shareTitle.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        shareText.Children.Add(shareTitle);
        shareText.Children.Add(Ui.Text(m.Sharing ? "Nearby computers can see it." : "Other computers can't see it.",
            "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        share.Children.Add(shareText);
        var toggle = new ToggleSwitch { IsOn = m.Sharing, OnContent = "", OffContent = "", MinWidth = 0, VerticalAlignment = VerticalAlignment.Center };
        toggle.Toggled += (_, _) => { Hide(); a.SetSharing(toggle.IsOn); };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(toggle, "Share this PC");
        Grid.SetColumn(toggle, 1);
        share.Children.Add(toggle);
        stack.Children.Add(Ui.Panel(share, radius: 14, padding: new Thickness(12, 8, 12, 8)));

        // What's live, with the way out.
        if (m.Live is { } live)
        {
            var block = new StackPanel { Spacing = 6 };
            var title = Ui.Text(live.Title, wrap: false);
            title.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
            var titleRow = Ui.Row(Ui.LiveDot(7), title);
            block.Children.Add(titleRow);
            if (live.Detail is { } detail)
                block.Children.Add(Ui.Text(detail, "CaptionTextBlockStyle", Ui.Secondary));
            var stop = m.Viewing is not null
                ? Ui.Pill("Disconnect", () => { Hide(); a.Disconnect(); }, prominent: true)
                : Ui.Pill("Stop sharing", () => { Hide(); a.SetSharing(false); }, prominent: true);
            stop.HorizontalAlignment = HorizontalAlignment.Left;
            stop.Margin = new Thickness(0, 4, 0, 0);
            block.Children.Add(stop);
            var panel = Ui.Panel(block, radius: 14, fill: 0.08, padding: new Thickness(12));
            panel.BorderBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(90, 61, 209, 133));
            stack.Children.Add(panel);
        }

        // Nearby: computers that are sharing, each with Extend and View.
        var nearbyHead = new Grid { Padding = new Thickness(4, 6, 4, 0) };
        nearbyHead.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        nearbyHead.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        nearbyHead.Children.Add(Ui.Eyebrow("Nearby"));
        var looking = new ProgressRing { IsActive = true, Width = 12, Height = 12, Foreground = Ui.White(0.6) };
        Grid.SetColumn(looking, 1);
        nearbyHead.Children.Add(looking);
        stack.Children.Add(nearbyHead);
        var sharing = m.Nearby.Where(p => p.Sharing).ToList();
        var notSharing = m.Nearby.Count - sharing.Count;
        if (sharing.Count == 0)
        {
            var waiting = Ui.Text(notSharing > 0
                ? $"Waiting for a computer to share · {notSharing} not sharing"
                : "Waiting for a computer… Open Annex on it and turn on sharing.", "CaptionTextBlockStyle", Ui.Secondary);
            waiting.Margin = new Thickness(4, 0, 4, 4);
            stack.Children.Add(waiting);
        }
        else
        {
            var rows = new StackPanel { Spacing = 2 };
            foreach (var peer in sharing.Take(6))
                rows.Children.Add(PeerItem(peer, m.Viewing is null, a));
            stack.Children.Add(rows);
            if (notSharing > 0)
            {
                var more = Ui.Text($"+{notSharing} not sharing", "CaptionTextBlockStyle", Ui.Tertiary, wrap: false);
                more.Margin = new Thickness(4, 0, 0, 0);
                stack.Children.Add(more);
            }
        }

        stack.Children.Add(new Border { Height = 1, Background = Ui.White(0.08), Margin = new Thickness(-12, 2, -12, 0) });
        var footer = new Grid { Padding = new Thickness(4, 0, 0, 0) };
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var version = Ui.Text($"Annex {AnnexNative.Version}", "CaptionTextBlockStyle", Ui.Tertiary, wrap: false);
        version.VerticalAlignment = VerticalAlignment.Center;
        footer.Children.Add(version);
        var quitLabel = Ui.Text("Quit Annex", "CaptionTextBlockStyle", wrap: false);
        var quit = Ui.Flat(quitLabel, new SolidColorBrush(Microsoft.UI.Colors.Transparent), Ui.White(0.06), Ui.White(0.7),
            () => { Hide(); a.Quit(); }, radius: 8);
        quit.Padding = new Thickness(8, 4, 8, 4);
        Grid.SetColumn(quit, 1);
        footer.Children.Add(quit);
        stack.Children.Add(footer);

        _root.Children.Clear();
        _root.Children.Add(stack);
    }

    /// <summary>A computer that's sharing: its name and how it's reached, then Extend and View.</summary>
    private static Grid PeerItem(PeerInfo peer, bool enabled, Actions a)
    {
        var row = new Grid { Padding = new Thickness(4, 5, 0, 5), ColumnSpacing = 10 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var n = peer.Name.ToLowerInvariant();
        row.Children.Add(new Border
        {
            Width = 30,
            Height = 30,
            CornerRadius = new CornerRadius(8),
            Background = Ui.White(0.07),
            VerticalAlignment = VerticalAlignment.Center,
            Child = Ui.Glyph(n.Contains("book") || n.Contains("laptop") ? "\uE7F8" : "\uE7F4", 13),
        });
        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        var name = Ui.Text(peer.Name, wrap: false);
        name.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        text.Children.Add(name);
        text.Children.Add(Ui.Text(MainWindow.LinkKind(peer).Label, "CaptionTextBlockStyle", Ui.Secondary, wrap: false));
        Grid.SetColumn(text, 1);
        row.Children.Add(text);
        var extend = Ui.Pill("Extend", () => a.Connect(peer, true), prominent: true);
        var view = Ui.Pill("View", () => a.Connect(peer, false), prominent: false);
        ToolTipService.SetToolTip(extend, "This PC becomes a second screen for it.");
        ToolTipService.SetToolTip(view, "Show its screen here, in a window.");
        extend.IsEnabled = enabled && peer.Extend != false;
        view.IsEnabled = enabled;
        var buttons = Ui.Row(extend, view);
        buttons.Spacing = 6;
        buttons.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(buttons, 2);
        row.Children.Add(buttons);
        return row;
    }

    /// <summary>A small round icon button (Open Annex, Settings).</summary>
    private static Button Round(string glyph, string tip, Action click)
    {
        var b = Ui.Flat(Ui.Glyph(glyph, 13), Ui.White(0.07), Ui.White(0.13), Ui.White(0.8), click, radius: 15);
        b.Width = 30;
        b.Height = 30;
        b.Padding = new Thickness(0);
        b.HorizontalContentAlignment = HorizontalAlignment.Center;
        ToolTipService.SetToolTip(b, tip);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, tip);
        return b;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Point
    {
        public int X;
        public int Y;
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetCursorPos(out Point point);

    [LibraryImport("user32.dll")]
    private static partial uint GetDpiForSystem();
}

using System.Runtime.InteropServices;
using Annex.Interop;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace Annex.App;

/// <summary>
/// The tray right-click menu: a small borderless Acrylic window in the Windows 11 style (status
/// header, sharing, nearby computers, open/settings/quit) instead of the classic Win32 menu.
/// Opens at the cursor, above the taskbar, and hides when it loses focus.
/// </summary>
internal sealed partial class TrayMenu : Window
{
    /// <summary>What the menu shows; rebuilt every time it opens.</summary>
    public sealed record Model(
        string DeviceName,
        string Status,
        bool Sharing,
        string? Viewing,
        IReadOnlyList<PeerInfo> Nearby);

    public sealed record Actions(
        Action<bool> SetSharing,
        Action<PeerInfo, bool> Connect,
        Action Disconnect,
        Action Open,
        Action OpenSettings,
        Action Quit);

    private const double WidthDip = 320;
    private readonly PopupBehavior _popup;
    private readonly Grid _root = new();
    private Windows.Graphics.PointInt32 _anchor;

    public TrayMenu()
    {
        Title = "Annex menu";
        SystemBackdrop = new DesktopAcrylicBackdrop();
        // Disabled rows read as dimmed menu text, not as grey buttons.
        var clear = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        _root.Resources["ButtonBackgroundDisabled"] = clear;
        _root.Resources["ButtonBorderBrushDisabled"] = clear;
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
        var stack = new StackPanel { Padding = new Thickness(4), Spacing = 1 };

        // Header: icon, this computer's name and what Annex is doing.
        var header = new Grid { Padding = new Thickness(11, 8, 11, 6), ColumnSpacing = 10 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var icon = Ui.AppIcon(28);
        icon.VerticalAlignment = VerticalAlignment.Center;
        header.Children.Add(icon);
        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(Ui.Text(m.DeviceName, "BodyStrongTextBlockStyle", wrap: false));
        var status = Ui.Text(m.Status, "CaptionTextBlockStyle", Ui.Secondary, wrap: false);
        text.Children.Add(status);
        Grid.SetColumn(text, 1);
        header.Children.Add(text);
        stack.Children.Add(header);
        stack.Children.Add(new MenuFlyoutSeparator());

        stack.Children.Add(m.Sharing
            ? Item("", "Stop sharing", () => { Hide(); a.SetSharing(false); })
            : Item("", "Start sharing this screen", () => { Hide(); a.SetSharing(true); }));
        if (m.Viewing is { } viewing)
        {
            stack.Children.Add(Item("", viewing, null));
            stack.Children.Add(Item("", "Disconnect", () => { Hide(); a.Disconnect(); }));
        }

        stack.Children.Add(new MenuFlyoutSeparator());
        var nearby = Ui.Text("Nearby", "CaptionTextBlockStyle", Ui.Secondary, wrap: false);
        nearby.Margin = new Thickness(11, 4, 11, 2);
        stack.Children.Add(nearby);
        if (m.Nearby.Count == 0)
            stack.Children.Add(Item("", "Looking for computers…", null));
        foreach (var peer in m.Nearby.Take(6))
            stack.Children.Add(PeerItem(peer, m.Viewing is null, a));

        stack.Children.Add(new MenuFlyoutSeparator());
        stack.Children.Add(Item("", "Open Annex", () => { Hide(); a.Open(); }));
        stack.Children.Add(Item("", "Settings", () => { Hide(); a.OpenSettings(); }));
        stack.Children.Add(Item("", "Quit Annex", () => { Hide(); a.Quit(); }));

        _root.Children.Clear();
        _root.Children.Add(stack);
    }

    /// <summary>A menu row: icon and label. <paramref name="click"/> null = a dimmed, informational row.</summary>
    private static Button Item(string glyph, string label, Action? click)
    {
        var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        content.Children.Add(Ui.Glyph(glyph, 16));
        content.Children.Add(Ui.Text(label, wrap: false));
        var b = MenuButton(content);
        if (click is null) b.IsEnabled = false;
        else b.Click += (_, _) => click();
        return b;
    }

    /// <summary>A nearby computer with its two actions as compact icon buttons.</summary>
    private static Grid PeerItem(PeerInfo peer, bool enabled, Actions a)
    {
        var row = new Grid { Padding = new Thickness(11, 0, 4, 0), MinHeight = 36, ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var icon = Ui.Glyph("", 16);
        icon.VerticalAlignment = VerticalAlignment.Center;
        row.Children.Add(icon);
        var name = Ui.Text(peer.Name, wrap: false);
        name.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(name, 1);
        row.Children.Add(name);
        var show = IconButton("", "Show its screen here", () => a.Connect(peer, false));
        var extend = IconButton("", "Be its extra display", () => a.Connect(peer, true));
        show.IsEnabled = extend.IsEnabled = enabled;
        var buttons = Ui.Row(show, extend);
        buttons.Spacing = 2;
        Grid.SetColumn(buttons, 2);
        row.Children.Add(buttons);
        return row;
    }

    private static Button IconButton(string glyph, string tip, Action click)
    {
        var b = new Button
        {
            Content = Ui.Glyph(glyph, 14),
            Padding = new Thickness(8, 6, 8, 6),
            Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
            BorderThickness = new Thickness(0),
            CornerRadius = new CornerRadius(4),
        };
        ToolTipService.SetToolTip(b, tip);
        b.Click += (_, _) => click();
        return b;
    }

    private static Button MenuButton(UIElement content) => new()
    {
        Content = content,
        HorizontalAlignment = HorizontalAlignment.Stretch,
        HorizontalContentAlignment = HorizontalAlignment.Left,
        VerticalContentAlignment = VerticalAlignment.Center,
        Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent),
        BorderThickness = new Thickness(0),
        Padding = new Thickness(11, 0, 11, 0),
        MinHeight = 34,
        CornerRadius = new CornerRadius(4),
    };

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

using System.Diagnostics;
using Microsoft.UI;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.Graphics;
using Windows.Storage;

namespace Annex.App;

/// <summary>A black island hanging from the top of the screen, like the Mac's notch: while another
/// computer is connected, dragging files towards the top opens it, and dropping sends them. Files
/// arriving show there too. Windows can't draw a see-through window here, so the island is solid
/// black with a flowing silver edge and grows and shrinks with a spring.</summary>
internal sealed partial class DropTray
{
    private enum Phase { Hidden, Glowing, Open, Transfers }

    private sealed class Item(string key, bool incoming, string name, ulong size)
    {
        public string Key { get; } = key;
        public bool Incoming { get; } = incoming;
        public string Name { get; } = name;
        public ulong Size { get; } = size;
        public ulong Done;
        public bool? Ok;
        public string? Path;
        public string? Error;
    }

    /// <summary>Dropped files (folders already expanded into their files).</summary>
    public event Action<IReadOnlyList<string>>? Dropped;

    private readonly DispatcherQueue _ui;
    private readonly DispatcherQueueTimer _watch;
    private readonly List<Item> _items = [];
    private Window? _window;
    private Border? _island;
    private Grid? _content;
    private Phase _phase = Phase.Hidden;
    private string _peer = "";
    private bool _canSend;
    private bool _hovering;
    private DispatcherQueueTimer? _hideTimer;

    // Watching the mouse for a drag (Windows has no "a drag started" signal for other apps).
    private bool _buttonDown;
    private bool _dragging;
    private PointInt32 _downAt;
    private long _downTicks;
    private bool _fromExplorer;

    // The island's size in device-independent pixels, animated.
    private double _w, _h;
    private DispatcherQueueTimer? _sizeTimer;

    private const double PillWidth = 200, PillHeight = 34, TrayWidth = 380;
    /// <summary>The window sits this far above the screen so only its bottom corners are round.</summary>
    private const double Overhang = 14;

    public DropTray(DispatcherQueue ui)
    {
        _ui = ui;
        _watch = ui.CreateTimer();
        _watch.Interval = TimeSpan.FromMilliseconds(40);
        _watch.Tick += (_, _) => WatchMouse();
    }

    // ---- State from the app ------------------------------------------------------------

    /// <summary>Files can go to <paramref name="peer"/> now; null: nothing connected with control.</summary>
    public void SetTarget(string? peer)
    {
        if (peer is not null) _peer = peer;
        var can = peer is not null;
        if (can == _canSend) return;
        _canSend = can;
        if (can) _watch.Start();
        else
        {
            _watch.Stop();
            _dragging = _buttonDown = false;
            if (_phase is Phase.Glowing or Phase.Open) Hide();
        }
    }

    public void Started(ulong session, ulong id, bool incoming, string name, ulong size)
    {
        var key = Key(session, id, incoming);
        _items.RemoveAll(i => i.Key == key);
        _items.Add(new Item(key, incoming, name, size));
        while (_items.Count > 4 && _items.FindIndex(i => i.Ok is not null) is var i and >= 0) _items.RemoveAt(i);
        Show(Phase.Transfers);
    }

    public void Progress(ulong session, ulong id, bool incoming, ulong done)
    {
        if (Find(session, id, incoming) is { } item)
        {
            item.Done = done;
            if (_phase == Phase.Transfers) Render();
        }
    }

    public void Ended(ulong session, ulong id, bool incoming, bool ok, string? path, string? error)
    {
        if (Find(session, id, incoming) is not { } item) return;
        item.Ok = ok;
        item.Path = path;
        item.Error = error;
        if (ok) item.Done = item.Size;
        if (_phase == Phase.Transfers) Render();
        if (_items.TrueForAll(i => i.Ok is not null)) HideLater(5);
    }

    private Item? Find(ulong session, ulong id, bool incoming) => _items.Find(i => i.Key == Key(session, id, incoming));
    private static string Key(ulong session, ulong id, bool incoming) => $"{session}-{id}-{incoming}";

    // ---- Watching drags ----------------------------------------------------------------

    private void WatchMouse()
    {
        var down = (GetAsyncKeyState(VK_LBUTTON) & 0x8000) != 0;
        GetCursorPos(out var w);
        var p = new PointInt32(w.X, w.Y);
        if (down && !_buttonDown)
        {
            _buttonDown = true;
            _downAt = p;
            _downTicks = Environment.TickCount64;
            _fromExplorer = IsExplorerAt(p);
        }
        else if (!down && _buttonDown)
        {
            _buttonDown = false;
            if (_dragging)
            {
                _dragging = false;
                // A drop on the island arrives just after the button goes up.
                var t = _ui.CreateTimer();
                t.Interval = TimeSpan.FromMilliseconds(350);
                t.IsRepeating = false;
                t.Tick += (_, _) => { if (!_dragging && _phase is Phase.Glowing or Phase.Open) Hide(); };
                t.Start();
            }
            return;
        }
        if (!down) return;
        if (!_dragging)
        {
            var moved = Math.Abs(p.X - _downAt.X) + Math.Abs(p.Y - _downAt.Y);
            if (moved < 24 || Environment.TickCount64 - _downTicks < 250) return;
            _dragging = true;
            // Files nearly always come from File Explorer or the desktop; glow for those straight away.
            if (_fromExplorer && _phase == Phase.Hidden) Show(Phase.Glowing);
        }
        if (_phase == Phase.Transfers) return;
        var near = DistanceToIsland(p) < 160 * Scale;
        if (near && _phase != Phase.Open) Show(Phase.Open);
        else if (!near && _phase == Phase.Open)
        {
            if (_fromExplorer) Show(Phase.Glowing);
            else Hide();
        }
    }

    private static bool IsExplorerAt(PointInt32 p)
    {
        var hwnd = WindowFromPoint(new PointInt32Win { X = p.X, Y = p.Y });
        if (hwnd == 0 || GetWindowThreadProcessId(hwnd, out var pid) == 0) return false;
        try
        {
            using var proc = Process.GetProcessById((int)pid);
            return proc.ProcessName.Equals("explorer", StringComparison.OrdinalIgnoreCase);
        }
        catch (ArgumentException)
        {
            return false;
        }
    }

    private double DistanceToIsland(PointInt32 p)
    {
        var area = DisplayArea.Primary.OuterBounds;
        var half = PillWidth / 2 * Scale;
        var cx = area.X + area.Width / 2.0;
        var dx = Math.Max(Math.Abs(p.X - cx) - half, 0);
        var dy = Math.Max(p.Y - area.Y - PillHeight * Scale, 0);
        return Math.Sqrt(dx * dx + dy * dy);
    }

    // ---- The window ----------------------------------------------------------------------

    private double Scale => _window is null ? GetDpiForSystem() / 96.0 : GetDpiForWindow(WinRT.Interop.WindowNative.GetWindowHandle(_window)) / 96.0;

    private void Ensure()
    {
        if (_window is not null) return;
        var window = new Window { Title = "Annex" };
        var root = new Grid { AllowDrop = true, Background = new SolidColorBrush(Colors.Black) };
        root.DragOver += (_, e) =>
        {
            if (!e.DataView.Contains(StandardDataFormats.StorageItems)) return;
            e.AcceptedOperation = DataPackageOperation.Copy;
            e.DragUIOverride.Caption = $"Send to {_peer}";
            if (_phase is Phase.Hidden or Phase.Glowing) Show(Phase.Open);
        };
        root.Drop += async (_, e) =>
        {
            if (!e.DataView.Contains(StandardDataFormats.StorageItems)) return;
            var deferral = e.GetDeferral();
            try
            {
                var items = await e.DataView.GetStorageItemsAsync();
                var files = items.SelectMany(i => i is StorageFolder ? FilesIn(i.Path) : [i.Path]).ToList();
                _dragging = false;
                if (files.Count == 0) { Hide(); return; }
                Show(Phase.Transfers);
                Dropped?.Invoke(files);
            }
            finally
            {
                deferral.Complete();
            }
        };
        root.PointerEntered += (_, _) => _hovering = true;
        root.PointerExited += (_, _) =>
        {
            _hovering = false;
            if (_phase == Phase.Transfers && _items.TrueForAll(i => i.Ok is not null)) HideLater(2);
        };

        // The light: silver flowing round the edge.
        var turn = new RotateTransform { CenterX = 0.5, CenterY = 0.5 };
        var silver = new LinearGradientBrush
        {
            StartPoint = new Point(0, 0),
            EndPoint = new Point(1, 1),
            RelativeTransform = turn,
            GradientStops =
            {
                new GradientStop { Color = Grey(0.62), Offset = 0 },
                new GradientStop { Color = Colors.White, Offset = 0.2 },
                new GradientStop { Color = Grey(0.84), Offset = 0.4 },
                new GradientStop { Color = Colors.White, Offset = 0.6 },
                new GradientStop { Color = Grey(0.7), Offset = 0.8 },
                new GradientStop { Color = Colors.White, Offset = 1 },
            },
        };
        var spin = new DoubleAnimation { From = 0, To = 360, Duration = TimeSpan.FromSeconds(2.4), RepeatBehavior = RepeatBehavior.Forever, EnableDependentAnimation = true };
        Storyboard.SetTarget(spin, turn);
        Storyboard.SetTargetProperty(spin, "Angle");
        var story = new Storyboard();
        story.Children.Add(spin);

        _content = new Grid { Padding = new Thickness(18, Overhang + 10, 18, 12), Opacity = 0 };
        _island = new Border
        {
            BorderBrush = silver,
            BorderThickness = new Thickness(2.5, 0, 2.5, 2.5),
            CornerRadius = new CornerRadius(0, 0, 20, 20),
            Child = _content,
        };
        root.Children.Add(_island);
        window.Content = root;

        var app = window.AppWindow;
        if (app.Presenter is OverlappedPresenter p)
        {
            p.SetBorderAndTitleBar(false, false);
            p.IsResizable = false;
            p.IsMaximizable = false;
            p.IsMinimizable = false;
            p.IsAlwaysOnTop = true;
        }
        app.IsShownInSwitchers = false;
        _window = window;
        root.Loaded += (_, _) => story.Begin();
        _w = PillWidth * 0.8;
        _h = PillHeight * 0.5;
    }

    private static Windows.UI.Color Grey(double v) => Windows.UI.Color.FromArgb(255, (byte)(v * 255), (byte)(v * 255), (byte)(v * 255));

    private (double W, double H) Target(Phase phase) => phase switch
    {
        Phase.Glowing => (PillWidth, PillHeight),
        Phase.Open => (TrayWidth, 150),
        Phase.Transfers => (TrayWidth, 30 + Math.Max(_items.Count, 1) * 46),
        _ => (PillWidth * 0.8, PillHeight * 0.5),
    };

    private void Show(Phase phase)
    {
        _hideTimer?.Stop();
        Ensure();
        var was = _phase;
        _phase = phase;
        if (was == Phase.Hidden)
        {
            Place(_w, _h);
            _window!.AppWindow.Show(false);
        }
        _island!.BorderBrush.Opacity = phase is Phase.Glowing or Phase.Open ? 1 : 0;
        Render();
        var (w, h) = Target(phase);
        // Pops out with a little overshoot.
        AnimateTo(w, h, 420, overshoot: true);
        Fade(_content!, phase is Phase.Open or Phase.Transfers ? 1 : 0, 220);
    }

    private void Hide()
    {
        if (_phase == Phase.Hidden || _window is null) return;
        _hideTimer?.Stop();
        _phase = Phase.Hidden;
        Fade(_content!, 0, 120);
        // Pops back in, quicker and without the bounce.
        AnimateTo(PillWidth * 0.8, PillHeight * 0.5, 260, overshoot: false, done: () =>
        {
            if (_phase != Phase.Hidden) return;
            _window.AppWindow.Hide();
            _items.RemoveAll(i => i.Ok is not null);
        });
    }

    private void HideLater(double seconds)
    {
        _hideTimer ??= _ui.CreateTimer();
        _hideTimer.Stop();
        _hideTimer.Interval = TimeSpan.FromSeconds(seconds);
        _hideTimer.IsRepeating = false;
        _hideTimer.Tick -= OnHideTimer;
        _hideTimer.Tick += OnHideTimer;
        _hideTimer.Start();
    }

    private void OnHideTimer(DispatcherQueueTimer t, object _)
    {
        if (_phase == Phase.Transfers && !_hovering && !_dragging) Hide();
    }

    /// <summary>Resizes the window round the top centre, frame by frame (Windows can't animate a window's size itself).</summary>
    private void AnimateTo(double w, double h, double ms, bool overshoot, Action? done = null)
    {
        _sizeTimer?.Stop();
        var (w0, h0) = (_w, _h);
        var start = Environment.TickCount64;
        var t = _ui.CreateTimer();
        t.Interval = TimeSpan.FromMilliseconds(8);
        t.Tick += (_, _) =>
        {
            var x = Math.Min((Environment.TickCount64 - start) / ms, 1);
            var e = overshoot ? EaseOutBack(x) : EaseOutCubic(x);
            _w = w0 + (w - w0) * e;
            _h = h0 + (h - h0) * e;
            Place(_w, _h);
            if (x >= 1)
            {
                t.Stop();
                done?.Invoke();
            }
        };
        _sizeTimer = t;
        t.Start();
    }

    private static double EaseOutBack(double x)
    {
        const double c1 = 1.4, c3 = c1 + 1;
        return 1 + c3 * Math.Pow(x - 1, 3) + c1 * Math.Pow(x - 1, 2);
    }

    private static double EaseOutCubic(double x) => 1 - Math.Pow(1 - x, 3);

    private void Place(double w, double h)
    {
        var area = DisplayArea.Primary.OuterBounds;
        var s = Scale;
        var pw = (int)Math.Round(w * s);
        var ph = (int)Math.Round((h + Overhang) * s);
        _window!.AppWindow.MoveAndResize(new RectInt32(area.X + (area.Width - pw) / 2, area.Y - (int)(Overhang * s), pw, ph));
    }

    private static void Fade(UIElement element, double to, double ms)
    {
        var a = new DoubleAnimation { To = to, Duration = TimeSpan.FromMilliseconds(ms) };
        Storyboard.SetTarget(a, element);
        Storyboard.SetTargetProperty(a, "Opacity");
        var s = new Storyboard();
        s.Children.Add(a);
        s.Begin();
    }

    // ---- What's inside -------------------------------------------------------------------

    private void Render()
    {
        if (_content is null) return;
        _content.Children.Clear();
        if (_phase == Phase.Open)
        {
            var stack = new StackPanel { Spacing = 6, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
            stack.Children.Add(new FontIcon { Glyph = "", FontSize = 28, Foreground = Ui.White(1) });
            stack.Children.Add(new TextBlock
            {
                Text = $"Drop to send to {_peer}", FontSize = 14, FontWeight = FontWeights.SemiBold,
                Foreground = Ui.White(1), HorizontalAlignment = HorizontalAlignment.Center,
            });
            stack.Children.Add(new TextBlock
            {
                Text = "Same quality, byte for byte", FontSize = 12, Foreground = Ui.White(0.55),
                HorizontalAlignment = HorizontalAlignment.Center,
            });
            _content.Children.Add(stack);
        }
        else if (_phase == Phase.Transfers)
        {
            var list = new StackPanel { Spacing = 8 };
            foreach (var item in _items) list.Children.Add(Row(item));
            _content.Children.Add(list);
        }
    }

    private static readonly SolidColorBrush Live = new(Windows.UI.Color.FromArgb(255, 61, 209, 133));

    private Grid Row(Item item)
    {
        var row = new Grid { ColumnSpacing = 10, Height = 38 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        UIElement badge = item.Ok switch
        {
            true => new FontIcon { Glyph = "", FontSize = 16, Foreground = Live },
            false => new FontIcon { Glyph = "", FontSize = 14, Foreground = Ui.White(0.5) },
            null => new ProgressRing
            {
                IsIndeterminate = false, Width = 26, Height = 26, Foreground = Live,
                Value = item.Size == 0 ? 100 : Math.Min(100.0 * item.Done / item.Size, 100),
            },
        };
        var badgeBox = new Grid { Width = 28, Height = 28 };
        badgeBox.Children.Add(badge);
        row.Children.Add(badgeBox);

        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(new TextBlock
        {
            Text = item.Name, FontSize = 12.5, FontWeight = FontWeights.SemiBold, Foreground = Ui.White(1),
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        text.Children.Add(new TextBlock { Text = Status(item), FontSize = 11.5, Foreground = Ui.White(0.6), TextTrimming = TextTrimming.CharacterEllipsis });
        Grid.SetColumn(text, 1);
        row.Children.Add(text);

        if (item is { Incoming: true, Ok: true, Path: { } path })
        {
            var show = Ui.Flat(new TextBlock { Text = "Show", FontSize = 11.5, FontWeight = FontWeights.SemiBold },
                Ui.White(0.15), Ui.White(0.25), Ui.White(1), () => Process.Start("explorer.exe", $"/select,\"{path}\""), radius: 12);
            show.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(show, 2);
            row.Children.Add(show);
        }
        return row;
    }

    private string Status(Item item)
    {
        var total = Size(item.Size);
        return item.Ok switch
        {
            true => item.Incoming ? $"Received from {_peer} · {total}" : $"Sent to {_peer} · {total}",
            false => item.Error ?? "Didn't arrive",
            null => $"{(item.Incoming ? "Receiving" : "Sending")} {Size(item.Done)} of {total}",
        };
    }

    private static string Size(ulong bytes) => bytes switch
    {
        < 1_000 => $"{bytes} bytes",
        < 1_000_000 => $"{bytes / 1_000.0:0} KB",
        < 1_000_000_000 => $"{bytes / 1_000_000.0:0.#} MB",
        _ => $"{bytes / 1_000_000_000.0:0.##} GB",
    };

    /// <summary>A file, or every file inside a folder (hidden and system files skipped).</summary>
    private static IEnumerable<string> FilesIn(string folder)
    {
        try
        {
            return new DirectoryInfo(folder)
                .EnumerateFiles("*", new EnumerationOptions { RecurseSubdirectories = true, AttributesToSkip = System.IO.FileAttributes.Hidden | System.IO.FileAttributes.System })
                .Select(f => f.FullName).ToList();
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            return [];
        }
    }

    // ---- Win32 ---------------------------------------------------------------------------

    private const int VK_LBUTTON = 0x01;

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial short GetAsyncKeyState(int key);

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    private static partial bool GetCursorPos(out PointInt32Win point);

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial nint WindowFromPoint(PointInt32Win point);

    /// <summary>Win32 POINT.</summary>
    private struct PointInt32Win { public int X, Y; }

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial uint GetWindowThreadProcessId(nint hwnd, out uint processId);

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial uint GetDpiForWindow(nint hwnd);

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial uint GetDpiForSystem();
}

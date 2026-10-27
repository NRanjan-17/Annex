using System.Diagnostics;
using Annex.Interop;
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

    private sealed class Item(string key, bool viewer, ulong session, ulong id, bool incoming, string name, ulong size)
    {
        public string Key { get; } = key;
        /// <summary>The transfer belongs to the viewer (this PC watching another), not a host session.</summary>
        public bool Viewer { get; } = viewer;
        public ulong Session { get; } = session;
        public ulong Id { get; } = id;
        public bool Incoming { get; } = incoming;
        public string Name { get; } = name;
        public ulong Size { get; } = size;
        public ulong Done;
        public bool? Ok;
        public string? Path;
        public string? Error;
        public bool Paused;
        /// <summary>Unsend (going) or Stop (arriving) was pressed.</summary>
        public bool Cancelled;
    }

    /// <summary>Dropped files (folders already expanded into their files).</summary>
    public event Action<IReadOnlyList<string>>? Dropped;

    /// <summary>Pause/Resume pressed on a file being sent: (viewer, session, id, paused).</summary>
    public event Action<bool, ulong, ulong, bool>? PauseRequested;

    /// <summary>Unsend or Stop pressed: (viewer, session, id, incoming).</summary>
    public event Action<bool, ulong, ulong, bool>? CancelRequested;

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
    private bool _fileDrag;

    // The island's size in device-independent pixels, animated.
    private double _w, _h;
    private DispatcherQueueTimer? _sizeTimer;

    private const double PillWidth = 200, PillHeight = 34, TrayWidth = 380;
    /// <summary>The window sits this far above the screen so only its bottom corners are round.</summary>
    private const double Overhang = 14;
    private const double Radius = 20;

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
        AnnexLog.Info(can ? $"[drop] watching drags: files can go to {peer}" : "[drop] stopped watching drags: nothing connected with control");
        if (can) _watch.Start();
        else
        {
            _watch.Stop();
            _dragging = _buttonDown = false;
            if (_phase is Phase.Glowing or Phase.Open) Hide("nothing to send to");
        }
    }

    /// <summary><paramref name="viewer"/>: the transfer is the viewer's rather than a host session's.</summary>
    public void Started(bool viewer, ulong session, ulong id, bool incoming, string name, ulong size)
    {
        var key = Key(viewer, session, id, incoming);
        _items.RemoveAll(i => i.Key == key);
        _items.Add(new Item(key, viewer, session, id, incoming, name, size));
        while (_items.Count > 4 && _items.FindIndex(i => i.Ok is not null) is var i and >= 0) _items.RemoveAt(i);
        Show(Phase.Transfers, incoming ? "a file is arriving" : "a file is going");
    }

    public void Progress(bool viewer, ulong session, ulong id, bool incoming, ulong done)
    {
        if (Find(viewer, session, id, incoming) is { } item)
        {
            item.Done = done;
            if (_phase == Phase.Transfers) Render();
        }
    }

    public void Ended(bool viewer, ulong session, ulong id, bool incoming, bool ok, string? path, string? error)
    {
        if (Find(viewer, session, id, incoming) is not { } item) return;
        item.Ok = ok;
        item.Paused = false;
        item.Path = path;
        item.Error = error;
        if (ok) item.Done = item.Size;
        if (_phase == Phase.Transfers) Render();
        if (AllDone) HideLater(5);
    }

    private Item? Find(bool viewer, ulong session, ulong id, bool incoming) => _items.Find(i => i.Key == Key(viewer, session, id, incoming));
    private static string Key(bool viewer, ulong session, ulong id, bool incoming) => $"{(viewer ? "v" : "h")}-{session}-{id}-{incoming}";

    /// <summary>Nothing left moving or paused: the island may tuck itself away.</summary>
    private bool AllDone => _items.TrueForAll(i => i.Ok is not null);

    private void TogglePause(Item item)
    {
        if (item.Ok is not null) return;
        item.Paused = !item.Paused;
        AnnexLog.Info($"[drop] {(item.Paused ? "pause" : "resume")} pressed for {item.Name} (file {item.Id}, {(item.Viewer ? "viewer" : $"session {item.Session}")})");
        if (item.Paused) _hideTimer?.Stop();
        PauseRequested?.Invoke(item.Viewer, item.Session, item.Id, item.Paused);
        if (_phase == Phase.Transfers) Render();
    }

    private void Cancel(Item item)
    {
        if (item.Ok is not null) return;
        AnnexLog.Info($"[drop] {(item.Incoming ? "stop" : "unsend")} pressed for {item.Name} (file {item.Id}, {(item.Viewer ? "viewer" : $"session {item.Session}")})");
        item.Cancelled = true;
        item.Paused = false;
        item.Ok = false;
        CancelRequested?.Invoke(item.Viewer, item.Session, item.Id, item.Incoming);
        if (_phase == Phase.Transfers) Render();
        if (AllDone) HideLater(5);
    }

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
                t.Tick += (_, _) => { if (!_dragging && _phase is Phase.Glowing or Phase.Open) Hide("drag ended elsewhere"); };
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
            _fileDrag = false;
            if (AnnexLog.Verbose) AnnexLog.Trace($"[drop] drag started (from Explorer {_fromExplorer})");
        }
        // Pop out for files and folders from any app: Explorer and the desktop, or anything showing the
        // shell's drag image.
        if (!_fileDrag && (_fromExplorer || DragImageShown()))
        {
            _fileDrag = true;
            if (_phase == Phase.Hidden) Show(Phase.Glowing, _fromExplorer ? "file drag from Explorer" : "file drag (SysDragImage shown)");
        }
        if (_phase == Phase.Transfers) return;
        var distance = DistanceToIsland(p);
        var near = distance < 160 * Scale;
        if (near && _phase != Phase.Open) Show(Phase.Open, $"drag near the island ({distance:0} px)");
        else if (!near && _phase == Phase.Open)
        {
            if (_fileDrag) Show(Phase.Glowing, $"drag moved away ({distance:0} px)");
            else Hide($"drag moved away ({distance:0} px), not a file drag");
        }
    }

    private static bool DragImageShown()
    {
        var hwnd = FindWindowW("SysDragImage", null);
        return hwnd != 0 && IsWindowVisible(hwnd);
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
            if (_phase is Phase.Hidden or Phase.Glowing) Show(Phase.Open, "files dragged over the island");
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
                if (files.Count == 0)
                {
                    AnnexLog.Info($"[drop] dropped {items.Count} item(s) with no files in them");
                    Hide("dropped nothing to send");
                    return;
                }
                LogDropped(files);
                Show(Phase.Transfers, "files dropped");
                Dropped?.Invoke(files);
            }
            catch (Exception ex)
            {
                AnnexLog.Error($"[drop] couldn't take the dropped files: {ex.GetType().Name}: {ex.Message}");
                Hide("drop failed");
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
            if (_phase == Phase.Transfers && AllDone) HideLater(2);
        };
        window.Closed += (_, _) => AnnexLog.Debug("[drop] island window closed");

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
            CornerRadius = new CornerRadius(0, 0, Radius, Radius),
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

    /// <summary><paramref name="why"/>: what triggered it, for the log.</summary>
    private void Show(Phase phase, string why)
    {
        _hideTimer?.Stop();
        Ensure();
        var was = _phase;
        _phase = phase;
        if (was != phase) AnnexLog.Debug($"[drop] {was} -> {phase}: {why}");
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

    private void Hide(string why)
    {
        if (_phase == Phase.Hidden || _window is null) return;
        AnnexLog.Debug($"[drop] {_phase} -> Hidden: {why}");
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
        // A paused file is still unfinished: the island stays so it can be resumed.
        if (_phase == Phase.Transfers && !_hovering && !_dragging && AllDone) Hide("transfers finished");
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
        // Round the bottom corners like the silver edge (the top ones are above the screen).
        var r = (int)Math.Round(Radius * 2 * s);
        var region = CreateRoundRectRgn(0, -r, pw + 1, ph + 1, r, r);
        if (region != 0 && SetWindowRgn(WinRT.Interop.WindowNative.GetWindowHandle(_window), region, true) == 0) DeleteObject(region);
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
                Opacity = item.Paused ? 0.35 : 1,
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
                Ui.White(0.15), Ui.White(0.25), Ui.White(1), () =>
                {
                    AnnexLog.Debug($"[drop] show {path} in Explorer");
                    Process.Start("explorer.exe", $"/select,\"{path}\"");
                }, radius: 12);
            show.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(show, 2);
            row.Children.Add(show);
        }
        else if (item.Ok is null)
        {
            // Small round buttons for a file still on its way.
            var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
            if (!item.Incoming)
            {
                buttons.Children.Add(IconButton(item.Paused ? "\uE768" : "\uE769", item.Paused ? "Resume" : "Pause", () => TogglePause(item)));
                buttons.Children.Add(IconButton("\uE7A7", "Unsend", () => Cancel(item)));
            }
            else buttons.Children.Add(IconButton("\uE711", "Stop", () => Cancel(item)));
            Grid.SetColumn(buttons, 2);
            row.Children.Add(buttons);
        }
        return row;
    }

    /// <summary>A round icon button in the island's style (like Show).</summary>
    private static Button IconButton(string glyph, string label, Action click)
    {
        var b = Ui.Flat(new FontIcon { Glyph = glyph, FontSize = 11 }, Ui.White(0.15), Ui.White(0.25), Ui.White(1), click, radius: 13);
        b.Width = 26;
        b.Height = 26;
        b.Padding = new Thickness(0);
        b.HorizontalContentAlignment = HorizontalAlignment.Center;
        b.VerticalContentAlignment = VerticalAlignment.Center;
        ToolTipService.SetToolTip(b, label);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(b, label);
        return b;
    }

    private string Status(Item item)
    {
        var total = Size(item.Size);
        return item.Ok switch
        {
            true => item.Incoming ? $"Received from {_peer} · {total}" : $"Sent to {_peer} · {total}",
            false when item.Cancelled => item.Incoming ? "Stopped" : "Unsent",
            false => item.Error ?? "Didn't arrive",
            null when item.Paused => $"Paused · {Size(item.Done)} of {total}",
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
            AnnexLog.Warn($"[drop] couldn't list the files in {folder}: {e.Message}");
            return [];
        }
    }

    /// <summary>How many files, how big, and their names (sizes only where they can be read).</summary>
    private static void LogDropped(IReadOnlyList<string> files)
    {
        long total = 0;
        foreach (var f in files)
        {
            try { total += new FileInfo(f).Length; }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException) { }
        }
        var names = string.Join(", ", files.Take(10).Select(Path.GetFileName));
        AnnexLog.Info($"[drop] dropped {files.Count} file(s), {total} bytes: {names}{(files.Count > 10 ? ", …" : "")}");
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

    [System.Runtime.InteropServices.LibraryImport("gdi32.dll")]
    private static partial nint CreateRoundRectRgn(int left, int top, int right, int bottom, int widthEllipse, int heightEllipse);

    [System.Runtime.InteropServices.LibraryImport("gdi32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    private static partial bool DeleteObject(nint obj);

    /// <summary>Windows owns <paramref name="region"/> once this succeeds.</summary>
    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    private static partial int SetWindowRgn(nint hwnd, nint region, [System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)] bool redraw);

    [System.Runtime.InteropServices.LibraryImport("user32.dll", StringMarshalling = System.Runtime.InteropServices.StringMarshalling.Utf16)]
    private static partial nint FindWindowW(string className, string? windowName);

    [System.Runtime.InteropServices.LibraryImport("user32.dll")]
    [return: System.Runtime.InteropServices.MarshalAs(System.Runtime.InteropServices.UnmanagedType.Bool)]
    private static partial bool IsWindowVisible(nint hwnd);
}

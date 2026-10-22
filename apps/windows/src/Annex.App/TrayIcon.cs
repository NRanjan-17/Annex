using System.Runtime.InteropServices;

namespace Annex.App;

/// <summary>
/// Notification-area (system tray) icon built on Shell_NotifyIcon. WinUI 3 has no tray API, so
/// this owns a hidden Win32 window on the UI thread (the WinUI dispatcher pumps its messages).
/// Left click opens the main window; right click shows the menu.
/// </summary>
internal sealed unsafe partial class TrayIcon : IDisposable
{
    public sealed record MenuItem(string Text, Action OnClick, bool Enabled = true);

    private const uint WM_APP_TRAY = 0x8000 + 1;
    private const uint WM_LBUTTONUP = 0x0202;
    private const uint WM_RBUTTONUP = 0x0205;
    private const uint WM_CONTEXTMENU = 0x007B;
    private const uint WM_NULL = 0x0000;
    private const uint NIM_ADD = 0, NIM_MODIFY = 1, NIM_DELETE = 2;
    private const uint NIF_MESSAGE = 0x1, NIF_ICON = 0x2, NIF_TIP = 0x4, NIF_SHOWTIP = 0x80;
    private const uint MF_STRING = 0x0, MF_GRAYED = 0x1, MF_SEPARATOR = 0x800;
    private const uint TPM_RETURNCMD = 0x100, TPM_RIGHTBUTTON = 0x2;
    private const uint IMAGE_ICON = 1, LR_LOADFROMFILE = 0x10, LR_DEFAULTSIZE = 0x40;

    private static TrayIcon? s_instance;
    private static uint s_taskbarCreated;

    private readonly nint _hwnd;
    private readonly nint _icon;
    private string _tooltip;

    /// <summary>Builds the right-click menu each time it opens (null entries are separators).</summary>
    public Func<IReadOnlyList<MenuItem?>> MenuProvider { get; set; } = () => [];
    public event Action? LeftClick;

    public TrayIcon(string tooltip, string iconPath)
    {
        if (s_instance is not null) throw new InvalidOperationException("only one tray icon is supported");
        s_instance = this;
        _tooltip = tooltip;
        var hinst = GetModuleHandleW(null);
        fixed (char* cls = "AnnexTrayWindow")
        {
            var wc = new WNDCLASSEXW
            {
                cbSize = (uint)sizeof(WNDCLASSEXW),
                lpfnWndProc = (nint)(delegate* unmanaged<nint, uint, nint, nint, nint>)&WndProc,
                hInstance = hinst,
                lpszClassName = cls,
            };
            RegisterClassExW(&wc);
            // A real (never shown) top-level window: menus need a foreground-capable owner.
            _hwnd = CreateWindowExW(0, cls, cls, 0, 0, 0, 0, 0, 0, 0, hinst, 0);
        }
        s_taskbarCreated = RegisterWindowMessageW("TaskbarCreated");
        _icon = LoadImageW(0, iconPath, IMAGE_ICON, 0, 0, LR_LOADFROMFILE | LR_DEFAULTSIZE);
        Add();
    }

    public string Tooltip
    {
        get => _tooltip;
        set
        {
            _tooltip = value;
            var data = Data(NIF_TIP | NIF_SHOWTIP);
            Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    private NOTIFYICONDATAW Data(uint flags)
    {
        var d = new NOTIFYICONDATAW { cbSize = (uint)sizeof(NOTIFYICONDATAW), hWnd = _hwnd, uID = 1, uFlags = flags };
        d.uCallbackMessage = WM_APP_TRAY;
        d.hIcon = _icon;
        var tip = _tooltip.Length > 127 ? _tooltip[..127] : _tooltip;
        tip.AsSpan().CopyTo(new Span<char>(d.szTip, 128));
        return d;
    }

    private void Add()
    {
        var data = Data(NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP);
        Shell_NotifyIconW(NIM_ADD, &data);
    }

    private void ShowMenu()
    {
        var items = MenuProvider();
        var menu = CreatePopupMenu();
        try
        {
            for (var i = 0; i < items.Count; i++)
            {
                if (items[i] is { } item)
                    AppendMenuW(menu, MF_STRING | (item.Enabled ? 0 : MF_GRAYED), (nuint)(i + 1), item.Text);
                else
                    AppendMenuW(menu, MF_SEPARATOR, 0, null);
            }
            GetCursorPos(out var pt);
            SetForegroundWindow(_hwnd);
            var cmd = TrackPopupMenuEx(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.X, pt.Y, _hwnd, 0);
            PostMessageW(_hwnd, WM_NULL, 0, 0);
            if (cmd > 0 && cmd <= items.Count && items[cmd - 1] is { Enabled: true } chosen) chosen.OnClick();
        }
        finally
        {
            DestroyMenu(menu);
        }
    }

    [UnmanagedCallersOnly]
    private static nint WndProc(nint hwnd, uint msg, nint wParam, nint lParam)
    {
        var self = s_instance;
        try
        {
            if (self is not null && msg == WM_APP_TRAY)
            {
                switch ((uint)(lParam & 0xFFFF))
                {
                    case WM_LBUTTONUP: self.LeftClick?.Invoke(); break;
                    case WM_RBUTTONUP or WM_CONTEXTMENU: self.ShowMenu(); break;
                }
                return 0;
            }
            if (self is not null && msg == s_taskbarCreated && msg != 0)
            {
                self.Add(); // Explorer restarted; re-create the icon.
                return 0;
            }
        }
        catch (Exception e)
        {
            System.Diagnostics.Debug.WriteLine($"tray: {e}");
        }
        return DefWindowProcW(hwnd, msg, wParam, lParam);
    }

    public void Dispose()
    {
        var data = Data(0);
        Shell_NotifyIconW(NIM_DELETE, &data);
        if (_icon != 0) DestroyIcon(_icon);
        if (_hwnd != 0) DestroyWindow(_hwnd);
        s_instance = null;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct POINT { public int X, Y; }

    [StructLayout(LayoutKind.Sequential)]
    private struct WNDCLASSEXW
    {
        public uint cbSize, style;
        public nint lpfnWndProc;
        public int cbClsExtra, cbWndExtra;
        public nint hInstance, hIcon, hCursor, hbrBackground;
        public char* lpszMenuName, lpszClassName;
        public nint hIconSm;
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NOTIFYICONDATAW
    {
        public uint cbSize;
        public nint hWnd;
        public uint uID, uFlags, uCallbackMessage;
        public nint hIcon;
        public fixed char szTip[128];
        public uint dwState, dwStateMask;
        public fixed char szInfo[256];
        public uint uVersion;
        public fixed char szInfoTitle[64];
        public uint dwInfoFlags;
        public Guid guidItem;
        public nint hBalloonIcon;
    }

    [LibraryImport("shell32.dll")] [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool Shell_NotifyIconW(uint message, NOTIFYICONDATAW* data);
    [LibraryImport("user32.dll")] private static partial ushort RegisterClassExW(WNDCLASSEXW* wc);
    [LibraryImport("user32.dll")]
    private static partial nint CreateWindowExW(uint exStyle, char* cls, char* name, uint style, int x, int y, int w, int h,
        nint parent, nint menu, nint instance, nint param);
    [LibraryImport("user32.dll")] private static partial nint DefWindowProcW(nint hwnd, uint msg, nint w, nint l);
    [LibraryImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static partial bool DestroyWindow(nint hwnd);
    [LibraryImport("user32.dll", StringMarshalling = StringMarshalling.Utf16)] private static partial uint RegisterWindowMessageW(string name);
    [LibraryImport("user32.dll", StringMarshalling = StringMarshalling.Utf16)]
    private static partial nint LoadImageW(nint inst, string name, uint type, int cx, int cy, uint flags);
    [LibraryImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static partial bool DestroyIcon(nint icon);
    [LibraryImport("user32.dll")] private static partial nint CreatePopupMenu();
    [LibraryImport("user32.dll", StringMarshalling = StringMarshalling.Utf16)] [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool AppendMenuW(nint menu, uint flags, nuint id, string? text);
    [LibraryImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static partial bool DestroyMenu(nint menu);
    [LibraryImport("user32.dll")] private static partial int TrackPopupMenuEx(nint menu, uint flags, int x, int y, nint hwnd, nint tpm);
    [LibraryImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static partial bool GetCursorPos(out POINT pt);
    [LibraryImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static partial bool SetForegroundWindow(nint hwnd);
    [LibraryImport("user32.dll")] [return: MarshalAs(UnmanagedType.Bool)] private static partial bool PostMessageW(nint hwnd, uint msg, nint w, nint l);
    [LibraryImport("kernel32.dll", StringMarshalling = StringMarshalling.Utf16)] private static partial nint GetModuleHandleW(string? name);
}

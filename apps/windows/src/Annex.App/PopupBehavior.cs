using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;

namespace Annex.App;

/// <summary>
/// Makes a borderless window behave like a popup: takes focus when shown, and hides as soon as anything else
/// is in the foreground. Relying on the Deactivated event alone is not enough: when opened from the tray,
/// Windows may refuse to make the window active, so it never deactivates and stays on screen.
/// </summary>
public sealed class PopupBehavior
{
    private readonly Window _window;
    private readonly IntPtr _hwnd;
    private readonly DispatcherQueueTimer _watch;
    private DateTime _shownAt;

    public PopupBehavior(Window window)
    {
        _window = window;
        _hwnd = WinRT.Interop.WindowNative.GetWindowHandle(window);

        // Look like a menu, not a window: no DWM open/close zoom (we animate ourselves) and small corners.
        var on = 1;
        DwmSetWindowAttribute(_hwnd, DwmwaTransitionsForceDisabled, ref on, sizeof(int));
        var corner = DwmwcpRoundSmall;
        DwmSetWindowAttribute(_hwnd, DwmwaWindowCornerPreference, ref corner, sizeof(int));
        _watch = window.DispatcherQueue.CreateTimer();
        _watch.Interval = TimeSpan.FromMilliseconds(150);
        _watch.Tick += (_, _) =>
        {
            // Grace period while Windows hands over focus after the tray click.
            if (DateTime.UtcNow - _shownAt > TimeSpan.FromMilliseconds(400) && GetForegroundWindow() != _hwnd)
            {
                Hide();
            }
        };

        window.Activated += (_, e) =>
        {
            if (e.WindowActivationState == WindowActivationState.Deactivated)
            {
                Hide();
            }
        };
    }

    public bool IsVisible => _window.AppWindow.IsVisible;

    /// <summary>Shows the window at <paramref name="bounds"/> (physical pixels) and gives it focus.</summary>
    public void Show(Windows.Graphics.RectInt32 bounds)
    {
        _window.AppWindow.MoveAndResize(bounds);
        AnimateIn();
        _window.AppWindow.Show();
        _window.Activate();
        SetForegroundWindow(_hwnd);
        _shownAt = DateTime.UtcNow;
        _watch.Start();
    }

    /// <summary>Windows 11 menu-like entrance: content fades in while sliding up a few pixels.</summary>
    private void AnimateIn()
    {
        if (_window.Content is not UIElement root)
        {
            return;
        }

        if (root.RenderTransform is not Microsoft.UI.Xaml.Media.TranslateTransform slide)
        {
            slide = new Microsoft.UI.Xaml.Media.TranslateTransform();
            root.RenderTransform = slide;
        }

        root.Opacity = 0;
        slide.Y = 10;
        var ease = new Microsoft.UI.Xaml.Media.Animation.ExponentialEase { EasingMode = Microsoft.UI.Xaml.Media.Animation.EasingMode.EaseOut, Exponent = 5 };
        var duration = new Duration(TimeSpan.FromMilliseconds(200));
        var fade = new Microsoft.UI.Xaml.Media.Animation.DoubleAnimation { From = 0, To = 1, Duration = duration, EasingFunction = ease };
        var move = new Microsoft.UI.Xaml.Media.Animation.DoubleAnimation { From = 10, To = 0, Duration = duration, EasingFunction = ease };
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTarget(fade, root);
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTargetProperty(fade, "Opacity");
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTarget(move, slide);
        Microsoft.UI.Xaml.Media.Animation.Storyboard.SetTargetProperty(move, "Y");
        var storyboard = new Microsoft.UI.Xaml.Media.Animation.Storyboard();
        storyboard.Children.Add(fade);
        storyboard.Children.Add(move);
        storyboard.Begin();
    }

    // dwmapi.h
    private const int DwmwaTransitionsForceDisabled = 3;
    private const int DwmwaWindowCornerPreference = 33;
    private const int DwmwcpRoundSmall = 3;

    [DllImport("dwmapi.dll")]
    private static extern int DwmSetWindowAttribute(IntPtr hwnd, int attribute, ref int value, int size);

    public void Hide()
    {
        _watch.Stop();
        _window.AppWindow.Hide();
    }

    /// <summary>Common popup chrome: no title bar, not resizable, topmost, not in Alt+Tab.</summary>
    public static void ConfigureChrome(Window window)
    {
        if (window.AppWindow.Presenter is OverlappedPresenter p)
        {
            p.IsResizable = false;
            p.IsMaximizable = false;
            p.IsMinimizable = false;
            p.IsAlwaysOnTop = true;
            p.SetBorderAndTitleBar(true, false);
        }

        window.AppWindow.IsShownInSwitchers = false;
    }

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hwnd);
}

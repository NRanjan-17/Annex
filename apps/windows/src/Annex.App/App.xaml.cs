using System.Runtime.InteropServices;
using Annex.Interop;
using Microsoft.UI.Xaml;

namespace Annex.App;

public partial class App : Application
{
    private MainWindow? _window;

    public App()
    {
        // A crash during startup would otherwise close Annex without a trace.
        UnhandledException += (_, e) =>
        {
            CrashReport.Show(e.Exception);
            e.Handled = false;
        };
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
        {
            if (e.ExceptionObject is Exception ex) CrashReport.Show(ex);
        };
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        try
        {
            // Started by "Start with Windows": stay in the tray until the user opens the window.
            var background = Environment.GetCommandLineArgs().Contains(AppSettings.BackgroundArg);
            _window = new MainWindow();
            if (!background) _window.Activate();
        }
        catch (Exception ex)
        {
            CrashReport.Show(ex);
            Exit();
        }
    }
}

/// <summary>Writes the error to %LOCALAPPDATA%\Annex\crash.log and tells the user where it is.</summary>
internal static partial class CrashReport
{
    private static bool s_shown;

    public static void Show(Exception ex)
    {
        AnnexLog.Error($"[app] crash: {ex}");
        var path = Path.Combine(MainWindow.DataDirectory, "crash.log");
        try
        {
            Directory.CreateDirectory(MainWindow.DataDirectory);
            File.AppendAllText(path, $"[{DateTimeOffset.Now:O}] Annex {typeof(App).Assembly.GetName().Version}\n{ex}\n\n");
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AnnexLog.Warn($"[app] couldn't write crash.log: {e.Message}");
        }
        if (s_shown) return;
        s_shown = true;
        // A plain Win32 message box: works even when XAML failed to start.
        var text = BlockedByAppControl(ex)
            ? "Windows blocked part of Annex.\n\n" +
              "Smart App Control stopped annex_ffi.dll from loading, because this build of Annex isn't code-signed yet.\n\n" +
              "To use Annex, turn Smart App Control off in Windows Security → App & browser control → " +
              "Smart App Control settings. On some versions of Windows it can't be turned back on without resetting Windows.\n\n" +
              $"Details were saved to:\n{path}"
            : $"Annex ran into a problem and has to close.\n\n{ex.GetType().Name}: {ex.Message}\n\nDetails were saved to:\n{path}";
        MessageBoxW(0, text, "Annex", 0x10 /* MB_ICONERROR */);
    }

    /// <summary>Smart App Control / App Control for Business refused to load one of our DLLs
    /// (ERROR_SYSTEM_INTEGRITY_POLICY_VIOLATION, 0x800711C7). The code appears in the message in every language.</summary>
    private static bool BlockedByAppControl(Exception ex)
    {
        for (Exception? e = ex; e != null; e = e.InnerException)
            if (e is DllNotFoundException && e.Message.Contains("0x800711C7", StringComparison.OrdinalIgnoreCase))
                return true;
        return false;
    }

    [LibraryImport("user32.dll", StringMarshalling = StringMarshalling.Utf16)]
    private static partial int MessageBoxW(nint hwnd, string text, string caption, uint type);
}

using System.Runtime.InteropServices;
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
        var path = Path.Combine(MainWindow.DataDirectory, "crash.log");
        try
        {
            Directory.CreateDirectory(MainWindow.DataDirectory);
            File.AppendAllText(path, $"[{DateTimeOffset.Now:O}] Annex {typeof(App).Assembly.GetName().Version}\n{ex}\n\n");
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
        }
        if (s_shown) return;
        s_shown = true;
        // A plain Win32 message box: works even when XAML failed to start.
        MessageBoxW(0, $"Annex ran into a problem and has to close.\n\n{ex.GetType().Name}: {ex.Message}\n\nDetails were saved to:\n{path}",
            "Annex", 0x10 /* MB_ICONERROR */);
    }

    [LibraryImport("user32.dll", StringMarshalling = StringMarshalling.Utf16)]
    private static partial int MessageBoxW(nint hwnd, string text, string caption, uint type);
}

using Microsoft.UI.Xaml;

namespace Annex.App;

public partial class App : Application
{
    private MainWindow? _window;

    public App()
    {
        InitializeComponent();
    }

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        // Started by "Start with Windows": stay in the tray until the user opens the window.
        var background = Environment.GetCommandLineArgs().Contains(AppSettings.BackgroundArg);
        _window = new MainWindow();
        if (!background) _window.Activate();
    }
}

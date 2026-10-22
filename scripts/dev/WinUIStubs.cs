// Used only by scripts/dev/check-winui-csharp.sh. Keep in sync with x:Name elements in MainWindow.xaml.
using Microsoft.UI.Xaml.Controls;
namespace Annex.App;
// Stand-ins for what the XAML compiler generates from MainWindow.xaml.
public sealed partial class MainWindow
{
    private Grid RootGrid = new();
    private void InitializeComponent() { }
}

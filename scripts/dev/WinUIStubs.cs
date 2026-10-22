// Used only by scripts/dev/check-winui-csharp.sh. Keep in sync with x:Name elements in MainWindow.xaml.
using Microsoft.UI.Xaml.Controls;
namespace Annex.App;
// Stand-ins for what the XAML compiler generates from MainWindow.xaml.
public sealed partial class MainWindow
{
    private TextBlock DeviceNameText = new(), FingerprintText = new(), NoHostsText = new(), StatusText = new(), LatencyText = new(), VersionText = new();
    private ListView HostsList = new(), TrustedList = new();
    private TextBox AddressBox = new();
    private ToggleSwitch HostToggle = new(), ExtendToggle = new();
    private ComboBox MonitorBox = new();
    private CheckBox HardwareBox = new();
    private TextBlock HostStatusText = new();
    private void InitializeComponent() { }
}

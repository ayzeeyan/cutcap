using Avalonia.Controls;
using Avalonia.Interactivity;

namespace CutCap.Desktop.Views;

public partial class ExportRunningWindow : Window
{
    public ExportRunningWindow() => InitializeComponent();
    private void Keep(object? sender, RoutedEventArgs e) => Close(false);
    private void Stop(object? sender, RoutedEventArgs e) => Close(true);
}

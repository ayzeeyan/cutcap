using Avalonia.Controls;
using Avalonia.Interactivity;

namespace CutCap.Desktop.Views;

/// <summary>View glue only: everything shown here comes from ExportViewModel.</summary>
public partial class ExportWindow : Window
{
    public ExportWindow() => InitializeComponent();
    private void CloseWindow(object? sender, RoutedEventArgs e) => Close();
}

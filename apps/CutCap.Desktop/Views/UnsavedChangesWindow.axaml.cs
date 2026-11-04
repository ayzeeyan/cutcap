using Avalonia.Controls;
using Avalonia.Interactivity;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

public partial class UnsavedChangesWindow : Window
{
    public UnsavedChangesWindow() => InitializeComponent();
    private void Cancel(object? sender, RoutedEventArgs e) => Close(UnsavedChoice.Cancel);
    private void Discard(object? sender, RoutedEventArgs e) => Close(UnsavedChoice.Discard);
    private void Save(object? sender, RoutedEventArgs e) => Close(UnsavedChoice.Save);
}

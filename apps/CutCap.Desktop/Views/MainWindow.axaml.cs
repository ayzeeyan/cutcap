using Avalonia.Controls;
using Avalonia.Platform.Storage;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

public partial class MainWindow : Window
{
    private static readonly FilePickerFileType Videos = new("Video")
    {
        Patterns = ["*.mp4", "*.mov", "*.m4v", "*.mkv", "*.webm", "*.avi", "*.ts"],
    };

    public MainWindow()
    {
        InitializeComponent();
    }

    protected override void OnDataContextChanged(EventArgs e)
    {
        base.OnDataContextChanged(e);
        if (DataContext is MainWindowViewModel viewModel)
        {
            viewModel.PickVideo = PickVideoAsync;
            viewModel.LayoutResetRequested += (_, _) => ResetLayout();
        }
    }

    /// <summary>Native, window-parented, asynchronous picker: the UI thread never blocks.</summary>
    private async Task<string?> PickVideoAsync()
    {
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = "Import source video",
            AllowMultiple = false,
            FileTypeFilter = [Videos, FilePickerFileTypes.All],
        });
        return files.Count > 0 ? files[0].TryGetLocalPath() : null;
    }

    private void ResetLayout()
    {
        Upper.ColumnDefinitions[0].Width = new GridLength(34, GridUnitType.Star);
        Upper.ColumnDefinitions[2].Width = new GridLength(41, GridUnitType.Star);
        Upper.ColumnDefinitions[4].Width = new GridLength(25, GridUnitType.Star);
        Workspace.RowDefinitions[0].Height = new GridLength(58, GridUnitType.Star);
        Workspace.RowDefinitions[2].Height = new GridLength(42, GridUnitType.Star);
    }
}

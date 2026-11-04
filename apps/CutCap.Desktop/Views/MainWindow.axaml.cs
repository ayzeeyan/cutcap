using Avalonia.Controls;
using Avalonia;
using Avalonia.Input;
using Avalonia.Platform.Storage;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

public partial class MainWindow : Window
{
    private MainWindowViewModel? _viewModel;
    private bool _allowClose;
    private bool _confirmingClose;
    private static readonly FilePickerFileType Projects = new("CutCap project") { Patterns = ["*.cutcap"] };
    private static readonly FilePickerFileType Videos = new("Video")
    {
        Patterns = ["*.mp4", "*.mov", "*.m4v", "*.mkv", "*.webm", "*.avi", "*.ts"],
    };

    public MainWindow()
    {
        InitializeComponent();
        Closing += ConfirmClosing;
        // Tunnel: lists and tabs would otherwise consume Space as "select".
        AddHandler(KeyDownEvent, OnPreviewKeyDown, Avalonia.Interactivity.RoutingStrategies.Tunnel);
    }

    /// <summary>
    /// Space toggles source playback anywhere in the workspace except where it
    /// already means something: text fields keep typing spaces and a focused
    /// button (or toggle) keeps Space for activating itself. Enter is never taken.
    /// Timeline editing keys (Ctrl+Z, Ctrl+Y / Ctrl+Shift+Z, Delete) are taken
    /// everywhere except in text fields, which keep their own undo and delete.
    /// </summary>
    private void OnPreviewKeyDown(object? sender, KeyEventArgs e)
    {
        if (EditingCommand(e) is { } command)
        {
            if (FocusManager?.GetFocusedElement() is TextBox) { return; }
            if (command.CanExecute(null)) { command.Execute(null); }
            e.Handled = true;
            return;
        }
        if (e.Key != Key.Space || e.KeyModifiers != KeyModifiers.None
            || FocusManager?.GetFocusedElement() is TextBox or Avalonia.Controls.Primitives.ToggleButton or Button
            || DataContext is not MainWindowViewModel { Player: var player })
        {
            return;
        }
        if (player.PlayPauseCommand.CanExecute(null))
        {
            player.PlayPauseCommand.Execute(null);
        }
        e.Handled = true;
    }

    private System.Windows.Input.ICommand? EditingCommand(KeyEventArgs e)
    {
        if (DataContext is not MainWindowViewModel { Timeline: var timeline }) { return null; }
        return (e.Key, e.KeyModifiers) switch
        {
            (Key.Z, KeyModifiers.Control) => timeline.UndoCommand,
            (Key.Y, KeyModifiers.Control) or (Key.Z, KeyModifiers.Control | KeyModifiers.Shift) => timeline.RedoCommand,
            (Key.Delete, KeyModifiers.None) or (Key.Back, KeyModifiers.None) when timeline.SelectedClip is not null
                => timeline.RemoveClipCommand,
            _ => null,
        };
    }

    protected override void OnDataContextChanged(EventArgs e)
    {
        if (_viewModel is not null)
        {
            _viewModel.PickVideo = null;
            _viewModel.PickRelinkSource = null;
            _viewModel.PickProject = null;
            _viewModel.PickSaveProject = null;
            _viewModel.ConfirmUnsaved = null;
            _viewModel.LayoutResetRequested -= LayoutReset;
        }
        base.OnDataContextChanged(e);
        _viewModel = DataContext as MainWindowViewModel;
        if (_viewModel is { } viewModel)
        {
            viewModel.PickVideo = PickVideoAsync;
            viewModel.PickRelinkSource = () => PickVideoAsync("Relink original source video");
            viewModel.PickProject = PickProjectAsync;
            viewModel.PickSaveProject = PickSaveProjectAsync;
            viewModel.ConfirmUnsaved = async () =>
                await new UnsavedChangesWindow().ShowDialog<UnsavedChoice?>(this) ?? UnsavedChoice.Cancel;
            viewModel.LayoutResetRequested += LayoutReset;
        }
    }

    private void LayoutReset(object? sender, EventArgs e) => ResetLayout();

    protected override void OnOpened(EventArgs e)
    {
        base.OnOpened(e);
        if (WindowState == WindowState.Normal && Screens.ScreenFromWindow(this) is { } screen)
        {
            // The desired 1440×900 logical canvas exceeded this laptop's 150%
            // work area and hid the project toolbar. Fit once, then let Windows
            // and the user own resizing; never assume a physical pixel density.
            Width = Math.Min(Width, Math.Max(MinWidth, screen.WorkingArea.Width / screen.Scaling - 32));
            Height = Math.Min(Height, Math.Max(MinHeight, screen.WorkingArea.Height / screen.Scaling - 32));
            Position = new PixelPoint(screen.WorkingArea.X + Math.Max(0, (screen.WorkingArea.Width - (int)(Width * screen.Scaling)) / 2),
                screen.WorkingArea.Y + Math.Max(0, (screen.WorkingArea.Height - (int)(Height * screen.Scaling)) / 2));
        }
    }

    private async void ConfirmClosing(object? sender, WindowClosingEventArgs e)
    {
        if (_allowClose || _viewModel is null) { return; }
        e.Cancel = true;
        if (_confirmingClose) { return; }
        _confirmingClose = true;
        try
        {
            if (await _viewModel.ConfirmCloseAsync())
            {
                // Finish this cancelled Closing event before issuing a new close.
                Avalonia.Threading.Dispatcher.UIThread.Post(() => { _allowClose = true; Close(); });
            }
        }
        finally { _confirmingClose = false; }
    }

    private async Task<string?> PickProjectAsync()
    {
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        { Title = "Open CutCap project", AllowMultiple = false, FileTypeFilter = [Projects] });
        return files.Count > 0 ? files[0].TryGetLocalPath() : null;
    }

    private async Task<string?> PickSaveProjectAsync(string? currentPath)
    {
        var file = await StorageProvider.SaveFilePickerAsync(new FilePickerSaveOptions
        {
            Title = "Save CutCap project", FileTypeChoices = [Projects], DefaultExtension = "cutcap",
            SuggestedFileName = currentPath is null ? "Untitled project.cutcap" : System.IO.Path.GetFileName(currentPath),
            ShowOverwritePrompt = true,
        });
        return file?.TryGetLocalPath();
    }

    /// <summary>Native, window-parented, asynchronous picker: the UI thread never blocks.</summary>
    private Task<string?> PickVideoAsync() => PickVideoAsync("Import source video");
    private async Task<string?> PickVideoAsync(string title)
    {
        var files = await StorageProvider.OpenFilePickerAsync(new FilePickerOpenOptions
        {
            Title = title,
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

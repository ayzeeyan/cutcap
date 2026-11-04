using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>Asset workspace category. Only Media is backed by the engine today.</summary>
public sealed record CategoryViewModel(string Name, string Glyph, bool IsAvailable, string Title, string Detail);

/// <summary>
/// Shell view model. Holds no project state of its own: every value is derived
/// from the latest engine view; commands go straight to the engine.
/// </summary>
public sealed partial class MainWindowViewModel : ObservableObject
{
    private readonly EngineHost _engine;
    private (StatusView Status, uint Pending)? _engineStatus;

    public MainWindowViewModel(EngineHost engine)
    {
        _engine = engine;
        Library = new MediaLibraryViewModel(engine) { ImportCommand = ImportCommand };
        Player = new PlayerViewModel(engine) { ImportCommand = ImportCommand };
        Inspector = new InspectorViewModel(engine);
        Timeline = new TimelineViewModel(engine);
        Categories =
        [
            new("Media", "Import", true, "", ""),
            new("Audio", "Music", false, "Audio library is coming later",
                "Music and audio-only media need audio playback. Videos with sound already import under Media."),
            new("Text", "Text", false, "Text is coming later", "Titles and captions arrive with the editing engine and compositor."),
            new("Stickers", "Sticker", false, "Stickers are coming later", "Overlays arrive with the GPU compositor."),
            new("Effects", "Effects", false, "Effects are coming later", "Effects arrive with the GPU compositor. Nothing here is a working preset yet."),
            new("Transitions", "Transition", false, "Transitions are coming later", "Transitions need clips on the timeline and the compositor."),
            new("Filters", "Filters", false, "Filters are coming later", "Colour filters arrive with the GPU compositor."),
            new("Adjustment", "Sliders", false, "Adjustment is coming later", "Colour adjustment arrives with the GPU compositor."),
        ];
        SelectedCategory = Categories[0];
        engine.ViewChanged += (_, view) => Apply(view);
        engine.CommandRejected += (_, message) => ShowRejection(message);
    }

    public MediaLibraryViewModel Library { get; }
    public PlayerViewModel Player { get; }
    public InspectorViewModel Inspector { get; }
    public TimelineViewModel Timeline { get; }
    public IReadOnlyList<CategoryViewModel> Categories { get; }

    /// <summary>Opens the native file picker; supplied by the window.</summary>
    public Func<Task<string?>>? PickVideo { get; set; }

    public event EventHandler? LayoutResetRequested;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowMediaCategory), nameof(ShowUnavailableCategory))]
    public partial CategoryViewModel SelectedCategory { get; set; }

    [ObservableProperty]
    public partial string ProjectName { get; set; } = "Starting…";

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(ImportCommand), nameof(NewProjectCommand), nameof(ClearSelectionCommand))]
    public partial bool EngineReady { get; set; }

    [ObservableProperty]
    public partial string StatusText { get; set; } = "Starting the engine…";

    [ObservableProperty]
    public partial bool StatusIsError { get; set; }

    public bool ShowMediaCategory => SelectedCategory.IsAvailable;
    public bool ShowUnavailableCategory => !SelectedCategory.IsAvailable;

    private bool CanImport() => EngineReady && !Library.ImportBusy;

    [RelayCommand(CanExecute = nameof(CanImport))]
    private async Task ImportAsync()
    {
        if (PickVideo is null)
        {
            return;
        }
        Library.Choosing = true;
        Library.ImportBusy = true;
        string? path;
        try
        {
            path = await PickVideo();
        }
        finally
        {
            Library.Choosing = false;
            Library.ImportBusy = _engine.View?.Import.Busy ?? false;
        }
        if (path is null)
        {
            Library.DialogCancelled();
            return;
        }
        SelectedCategory = Categories[0];
        PreviewTrace.MarkImport();
        _engine.Run(session => session.Import(path));
    }

    [RelayCommand(CanExecute = nameof(EngineReady))]
    private void NewProject() => _engine.Run(session => session.NewProject());

    [RelayCommand(CanExecute = nameof(EngineReady))]
    private void ClearSelection() => _engine.Run(session => session.ClearSelection());

    [RelayCommand]
    private void ResetLayout()
    {
        Timeline.Zoom = 100;
        LayoutResetRequested?.Invoke(this, EventArgs.Empty);
    }

    private void Apply(SessionView view)
    {
        EngineReady = view.EngineReady;
        ProjectName = view.Project?.Name ?? "Starting…";
        // Show the engine status whenever it (or the pending count) changes; a local
        // rejection message stays until the engine reports something new.
        if (_engineStatus != (view.Status, view.Pending))
        {
            _engineStatus = (view.Status, view.Pending);
            StatusText = view.Status.Text + (view.Pending > 0 ? $" · {view.Pending} pending" : "");
            StatusIsError = view.Status.Error;
        }
        Library.Apply(view);
        Player.Apply(view.Source, view.EngineReady && !view.Import.Busy);
        Inspector.Apply(view.Inspector);
        Timeline.Apply(view);
        ImportCommand.NotifyCanExecuteChanged();
    }

    private void ShowRejection(string message)
    {
        StatusText = message;
        StatusIsError = true;
    }
}

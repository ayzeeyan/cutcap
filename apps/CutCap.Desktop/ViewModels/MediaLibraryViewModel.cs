using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>One imported source, as the engine reports it. Read-only in the UI.</summary>
public sealed partial class MediaItemViewModel(MediaView media) : ObservableObject
{
    public string Id { get; } = media.Id;

    [ObservableProperty]
    public partial MediaView Media { get; set; } = media;

    public string Name => Media.Name;
    public string Stamp => (Media.Estimated ? "≈" : "") + Media.Duration;
    public string Details => $"{Media.Resolution} · {Media.Format}";
    public string Tooltip => $"{Details}\n{Media.Detail}\n{Media.Path}";
    public bool Unavailable => Media.Availability is "missing" or "changed" or "unavailable";
    public string AvailabilityText => Media.Availability switch
    { "checking" => "Checking…", "missing" => "Missing media", "changed" => "Source changed", "unavailable" => "Unavailable", _ => "" };
    public bool ShowAvailability => Media.Availability != "ready";
    public bool HasAudio => Media.HasAudio;
    public string TileGlyph => Media.AudioOnly ? "Music" : "Film";
    public string AccessibleName =>
        $"{Media.Name}, {Media.Resolution}, {Media.Format}, {(Media.Estimated ? "about " : "")}{Media.Duration}, {(Media.HasAudio ? "with audio" : "no audio")}";

    partial void OnMediaChanged(MediaView value)
    {
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(Stamp));
        OnPropertyChanged(nameof(Details));
        OnPropertyChanged(nameof(HasAudio));
        OnPropertyChanged(nameof(TileGlyph));
        OnPropertyChanged(nameof(AccessibleName));
        OnPropertyChanged(nameof(Tooltip));
        OnPropertyChanged(nameof(Unavailable));
        OnPropertyChanged(nameof(AvailabilityText));
        OnPropertyChanged(nameof(ShowAvailability));
    }
}

/// <summary>
/// Library presentation: collection filter and name search are view state over
/// the engine's media list; they never change the project.
/// </summary>
public sealed partial class MediaLibraryViewModel : ObservableObject
{
    private readonly EngineHost _engine;
    private bool _applying;

    public MediaLibraryViewModel(EngineHost engine) => _engine = engine;

    public ObservableCollection<MediaItemViewModel> Items { get; } = [];
    public ObservableCollection<MediaItemViewModel> Visible { get; } = [];
    public IReadOnlyList<string> Collections { get; } = ["All media", "With audio", "Silent"];

    /// <summary>Set by the shell; opens the native picker.</summary>
    public IRelayCommand? ImportCommand { get; set; }

    [ObservableProperty]
    public partial int Filter { get; set; }

    [ObservableProperty]
    public partial string SearchText { get; set; } = "";

    [ObservableProperty]
    public partial MediaItemViewModel? SelectedItem { get; set; }

    [ObservableProperty]
    public partial bool ImportBusy { get; set; }

    [ObservableProperty]
    public partial bool Choosing { get; set; }

    [ObservableProperty]
    public partial string ImportMessage { get; set; } = "";

    [ObservableProperty]
    public partial bool ImportError { get; set; }

    public bool IsEmpty => Items.Count == 0;
    public bool HasItems => Items.Count > 0;
    public bool HasNoMatches => Items.Count > 0 && Visible.Count == 0;
    public string NoMatchesText => SearchText.Trim().Length > 0
        ? "No media name matches this search."
        : Filter == 1 ? "No imported source has audio." : "Every imported source has audio.";
    public string CountText => Items.Count == 1 ? "1 item" : $"{Items.Count} items";
    public bool ShowMessage => !ImportBusy && (ImportError || ImportMessage is not ("" or "Import a local video."));
    public bool ShowImportProgress => ImportBusy && !Choosing;
    public string BusyText => Choosing ? "Choose a local video…" : ImportMessage;

    partial void OnFilterChanged(int value) => Refilter();
    partial void OnSearchTextChanged(string value) => Refilter();
    partial void OnImportBusyChanged(bool value) => NotifyImportState();
    partial void OnChoosingChanged(bool value) => NotifyImportState();
    partial void OnImportMessageChanged(string value) => NotifyImportState();
    partial void OnImportErrorChanged(bool value) => NotifyImportState();

    partial void OnSelectedItemChanged(MediaItemViewModel? value)
    {
        if (!_applying && value is not null && !value.Media.Selected)
        {
            _engine.Run(session => session.SelectMedia(value.Id));
        }
    }

    /// <summary>Returns the inspector to the current source without reloading it.</summary>
    public void ShowSource(MediaItemViewModel item)
    {
        if (item.Media.Selected)
        {
            _engine.Run(session => session.SelectMedia(item.Id));
        }
    }

    [RelayCommand]
    private void CancelImport() => _engine.Run(session => session.CancelImport());

    /// <summary>The native picker was dismissed without a file.</summary>
    public void DialogCancelled()
    {
        ImportError = false;
        ImportMessage = "Import cancelled";
    }

    public void Apply(SessionView view)
    {
        _applying = true;
        try
        {
            Sync.Items(Items, view.Media, item => item.Id, media => media.Id,
                media => new MediaItemViewModel(media), (item, media) => item.Media = media);
            ImportBusy = view.Import.Busy || Choosing;
            ImportError = view.Import.Error;
            ImportMessage = view.Import.Message;
            if (Items.Count == 0)
            {
                // An emptied library (New Project) starts from the full, unsearched view.
                Filter = 0;
                SearchText = "";
            }
            Refilter();
        }
        finally
        {
            _applying = false;
        }
    }

    private void Refilter()
    {
        var wasApplying = _applying;
        _applying = true;
        try
        {
            var query = SearchText.Trim();
            var matches = Items.Where(item =>
                (Filter switch { 1 => item.HasAudio, 2 => !item.HasAudio, _ => true })
                && (query.Length == 0 || item.Name.Contains(query, StringComparison.CurrentCultureIgnoreCase))).ToList();
            Sync.Items(Visible, matches, item => item.Id, item => item.Id, item => item, (_, _) => { });
            SelectedItem = Visible.FirstOrDefault(item => item.Media.Selected);
        }
        finally
        {
            _applying = wasApplying;
        }
        OnPropertyChanged(nameof(IsEmpty));
        OnPropertyChanged(nameof(HasItems));
        OnPropertyChanged(nameof(HasNoMatches));
        OnPropertyChanged(nameof(NoMatchesText));
        OnPropertyChanged(nameof(CountText));
    }

    private void NotifyImportState()
    {
        OnPropertyChanged(nameof(ShowMessage));
        OnPropertyChanged(nameof(ShowImportProgress));
        OnPropertyChanged(nameof(BusyText));
        ImportCommand?.NotifyCanExecuteChanged();
    }
}

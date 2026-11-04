using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

public sealed partial class SectionViewModel(SectionView section) : ObservableObject
{
    public string Title { get; } = section.Title;

    [ObservableProperty]
    public partial IReadOnlyList<RowView> Rows { get; set; } = section.Rows;

    /// <summary>View state: survives updates of the same section, resets for a new subject.</summary>
    [ObservableProperty]
    public partial bool IsExpanded { get; set; } = true;
}

/// <summary>
/// Context-sensitive inspector. Details shows genuine metadata from the engine;
/// the clip tabs state honestly that they need a timeline clip.
/// </summary>
public sealed partial class InspectorViewModel : ObservableObject
{
    private readonly EngineHost _engine;

    public InspectorViewModel(EngineHost engine) => _engine = engine;

    public ObservableCollection<SectionViewModel> Sections { get; } = [];
    public IReadOnlyList<string> Tabs { get; } = ["Details", "Video", "Speed", "Adjustment", "Audio"];

    [ObservableProperty]
    public partial string Kind { get; set; } = "project";

    [ObservableProperty]
    public partial string Title { get; set; } = "Starting…";

    [ObservableProperty]
    public partial string Detail { get; set; } = "";

    [ObservableProperty]
    public partial int SelectedTab { get; set; }

    public bool IsSource => Kind == "source";
    public bool IsTrack => Kind == "track";
    public bool ShowDetails => SelectedTab == 0;
    public bool ShowClipPlaceholder => SelectedTab != 0;
    public string PlaceholderTitle => $"{Tabs[Math.Clamp(SelectedTab, 0, Tabs.Count - 1)]} settings need a clip";
    public string PlaceholderDetail => SelectedTab switch
    {
        1 => "Transform, crop and opacity appear here once clips can be placed on the timeline.",
        2 => "Speed and duration controls arrive with timeline editing.",
        3 => "Colour adjustment arrives with the GPU compositor.",
        _ => "Volume and fades arrive with audio playback.",
    };

    partial void OnKindChanged(string value)
    {
        OnPropertyChanged(nameof(IsSource));
        OnPropertyChanged(nameof(IsTrack));
    }

    partial void OnSelectedTabChanged(int value)
    {
        OnPropertyChanged(nameof(ShowDetails));
        OnPropertyChanged(nameof(ShowClipPlaceholder));
        OnPropertyChanged(nameof(PlaceholderTitle));
        OnPropertyChanged(nameof(PlaceholderDetail));
    }

    [RelayCommand]
    private void ClearSelection() => _engine.Run(session => session.ClearSelection());

    public void Apply(InspectorView inspector)
    {
        var newSubject = inspector.Title != Title || inspector.Kind != Kind;
        Kind = inspector.Kind;
        Title = inspector.Title;
        Detail = inspector.Detail;
        Sync.Items(Sections, inspector.Sections, s => s.Title, s => s.Title,
            s => new SectionViewModel(s), (item, s) => item.Rows = s.Rows);
        if (newSubject)
        {
            foreach (var section in Sections)
            {
                section.IsExpanded = true;
            }
        }
    }
}

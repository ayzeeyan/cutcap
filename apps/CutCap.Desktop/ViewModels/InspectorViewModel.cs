using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>One audio stream of the selected source, with its explicit Place action.</summary>
public sealed partial class AudioStreamItem(AudioStreamView stream, string mediaId, TimelineViewModel? timeline) : ObservableObject
{
    public uint Index { get; } = stream.Index;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Label), nameof(PlaceName))]
    public partial AudioStreamView Stream { get; set; } = stream;

    public string Label => Stream.Label;
    public string PlaceName => $"Place audio stream {Index} on the timeline";

    [RelayCommand]
    private void Place() => timeline?.PlaceAudioStream(mediaId, Index);
}

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

    public InspectorViewModel(EngineHost engine, TimelineViewModel? timeline = null)
    {
        _engine = engine;
        Timeline = timeline;
    }

    /// <summary>Unlink/Link and stream placement act through the timeline.</summary>
    public TimelineViewModel? Timeline { get; }

    /// <summary>The selected source's decodable audio streams, for explicit placement.</summary>
    public ObservableCollection<AudioStreamItem> AudioStreams { get; } = [];
    public bool ShowAudioStreams => IsSource && AudioStreams.Count > 0;

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

    /// <summary>"Video", "Audio" or empty: the identity badge next to the title.</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ShowBadge), nameof(BadgeIsAudio))]
    public partial string Badge { get; set; } = "";

    public bool ShowBadge => Badge.Length > 0;
    public bool BadgeIsAudio => Badge == "Audio";

    public bool IsSource => Kind == "source";
    public bool IsTrack => Kind == "track";
    public bool IsClip => Kind == "clip";
    public bool HasSelection => IsTrack || IsClip;
    public bool ShowDetails => SelectedTab == 0;
    public bool ShowClipPlaceholder => SelectedTab != 0;
    public string PlaceholderTitle => IsClip
        ? $"{Tabs[Math.Clamp(SelectedTab, 0, Tabs.Count - 1)]} settings are not available yet"
        : $"{Tabs[Math.Clamp(SelectedTab, 0, Tabs.Count - 1)]} settings need a clip";
    public string PlaceholderDetail => SelectedTab switch
    {
        1 => IsClip ? "Transform, crop and opacity arrive with the GPU compositor."
            : "Select a clip on the timeline. Transform, crop and opacity arrive with the GPU compositor.",
        2 => "Speed and trim controls arrive with a later editing phase.",
        3 => "Colour adjustment arrives with the GPU compositor.",
        _ => "Track volume, mute and solo are in the track header. Clip fades arrive later.",
    };

    partial void OnKindChanged(string value)
    {
        OnPropertyChanged(nameof(IsSource));
        OnPropertyChanged(nameof(IsTrack));
        OnPropertyChanged(nameof(IsClip));
        OnPropertyChanged(nameof(HasSelection));
        OnPropertyChanged(nameof(ShowAudioStreams));
        OnPropertyChanged(nameof(PlaceholderTitle));
        OnPropertyChanged(nameof(PlaceholderDetail));
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

    public void Apply(InspectorView inspector, MediaView? source = null)
    {
        var newSubject = inspector.Title != Title || inspector.Kind != Kind;
        Kind = inspector.Kind;
        Badge = inspector.Badge;
        Title = inspector.Title;
        Detail = inspector.Detail;
        var streams = inspector.Kind == "source" && source is not null ? source.AudioStreams ?? [] : [];
        Sync.Items(AudioStreams, streams, s => s.Index.ToString(), s => s.Index.ToString(),
            s => new AudioStreamItem(s, source!.Id, Timeline), (item, s) => item.Stream = s);
        OnPropertyChanged(nameof(ShowAudioStreams));
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

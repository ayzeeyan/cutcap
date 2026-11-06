using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Controls;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

public sealed partial class TrackViewModel(TrackView track) : ObservableObject
{
    public string Id { get; } = track.Id;

    [ObservableProperty]
    public partial TrackView Track { get; set; } = track;

    public string Name => Track.Name;
    public string Badge => Track.Badge;
    public bool IsVideo => Track.IsVideo;
    public bool IsAudio => !Track.IsVideo;
    // Lanes with clips carry no label (the clips are drawn instead).
    public string AccessibleName => Track.LaneLabel.Length == 0 ? Track.Name : $"{Track.Name}, {Track.LaneLabel}";

    partial void OnTrackChanged(TrackView value)
    {
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(Badge));
        OnPropertyChanged(nameof(IsVideo));
        OnPropertyChanged(nameof(IsAudio));
        OnPropertyChanged(nameof(AccessibleName));
    }
}

/// <summary>
/// Timeline over the engine's tracks and clips. Every clip, selection and
/// history value comes from the engine; this only forwards commands. Zoom is
/// presentation state. The playhead stays at timeline zero until a timeline
/// transport exists, and the source monitor never moves it.
/// <para>Insertion policy (explicit, append-to-end): the whole selected ready
/// source is placed after the last clip of the target video track. The target
/// is the selected track if it is a video track, else the track of the selected
/// clip, else the first video track.</para>
/// </summary>
public sealed partial class TimelineViewModel : ObservableObject
{
    public const int MaxTracks = 128;
    private readonly EngineHost _engine;
    private bool _applying;

    public TimelineViewModel(EngineHost engine) => _engine = engine;

    public ObservableCollection<TrackViewModel> Tracks { get; } = [];

    [ObservableProperty]
    public partial IReadOnlyList<LaneInfo> Lanes { get; set; } = [];

    [ObservableProperty]
    public partial TrackViewModel? SelectedTrack { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(ZoomInCommand), nameof(ZoomOutCommand))]
    public partial int Zoom { get; set; } = 100;

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(AddVideoTrackCommand), nameof(AddAudioTrackCommand), nameof(AddClipCommand),
        nameof(RemoveClipCommand), nameof(UndoCommand), nameof(RedoCommand))]
    public partial bool EngineReady { get; set; }

    /// <summary>Clips in project order, as the engine presents them.</summary>
    [ObservableProperty]
    public partial IReadOnlyList<ClipView> Clips { get; set; } = [];

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(RemoveClipCommand))]
    [NotifyPropertyChangedFor(nameof(RemoveClipTip))]
    public partial ClipView? SelectedClip { get; set; }

    /// <summary>The library's selected source, the candidate for insertion.</summary>
    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(AddClipCommand))]
    [NotifyPropertyChangedFor(nameof(AddClipTip))]
    public partial MediaView? Source { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(AddClipCommand))]
    [NotifyPropertyChangedFor(nameof(AddClipTip))]
    public partial TrackViewModel? TargetTrack { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(UndoCommand), nameof(RedoCommand))]
    [NotifyPropertyChangedFor(nameof(UndoTip), nameof(RedoTip))]
    public partial HistoryStatus History { get; set; } = new(false, false, null, null, 0);

    public string UndoTip => History.Undo is { } label ? $"Undo {label} (Ctrl+Z)" : "Nothing to undo";
    public string RedoTip => History.Redo is { } label ? $"Redo {label} (Ctrl+Y)" : "Nothing to redo";
    public string RemoveClipTip => SelectedClip is { } clip ? $"Remove {clip.Name} from the timeline (Delete). The source file is kept." : "Select a clip to remove it";
    public string AddClipTip => (Source, TargetTrack) switch
    {
        (null, _) => "Select a source video in Media to add it to the timeline",
        ({ Availability: not "ready" } source, _) => $"{source.Name} is unavailable; relink it first",
        (_, null) => "Add a video track first",
        var (source, track) => $"Add {source.Name} to the end of {track.Name}",
    };
    public string ClipCountText => Clips.Count == 1 ? "1 clip" : $"{Clips.Count} clips";

    /// <summary>A screen-reader summary of the drawn clip surface.</summary>
    public string LanesAccessibleName => SelectedClip is { } clip
        ? $"Track lanes, {ClipCountText}. Selected clip {clip.Name}, {clip.StartText} to {clip.EndText}{(clip.SourceReady ? "" : ", source unavailable")}"
        : $"Track lanes, {ClipCountText}";

    public string ZoomText => $"{Zoom}%";
    public string TrackCountText => Tracks.Count == 1 ? "1 track" : $"{Tracks.Count} tracks";
    [ObservableProperty]
    public partial string PlayheadText { get; set; } = "00:00.000";
    [ObservableProperty]
    public partial long PlayheadMs { get; set; }
    public void ApplyPreview(TimelinePreviewView? preview)
    {
        if (preview is null) return;
        PlayheadText = preview.PositionText;
        PlayheadMs = preview.PositionMs;
    }
    public void SeekRuler(long milliseconds) => _engine.Run(s => s.SeekTimelineMilliseconds(milliseconds));
    public bool HasTracks => Tracks.Count > 0;

    partial void OnZoomChanged(int value)
    {
        var clamped = Math.Clamp(value, TimelineScale.MinZoom, TimelineScale.MaxZoom);
        if (clamped != value)
        {
            Zoom = clamped;
            return;
        }
        OnPropertyChanged(nameof(ZoomText));
    }

    partial void OnSelectedTrackChanged(TrackViewModel? value)
    {
        if (!_applying && value is not null && !value.Track.Selected)
        {
            _engine.Run(session => session.SelectTrack(value.Id));
        }
    }

    private bool CanAddTrack() => EngineReady && Tracks.Count < MaxTracks;

    [RelayCommand(CanExecute = nameof(CanAddTrack))]
    private void AddVideoTrack() => _engine.Run(session => session.AddTrack(TrackKind.Video));

    [RelayCommand(CanExecute = nameof(CanAddTrack))]
    private void AddAudioTrack() => _engine.Run(session => session.AddTrack(TrackKind.Audio));

    private bool CanAddClip() => EngineReady && Source is { Availability: "ready" } && TargetTrack is not null;

    [RelayCommand(CanExecute = nameof(CanAddClip))]
    private void AddClip()
    {
        if (Source is { } source && TargetTrack is { } track)
        {
            _engine.Run(session => session.InsertClip(source.Id, track.Id));
        }
    }

    private bool CanRemoveClip() => EngineReady && SelectedClip is not null;

    [RelayCommand(CanExecute = nameof(CanRemoveClip))]
    private void RemoveClip()
    {
        if (SelectedClip is { } clip)
        {
            _engine.Run(session => session.RemoveClip(clip.Id));
        }
    }

    private bool CanUndo() => EngineReady && History.CanUndo;
    private bool CanRedo() => EngineReady && History.CanRedo;

    [RelayCommand(CanExecute = nameof(CanUndo))]
    private void Undo() => _engine.Run(session => session.Undo());

    [RelayCommand(CanExecute = nameof(CanRedo))]
    private void Redo() => _engine.Run(session => session.Redo());

    public void SelectClip(ClipView clip)
    {
        if (!clip.Selected)
        {
            _engine.Run(session => session.SelectClip(clip.Id));
        }
    }

    /// <summary>The insertion target named in the class summary.</summary>
    public static TrackViewModel? ChooseTarget(IReadOnlyList<TrackViewModel> tracks, ClipView? selectedClip) =>
        tracks.FirstOrDefault(t => t.Track.Selected && t.IsVideo)
        ?? (selectedClip is null ? null : tracks.FirstOrDefault(t => t.Id == selectedClip.TrackId && t.IsVideo))
        ?? tracks.FirstOrDefault(t => t.IsVideo);

    private bool CanZoomIn() => Zoom < TimelineScale.MaxZoom;
    private bool CanZoomOut() => Zoom > TimelineScale.MinZoom;

    [RelayCommand(CanExecute = nameof(CanZoomIn))]
    private void ZoomIn() => Zoom = Math.Min(TimelineScale.MaxZoom, Zoom + TimelineScale.ZoomStep);

    [RelayCommand(CanExecute = nameof(CanZoomOut))]
    private void ZoomOut() => Zoom = Math.Max(TimelineScale.MinZoom, Zoom - TimelineScale.ZoomStep);

    [RelayCommand]
    private void ResetZoom() => Zoom = 100;

    public void SelectIndex(int index)
    {
        if (index >= 0 && index < Tracks.Count)
        {
            SelectedTrack = Tracks[index];
        }
    }

    public void Apply(SessionView view, TimelineView? timeline)
    {
        _applying = true;
        try
        {
            EngineReady = view.EngineReady;
            Sync.Items(Tracks, view.Tracks, t => t.Id, t => t.Id, t => new TrackViewModel(t), (item, t) => item.Track = t);
            Lanes = view.Tracks.Select(t => new LaneInfo(t.IsVideo, t.Selected, t.LaneLabel)).ToArray();
            SelectedTrack = Tracks.FirstOrDefault(t => t.Track.Selected);
            var clips = timeline?.Clips ?? [];
            if (!ReferenceEquals(clips, Clips)) { Clips = clips; }
            SelectedClip = clips.FirstOrDefault(c => c.Selected);
            Source = view.Media.FirstOrDefault(m => m.Selected);
            TargetTrack = ChooseTarget(Tracks, SelectedClip);
            History = view.History ?? new HistoryStatus(false, false, null, null, 0);
        }
        finally
        {
            _applying = false;
        }
        OnPropertyChanged(nameof(TrackCountText));
        OnPropertyChanged(nameof(HasTracks));
        OnPropertyChanged(nameof(ClipCountText));
        OnPropertyChanged(nameof(LanesAccessibleName));
        OnPropertyChanged(nameof(AddClipTip));
        AddVideoTrackCommand.NotifyCanExecuteChanged();
        AddAudioTrackCommand.NotifyCanExecuteChanged();
        AddClipCommand.NotifyCanExecuteChanged();
    }
}

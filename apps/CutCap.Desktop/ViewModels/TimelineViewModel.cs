using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Controls;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

public sealed partial class TrackViewModel(TrackView track, EngineHost? engine = null) : ObservableObject
{
    public string Id { get; } = track.Id;

    // A volume drag: the slider follows the pointer while Rust hears the
    // previews; the value shown beside it is always the engine's.
    private bool _dragging;
    private double _dragDb;

    [ObservableProperty]
    public partial TrackView Track { get; set; } = track;

    public string Name => Track.Name;
    public string Badge => Track.Badge;
    public bool IsVideo => Track.IsVideo;
    public bool IsAudio => !Track.IsVideo;
    // Lanes with clips carry no label (the clips are drawn instead).
    public string AccessibleName => Track.LaneLabel.Length == 0 ? Track.Name : $"{Track.Name}, {Track.LaneLabel}";

    public bool Muted => Track.Muted;
    public bool Soloed => Track.Soloed;
    public bool Audible => Track.Audible;
    public string GainText => Track.GainText;
    public string MuteName => Muted ? $"Unmute {Name}" : $"Mute {Name}";
    public string SoloName => Soloed ? $"Remove solo from {Name}" : $"Solo {Name}";
    public string VolumeName => $"{Name} volume";
    public string MuteTip => Muted ? "Muted: this track is silent. Click to unmute." : "Mute this track";
    public string SoloTip => Soloed
        ? "Soloed: only soloed tracks play. Mute still wins. Click to remove."
        : "Solo: play only soloed tracks";
    public string VolumeTip => $"Volume {GainText}, from −60 to +6 dB. Esc cancels a drag.";

    /// <summary>Slider position in dB. Rust owns the value; this forwards a drag as previews.</summary>
    public double GainDb
    {
        get => _dragging ? _dragDb : Track.GainCdb / 100.0;
        set
        {
            var db = Math.Clamp(Math.Round(value * 2) / 2, -60, 6);
            if (Math.Abs(db - GainDb) < 0.001)
            {
                return;
            }
            _dragging = true;
            _dragDb = db;
            OnPropertyChanged();
            var cdb = Cdb(db);
            engine?.Run(session => session.PreviewTrackGain(Id, cdb));
        }
    }

    private static int Cdb(double db) => (int)Math.Round(db * 100);

    /// <summary>Ends a volume gesture with one undoable change. No-op outside a drag.</summary>
    public void CommitGain()
    {
        if (!_dragging)
        {
            return;
        }
        _dragging = false;
        var cdb = Cdb(_dragDb);
        engine?.Run(session => session.CommitTrackGain(Id, cdb));
        OnPropertyChanged(nameof(GainDb));
    }

    /// <summary>Abandons a volume gesture; the stored value applies again.</summary>
    public bool CancelGain()
    {
        if (!_dragging)
        {
            return false;
        }
        _dragging = false;
        engine?.Run(session => session.CancelTrackGain());
        OnPropertyChanged(nameof(GainDb));
        return true;
    }

    [RelayCommand]
    private void ToggleMute()
    {
        var muted = !Muted;
        engine?.Run(session => session.SetTrackMute(Id, muted));
    }

    [RelayCommand]
    private void ToggleSolo()
    {
        var soloed = !Soloed;
        engine?.Run(session => session.SetTrackSolo(Id, soloed));
    }

    partial void OnTrackChanged(TrackView value)
    {
        OnPropertyChanged(nameof(Name));
        OnPropertyChanged(nameof(Badge));
        OnPropertyChanged(nameof(IsVideo));
        OnPropertyChanged(nameof(IsAudio));
        OnPropertyChanged(nameof(AccessibleName));
        OnPropertyChanged(nameof(Muted));
        OnPropertyChanged(nameof(Soloed));
        OnPropertyChanged(nameof(Audible));
        OnPropertyChanged(nameof(GainText));
        OnPropertyChanged(nameof(MuteName));
        OnPropertyChanged(nameof(SoloName));
        OnPropertyChanged(nameof(VolumeName));
        OnPropertyChanged(nameof(MuteTip));
        OnPropertyChanged(nameof(SoloTip));
        OnPropertyChanged(nameof(VolumeTip));
        if (!_dragging)
        {
            OnPropertyChanged(nameof(GainDb));
        }
    }
}

/// <summary>
/// Timeline over the engine's tracks and clips. Every clip, selection and
/// history value comes from the engine; this only forwards commands. Zoom is
/// presentation state. The playhead is the Rust-owned timeline position; the
/// source monitor never moves it. Gestures in the lanes (move, trim, drop)
/// arrive here once, on release, as a single edit command (one undo step).
/// Split uses the Rust-owned playhead; Rust also decides whether it is possible.
/// <para>Insertion policy (explicit, append-to-end): the whole selected ready
/// source is placed after the last clip of the target video track. The target
/// is the selected track if it is a video track, else the track of the selected
/// clip, else the first video track. An audio-only source uses audio tracks by
/// the same rule.</para>
/// </summary>
public sealed partial class TimelineViewModel : ObservableObject
{
    public const int MaxTracks = 128;
    private readonly EngineHost _engine;
    private bool _applying;

    public TimelineViewModel(EngineHost engine)
    {
        _engine = engine;
        engine.ThumbnailsChanged += (_, _) => ThumbnailsChanged?.Invoke(this, EventArgs.Empty);
        engine.WaveformsChanged += (_, _) => WaveformsChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>Filmstrip thumbnails (presentation cache, never project state).</summary>
    public ThumbnailCache Thumbnails => _engine.Thumbnails;

    /// <summary>New thumbnails arrived.</summary>
    public event EventHandler? ThumbnailsChanged;

    /// <summary>Forwards the lanes' viewport demand to the engine's low-priority worker.</summary>
    public void RequestThumbnails(IReadOnlyList<ThumbnailWant> wants) => _engine.RequestThumbnails(wants);

    /// <summary>Waveform peaks (presentation cache, never project state).</summary>
    public WaveformCache Waveforms => _engine.Waveforms;

    /// <summary>New waveform chunks arrived.</summary>
    public event EventHandler? WaveformsChanged;

    /// <summary>Forwards the lanes' waveform demand to the engine's low-priority worker.</summary>
    public void RequestWaveforms(IReadOnlyList<WaveformWant> wants) => _engine.RequestWaveforms(wants);

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
        nameof(RemoveClipCommand), nameof(UndoCommand), nameof(RedoCommand), nameof(SplitCommand), nameof(UnlinkCommand))]
    public partial bool EngineReady { get; set; }

    /// <summary>Clips in project order, as the engine presents them.</summary>
    [ObservableProperty]
    public partial IReadOnlyList<ClipView> Clips { get; set; } = [];

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(RemoveClipCommand), nameof(SplitCommand), nameof(UnlinkCommand))]
    [NotifyPropertyChangedFor(nameof(RemoveClipTip))]
    public partial ClipView? SelectedClip { get; set; }

    /// <summary>Rust's split availability for the selected clip at the playhead.</summary>
    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(SplitCommand), nameof(UnlinkCommand), nameof(LinkCommand))]
    [NotifyPropertyChangedFor(nameof(SplitTip), nameof(UnlinkTip), nameof(ShowUnlink), nameof(ShowLink))]
    public partial EditingView Editing { get; set; } = new(false, "Select a clip, then place the playhead inside it to split (Ctrl+B).");

    /// <summary>The live gesture hint while dragging, otherwise the editing help.</summary>
    [ObservableProperty]
    public partial string EditHint { get; set; } = DefaultHint;

    public const string DefaultHint = "Drag clips to move · drag edges to trim · Shift: no snapping · Esc: cancel";

    public string SplitTip => Editing.SplitDetail;
    public string UnlinkTip => Editing.CanUnlink
        ? "Unlink this clip's video and audio so each can be edited alone"
        : "Select a linked clip to unlink its video and audio";
    public bool ShowUnlink => Editing.CanUnlink;
    public bool ShowLink => Editing.LinkCandidate is not null;

    /// <summary>The mix hit the output limit in this playback (Rust's counter).</summary>
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ClippingTip))]
    public partial ulong ClippedSamples { get; set; }

    public bool Clipping => ClippedSamples > 0;
    public string ClippingTip => $"The mix exceeded full scale: {ClippedSamples:N0} samples were clipped. Lower a track.";

    partial void OnClippedSamplesChanged(ulong value) => OnPropertyChanged(nameof(Clipping));

    /// <summary>Mixer diagnostics change every block; only the clip indicator is shown.</summary>
    public void ApplyAudio(AudioView? audio) => ClippedSamples = audio?.Mix?.ClippedSamples ?? 0;

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
        ({ AudioOnly: true }, null) => "Add an audio track first",
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
    /// <summary>Exact playhead (project ticks), a snap target; never derived in C#.</summary>
    [ObservableProperty]
    public partial long PlayheadTicks { get; set; }
    public void ApplyPreview(TimelinePreviewView? preview)
    {
        if (preview is null) return;
        PlayheadText = preview.PositionText;
        PlayheadMs = preview.PositionMs;
        PlayheadTicks = preview.PositionTicks;
    }

    /// <summary>Split availability can change with every playhead update.</summary>
    public void ApplyEditing(EditingView? editing)
    {
        if (editing is not null && editing != Editing) Editing = editing;
    }

    public void ShowGestureHint(string? hint) => EditHint = hint ?? DefaultHint;

    private bool CanSplit() => EngineReady && SelectedClip is not null && Editing.SplitEnabled;

    private bool CanUnlink() => EngineReady && SelectedClip is not null && Editing.CanUnlink;

    [RelayCommand(CanExecute = nameof(CanUnlink))]
    private void Unlink()
    {
        if (SelectedClip is { } clip)
        {
            _engine.Run(session => session.UnlinkClip(clip.Id));
        }
    }

    private bool CanLink() => EngineReady && SelectedClip is not null && Editing.LinkCandidate is not null;

    [RelayCommand(CanExecute = nameof(CanLink))]
    private void Link()
    {
        if (SelectedClip is { } clip && Editing.LinkCandidate is { } partner)
        {
            _engine.Run(session => session.LinkClips(clip.Id, partner));
        }
    }

    /// <summary>
    /// Places one chosen stream at the playhead, on the selected audio track
    /// or (Rust's rule) the first audio track free for its whole interval.
    /// </summary>
    public void PlaceAudioStream(string mediaId, uint stream)
    {
        var track = Tracks.FirstOrDefault(t => t.Track.Selected && t.IsAudio)?.Id;
        var position = TimelinePosition.Ticks(PlayheadTicks);
        _engine.Run(session => session.PlaceAudioStream(mediaId, stream, track, position));
    }

    [RelayCommand(CanExecute = nameof(CanSplit))]
    private void Split()
    {
        if (SelectedClip is { } clip)
        {
            _engine.Run(session => session.SplitClipAtPlayhead(clip.Id));
        }
    }

    /// <summary>Sends one completed gesture to the engine; Rust validates and applies it atomically.</summary>
    public void Commit(GesturePreview edit)
    {
        switch (edit.Kind)
        {
            case GestureKind.Move when edit.ClipId is { } clip && edit.TrackId is { } track:
                _engine.Run(s => s.MoveClip(clip, track, edit.Position, edit.Anchor));
                break;
            case GestureKind.TrimStart when edit.ClipId is { } clip:
                _engine.Run(s => s.TrimClip(clip, ClipEdge.Start, edit.Position));
                break;
            case GestureKind.TrimEnd when edit.ClipId is { } clip:
                _engine.Run(s => s.TrimClip(clip, ClipEdge.End, edit.Position));
                break;
            case GestureKind.Place when edit.MediaId is { } media && edit.TrackId is { } track:
                _engine.Run(s => s.PlaceClip(media, track, edit.Position));
                break;
        }
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
    public static TrackViewModel? ChooseTarget(IReadOnlyList<TrackViewModel> tracks, ClipView? selectedClip, bool audioOnly = false) =>
        tracks.FirstOrDefault(t => t.Track.Selected && t.IsAudio == audioOnly)
        ?? (selectedClip is null ? null : tracks.FirstOrDefault(t => t.Id == selectedClip.TrackId && t.IsAudio == audioOnly))
        ?? tracks.FirstOrDefault(t => t.IsAudio == audioOnly);

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
            Sync.Items(Tracks, view.Tracks, t => t.Id, t => t.Id, t => new TrackViewModel(t, _engine), (item, t) => item.Track = t);
            Lanes = view.Tracks.Select(t => new LaneInfo(t.IsVideo, t.Selected, t.LaneLabel, t.Id, t.IsVideo || t.Audible)).ToArray();
            SelectedTrack = Tracks.FirstOrDefault(t => t.Track.Selected);
            var clips = timeline?.Clips ?? [];
            if (!ReferenceEquals(clips, Clips)) { Clips = clips; }
            SelectedClip = clips.FirstOrDefault(c => c.Selected);
            Source = view.Media.FirstOrDefault(m => m.Selected);
            TargetTrack = ChooseTarget(Tracks, SelectedClip, Source?.AudioOnly ?? false);
            History = view.History ?? new HistoryStatus(false, false, null, null, 0);
            ApplyEditing(view.Editing);
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

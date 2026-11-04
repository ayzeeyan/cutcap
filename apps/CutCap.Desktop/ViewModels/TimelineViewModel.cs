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
    public string AccessibleName => $"{Track.Name}, empty {(Track.IsVideo ? "video" : "audio")} track";

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
/// Timeline shell over the engine's tracks. Zoom is presentation state; the
/// playhead stays at timeline zero until a timeline transport exists, and the
/// source monitor never moves it.
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
    [NotifyCanExecuteChangedFor(nameof(AddVideoTrackCommand), nameof(AddAudioTrackCommand))]
    public partial bool EngineReady { get; set; }

    public string ZoomText => $"{Zoom}%";
    public string TrackCountText => Tracks.Count == 1 ? "1 track" : $"{Tracks.Count} tracks";
    public string PlayheadText => "00:00.000";
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

    public void Apply(SessionView view)
    {
        _applying = true;
        try
        {
            EngineReady = view.EngineReady;
            Sync.Items(Tracks, view.Tracks, t => t.Id, t => t.Id, t => new TrackViewModel(t), (item, t) => item.Track = t);
            Lanes = view.Tracks.Select(t => new LaneInfo(t.IsVideo, t.Selected)).ToArray();
            SelectedTrack = Tracks.FirstOrDefault(t => t.Track.Selected);
        }
        finally
        {
            _applying = false;
        }
        OnPropertyChanged(nameof(TrackCountText));
        OnPropertyChanged(nameof(HasTracks));
        AddVideoTrackCommand.NotifyCanExecuteChanged();
        AddAudioTrackCommand.NotifyCanExecuteChanged();
    }
}

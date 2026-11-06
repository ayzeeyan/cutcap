using System.Diagnostics;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>
/// Source/Timeline monitor presentation. Source transport and timeline seeking
/// use separate Rust-owned positions; this view model never evaluates clips.
/// Displayed values come from the engine view (<see cref="Transport.Map"/>), so a command
/// that was merely accepted never shows as done, user input can never leave a
/// stale control behind (the Phase 1 slider/text regression), and no frontend
/// timer invents media time.
/// </summary>
public sealed partial class PlayerViewModel : ObservableObject
{
    private readonly EngineHost _engine;
    private readonly ScrubController _scrub;
    private bool _applying;
    private long _requested = -1;
    private long _display;
    private string _title = "";
    private long _duration;
    private PreviewMode _mode;
    private long _timelineEndTicks;
    private long _timelineRequestedTicks = -1;

    public PlayerViewModel(EngineHost engine)
    {
        _engine = engine;
        _scrub = new ScrubController(() => Stopwatch.GetTimestamp() * 1000 / Stopwatch.Frequency, Seek);
        engine.FrameChanged += (_, _) => FrameChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>
    /// A newer decoded frame is available. The view pulls pixels straight into its
    /// bitmap through <see cref="Session"/>; frames never become view-model properties.
    /// </summary>
    public event EventHandler? FrameChanged;

    public EngineSession? Session => _engine.Session;

    public IRelayCommand? ImportCommand { get; set; }

    [ObservableProperty]
    public partial int PreviewTabIndex { get; set; }
    partial void OnPreviewTabIndexChanged(int value)
    {
        if (!_applying) { _engine.Run(s => s.SetPreviewMode(value == 1 ? PreviewMode.Timeline : PreviewMode.Source)); }
    }
    public string SeekName => _mode == PreviewMode.Timeline ? "Timeline position" : "Source position";
    public string GoToName => _mode == PreviewMode.Timeline ? "Go to timeline time" : "Go to source time";
    public string FrameName => _mode == PreviewMode.Timeline ? "Decoded timeline frame" : "Decoded source frame";
    public string GoToHint => _mode == PreviewMode.Timeline
        ? "Accepts milliseconds, 1.5s, 00:01.500 or an exact integer followed by ticks. Enter requests a timeline frame."
        : "Accepts 1500 (milliseconds), 1.5s or 00:01.500. Enter requests the frame.";
    public string SeekHint => _mode == PreviewMode.Timeline
        ? "Seek the monitored video track while playing or paused. Milliseconds, timecode, or an exact integer followed by ticks."
        : "Source position. Drag or use arrows (100 ms) and Page keys (1 s); works while playing or paused.";

    [ObservableProperty]
    public partial bool Ready { get; set; }

    [ObservableProperty]
    public partial bool Busy { get; set; }

    [ObservableProperty]
    public partial bool HasFrame { get; set; }

    [ObservableProperty]
    public partial string Title { get; set; } = "No source selected";

    [ObservableProperty]
    public partial string Caption { get; set; } = "";

    [ObservableProperty]
    public partial string Status { get; set; } = "Select a video to preview";

    [ObservableProperty]
    public partial string Detail { get; set; } = "Import a local video into the media library.";

    /// <summary>Shown frame's source time while playing/paused, otherwise the requested position.</summary>
    [ObservableProperty]
    public partial string PositionText { get; set; } = "00:00.000";

    [ObservableProperty]
    public partial string PositionName { get; set; } = "Requested source position";

    [ObservableProperty]
    public partial string DurationText { get; set; } = "--:--.---";

    /// <summary>Go-to field text; overwritten only when the requested position changes.</summary>
    [ObservableProperty]
    public partial string GoToText { get; set; } = "00:00.000";

    [ObservableProperty]
    public partial double ScrubValue { get; set; }

    [ObservableProperty]
    public partial double ScrubMaximum { get; set; } = 1;

    [ObservableProperty]
    public partial bool CanScrub { get; set; }

    [ObservableProperty]
    public partial bool CanImport { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(PlayPauseCommand))]
    public partial bool CanPlay { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(StopCommand))]
    public partial bool CanStop { get; set; }

    [ObservableProperty]
    public partial bool ShowPause { get; set; }

    [ObservableProperty]
    public partial string PlayName { get; set; } = "Play";

    [ObservableProperty]
    public partial string PlayHint { get; set; } = "Select a source video to play it.";

    [ObservableProperty]
    public partial PlaybackState State { get; set; }

    /// <summary>Transient engine state (Starting, Seeking, Buffering, Ended, Failed); null when steady.</summary>
    [ObservableProperty]
    public partial string? Chip { get; set; }

    [ObservableProperty]
    public partial bool ChipBusy { get; set; }

    [ObservableProperty]
    public partial bool ShowIsolatedBusy { get; set; }

    [ObservableProperty]
    public partial string? PlaybackError { get; set; }

    public bool ShowPlayGlyph => !ShowPause;
    // The failure banner already carries the message; do not stack the chip under it.
    public bool ShowChip => Chip is not null && PlaybackError is null;
    public bool ShowEmptyState => !HasFrame;
    public bool ShowImportAction => !Ready && !Busy && CanImport;
    public bool ShowBusyBanner => HasFrame && ShowIsolatedBusy;
    public bool ShowDetailBanner => HasFrame && !ShowIsolatedBusy && BannerText.Length > 0;
    public string BannerText => PlaybackError is { } error ? $"Playback failed. {error}"
        : Detail.Length > 0 ? $"{Status}. {Detail}" : "";

    public bool IsScrubbing => _scrub.IsScrubbing;

    partial void OnHasFrameChanged(bool value) => NotifyStates();
    partial void OnBusyChanged(bool value) => NotifyStates();
    partial void OnReadyChanged(bool value) => NotifyStates();
    partial void OnDetailChanged(string value) => NotifyStates();
    partial void OnStatusChanged(string value) => NotifyStates();
    partial void OnCanImportChanged(bool value) => NotifyStates();
    partial void OnShowIsolatedBusyChanged(bool value) => NotifyStates();
    partial void OnPlaybackErrorChanged(string? value)
    {
        NotifyStates();
        OnPropertyChanged(nameof(ShowChip));
    }
    partial void OnShowPauseChanged(bool value) => OnPropertyChanged(nameof(ShowPlayGlyph));
    partial void OnChipChanged(string? value) => OnPropertyChanged(nameof(ShowChip));

    partial void OnScrubValueChanged(double value)
    {
        if (!_applying && Ready)
        {
            _scrub.Move((long)Math.Round(value));
        }
    }

    /// <summary>Pointer pressed on the scrubber: seeks are throttled until release.</summary>
    public void BeginScrub()
    {
        if (CanScrub)
        {
            _scrub.Begin();
        }
    }

    /// <summary>Pointer released: issue the final position if it was not sent yet.</summary>
    public void EndScrub() => _scrub.End();

    private long LastMs => Math.Max(0, _duration - 1);

    private void Seek(long milliseconds)
    {
        PreviewTrace.MarkRequest();
        PreviewTrace.Command("seek", milliseconds);
        _engine.Run(session => { if (_mode == PreviewMode.Timeline) session.SeekTimelineMilliseconds(milliseconds); else session.Seek(milliseconds); });
    }

    [RelayCommand]
    private void GoToStart() => Seek(0);

    // Relative steps start from what is on screen, not from a stale request.
    [RelayCommand]
    private void Back() => Seek(Math.Max(0, _display - 1000));

    [RelayCommand]
    private void Forward() => Seek(Math.Min(_display + 1000, _duration > 0 ? LastMs : 86_400_000));

    [RelayCommand]
    private void GoToEnd()
    {
        if (_mode == PreviewMode.Timeline) _engine.Run(s => s.SeekTimeline(_timelineEndTicks));
        else Seek(LastMs);
    }

    /// <summary>Accepts 1500 (milliseconds), 1.5s or 00:01.500; parsed by the engine.</summary>
    [RelayCommand]
    private void GoTo()
    {
        PreviewTrace.MarkRequest();
        PreviewTrace.Command("seek-text");
        _engine.Run(session => { if (_mode == PreviewMode.Timeline) session.SeekTimelineText(GoToText); else session.SeekText(GoToText); });
    }

    [RelayCommand]
    private void CancelPreview() => _engine.Run(session => { if (_mode == PreviewMode.Timeline) session.CancelTimelinePreview(); else session.CancelPreview(); });

    /// <summary>Toggles by the engine's intent: Pause while it intends to play, otherwise Play (Ended restarts at zero).</summary>
    [RelayCommand(CanExecute = nameof(CanPlay))]
    private void PlayPause()
    {
        if (ShowPause)
        {
            PreviewTrace.Command("pause");
            _engine.Run(session => { if (_mode == PreviewMode.Timeline) session.PauseTimeline(); else session.Pause(); });
        }
        else
        {
            PreviewTrace.MarkRequest();
            PreviewTrace.Command("play");
            _engine.Run(session => { if (_mode == PreviewMode.Timeline) session.PlayTimeline(); else session.Play(); });
        }
    }

    /// <summary>Stops the epoch; the engine then requests the zero-position frame.</summary>
    [RelayCommand(CanExecute = nameof(CanStop))]
    private void Stop()
    {
        PreviewTrace.Command("stop");
        _engine.Run(session => { if (_mode == PreviewMode.Timeline) session.StopTimeline(); else session.Stop(); });
    }

    public void ApplyMonitor(SourceView source, PlaybackView? playback, PreviewMode mode, TimelinePreviewView? timeline, bool canImport, bool engineReady, TimelinePlaybackView? timelinePlayback = null)
    {
        if (_mode != mode)
        {
            _scrub.Cancel(); _requested = -1; _timelineRequestedTicks = -1; _title = "";
            _mode = mode;
            OnPropertyChanged(nameof(SeekName)); OnPropertyChanged(nameof(SeekHint));
            OnPropertyChanged(nameof(GoToName)); OnPropertyChanged(nameof(GoToHint)); OnPropertyChanged(nameof(FrameName));
        }
        _mode = mode;
        _applying = true;
        PreviewTabIndex = mode == PreviewMode.Timeline ? 1 : 0;
        _applying = false;
        if (mode == PreviewMode.Source || timeline is null) { Apply(source, playback, canImport, engineReady); return; }
        var status = timeline.State switch
        {
            "starting" => "Starting timeline…", "playing" => "Playing timeline", "paused" => "Timeline paused",
            "seeking" => "Seeking timeline…", "buffering" => "Timeline buffering…", "ended" => "End of timeline",
            "evaluating" => "Evaluating timeline…", "loading" => "Decoding timeline frame…", "blank" => "Blank timeline frame",
            "unavailable" => "Timeline source unavailable", "no_frame" => "No source frame inside this clip", "failed" => "Timeline preview failed",
            "cancelled" => "Timeline preview cancelled", _ => "Timeline frame ready",
        };
        Apply(new SourceView(engineReady, timeline.Title, timeline.PositionMs, timeline.SeekMaxMs, false,
            timeline.Busy, timeline.HasFrame, status, timeline.Detail, timeline.Caption), null, false, engineReady);
        State = timelinePlayback?.State ?? PlaybackState.Stopped;
        CanPlay = engineReady && timeline.TrackId is not null && timeline.ExtentTicks > 0;
        CanStop = engineReady && (timeline.PositionTicks != 0 || State != PlaybackState.Stopped);
        ShowPause = timelinePlayback is { DesiredPlaying: true, State: PlaybackState.Starting or PlaybackState.Playing or PlaybackState.Seeking or PlaybackState.Buffering };
        PlayName = ShowPause ? "Pause timeline" : State == PlaybackState.Ended ? "Replay timeline" : "Play timeline";
        PlayHint = "Video only · plays the monitored track. Source playback keeps its own position.";
        PositionText = timeline.PositionText;
        PositionName = $"Timeline position, {timeline.PositionTicks} project ticks. Monitoring {timeline.TrackName}";
        DurationText = timeline.ExtentText;
        _timelineEndTicks = timeline.ExtentTicks;
        Chip = State switch { PlaybackState.Starting => "Starting…", PlaybackState.Seeking => "Seeking…", PlaybackState.Buffering => "Buffering…", PlaybackState.Ended => "End of timeline", PlaybackState.Failed => "Failed", _ => timeline.Busy ? "Loading…" : null };
        ChipBusy = State is PlaybackState.Starting or PlaybackState.Seeking or PlaybackState.Buffering || timeline.Busy;
        PlaybackError = timelinePlayback?.Error ?? (timeline.State == "failed" ? timeline.Detail : null);
        // A request changes independently of a running clock. Preserve typed text.
        var requestedTicks = State != PlaybackState.Stopped && timelinePlayback is not null ? timelinePlayback.RequestedTicks : timeline.PositionTicks;
        if (_timelineRequestedTicks != requestedTicks)
        {
            _timelineRequestedTicks = requestedTicks;
            GoToText = State != PlaybackState.Stopped && timelinePlayback is not null ? timelinePlayback.RequestedText : timeline.PositionInput;
        }
    }

    public void Apply(SourceView source, PlaybackView? playback, bool canImport, bool engineReady)
    {
        var transport = Transport.Map(source, playback, engineReady);
        _applying = true;
        try
        {
            if (source.Title != _title)
            {
                // A different source: an in-flight drag belongs to the old one.
                _scrub.Cancel();
                _title = source.Title;
                _requested = -1;
            }
            CanImport = canImport;
            Ready = source.Ready;
            Busy = source.Busy;
            HasFrame = source.HasFrame;
            Title = source.Ready ? source.Title : "No source selected";
            Caption = source.Caption;
            Status = source.Status;
            Detail = source.Detail;
            State = transport.State;
            CanPlay = transport.CanPlay;
            CanStop = transport.CanStop;
            ShowPause = transport.ShowPause;
            PlayName = transport.PlayName;
            PlayHint = transport.PlayHint;
            Chip = transport.Chip;
            ChipBusy = transport.ChipBusy;
            ShowIsolatedBusy = transport.ShowIsolatedBusy;
            PlaybackError = transport.Error;
            _duration = source.DurationMs;
            _display = transport.DisplayMs;
            ScrubMaximum = Math.Max(1, LastMs);
            CanScrub = source.Ready && source.DurationMs > 0;
            if (!_scrub.IsScrubbing)
            {
                ScrubValue = Math.Clamp(transport.SliderMs, 0, ScrubMaximum);
            }
            PositionText = Sync.Precise(transport.DisplayMs);
            PositionName = transport.DisplayIsFrame ? "Shown frame source time" : "Requested source position";
            DurationText = source.DurationMs > 0
                ? (source.DurationEstimated ? "≈" : "") + Sync.Precise(source.DurationMs)
                : "--:--.---";
            var requested = playback is { State: not PlaybackState.Stopped } ? playback.RequestedMs : source.PositionMs;
            if (requested != _requested && _mode != PreviewMode.Timeline)
            {
                _requested = requested;
                GoToText = Sync.Precise(requested);
            }
        }
        finally
        {
            _applying = false;
        }
    }

    private void NotifyStates()
    {
        OnPropertyChanged(nameof(ShowEmptyState));
        OnPropertyChanged(nameof(ShowImportAction));
        OnPropertyChanged(nameof(ShowBusyBanner));
        OnPropertyChanged(nameof(ShowDetailBanner));
        OnPropertyChanged(nameof(BannerText));
    }
}

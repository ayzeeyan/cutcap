using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>
/// Source monitor. Every position here is a source-media request; nothing in
/// this view model touches the timeline playhead. Displayed values always come
/// from the engine view, so user input can never leave a stale control behind
/// (the Phase 1 slider/text regression).
/// </summary>
public sealed partial class PlayerViewModel : ObservableObject
{
    private readonly EngineHost _engine;
    private bool _applying;
    private long _position = -1;
    private string _title = "";
    private long _duration;

    public PlayerViewModel(EngineHost engine)
    {
        _engine = engine;
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

    [ObservableProperty]
    public partial string PositionText { get; set; } = "00:00.000";

    [ObservableProperty]
    public partial string DurationText { get; set; } = "--:--.---";

    /// <summary>Go-to field text; overwritten whenever the requested position changes.</summary>
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

    public bool ShowEmptyState => !HasFrame;
    public bool ShowImportAction => !Ready && !Busy && CanImport;
    public bool ShowBusyBanner => HasFrame && Busy;
    public bool ShowDetailBanner => HasFrame && !Busy && Detail.Length > 0;
    public string BannerText => $"{Status}. {Detail}";

    partial void OnHasFrameChanged(bool value) => NotifyStates();
    partial void OnBusyChanged(bool value) => NotifyStates();
    partial void OnReadyChanged(bool value) => NotifyStates();
    partial void OnDetailChanged(string value) => NotifyStates();
    partial void OnCanImportChanged(bool value) => NotifyStates();

    partial void OnScrubValueChanged(double value)
    {
        if (!_applying && Ready)
        {
            Seek((long)Math.Round(value));
        }
    }

    private long LastMs => Math.Max(0, _duration - 1);

    private void Seek(long milliseconds)
    {
        PreviewTrace.MarkRequest();
        _engine.Run(session => session.Seek(milliseconds));
    }

    [RelayCommand]
    private void GoToStart() => Seek(0);

    [RelayCommand]
    private void Back() => Seek(Math.Max(0, _position - 1000));

    [RelayCommand]
    private void Forward() => Seek(Math.Min(_position + 1000, _duration > 0 ? LastMs : 86_400_000));

    [RelayCommand]
    private void GoToEnd() => Seek(LastMs);

    /// <summary>Accepts 1500 (milliseconds), 1.5s or 00:01.500; parsed by the engine.</summary>
    [RelayCommand]
    private void GoTo()
    {
        PreviewTrace.MarkRequest();
        _engine.Run(session => session.SeekText(GoToText));
    }

    [RelayCommand]
    private void CancelPreview() => _engine.Run(session => session.CancelPreview());

    public void Apply(SourceView source, bool canImport)
    {
        _applying = true;
        try
        {
            CanImport = canImport;
            Ready = source.Ready;
            Busy = source.Busy;
            HasFrame = source.HasFrame;
            Title = source.Ready ? source.Title : "No source selected";
            Caption = source.Caption;
            Status = source.Status;
            Detail = source.Detail;
            _duration = source.DurationMs;
            ScrubMaximum = Math.Max(1, LastMs);
            CanScrub = source.Ready && source.DurationMs > 0;
            ScrubValue = Math.Clamp(source.PositionMs, 0, ScrubMaximum);
            PositionText = Sync.Precise(source.PositionMs);
            DurationText = source.DurationMs > 0
                ? (source.DurationEstimated ? "≈" : "") + Sync.Precise(source.DurationMs)
                : "--:--.---";
            if (source.PositionMs != _position || source.Title != _title)
            {
                _position = source.PositionMs;
                _title = source.Title;
                GoToText = Sync.Precise(source.PositionMs);
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

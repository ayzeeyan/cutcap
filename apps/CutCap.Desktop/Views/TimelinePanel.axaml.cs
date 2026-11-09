using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Interactivity;
using CutCap.Desktop.ViewModels;

namespace CutCap.Desktop.Views;

/// <summary>
/// View glue only: keeps the ruler and the track headers aligned with the
/// scrolling lanes. No timeline state lives here.
/// </summary>
public partial class TimelinePanel : UserControl
{
    private bool _syncing;

    public TimelinePanel()
    {
        InitializeComponent();
        LanesScroll.ScrollChanged += (_, _) => FollowLanes();
        HeaderScroll.ScrollChanged += (_, _) => FollowHeaders();
        LanesScroll.PropertyChanged += (_, e) =>
        {
            if (e.Property == ScrollViewer.ViewportProperty)
            {
                Lanes.Viewport = LanesScroll.Viewport;
            }
        };
        Lanes.LanePressed += (_, index) => (DataContext as TimelineViewModel)?.SelectIndex(index);
        Lanes.ClipPressed += (_, clip) => (DataContext as TimelineViewModel)?.SelectClip(clip);
        Lanes.EditCommitted += (_, edit) => (DataContext as TimelineViewModel)?.Commit(edit);
        Lanes.GestureHint += (_, hint) => (DataContext as TimelineViewModel)?.ShowGestureHint(hint);
        Lanes.AutoScrollRequested += (_, dx) =>
        {
            var max = Math.Max(0, LanesScroll.Extent.Width - LanesScroll.Viewport.Width);
            LanesScroll.Offset = new Vector(Math.Clamp(LanesScroll.Offset.X + dx, 0, max), LanesScroll.Offset.Y);
            FollowLanes();
        };
        Ruler.PositionPressed += (_, milliseconds) => (DataContext as TimelineViewModel)?.SeekRuler(milliseconds);
        Lanes.ThumbnailDemand += (_, wants) => (DataContext as TimelineViewModel)?.RequestThumbnails(wants);
        Lanes.WaveformDemand += (_, wants) => (DataContext as TimelineViewModel)?.RequestWaveforms(wants);
        // A volume gesture ends when the pointer or key is released; one commit, one undo step.
        Headers.AddHandler(PointerReleasedEvent, (_, _) => EndGainGestures(), RoutingStrategies.Tunnel | RoutingStrategies.Bubble, true);
        Headers.AddHandler(PointerCaptureLostEvent, (_, _) => EndGainGestures(), RoutingStrategies.Direct | RoutingStrategies.Bubble, true);
        Headers.AddHandler(KeyUpEvent, (_, _) => EndGainGestures(), RoutingStrategies.Tunnel | RoutingStrategies.Bubble, true);
        Headers.AddHandler(KeyDownEvent, OnHeaderKeyDown, RoutingStrategies.Tunnel);
    }

    private void EndGainGestures()
    {
        foreach (var track in (DataContext as TimelineViewModel)?.Tracks ?? [])
        {
            track.CommitGain();
        }
    }

    private void OnHeaderKeyDown(object? sender, KeyEventArgs e)
    {
        if (e.Key != Key.Escape)
        {
            return;
        }
        foreach (var track in (DataContext as TimelineViewModel)?.Tracks ?? [])
        {
            e.Handled |= track.CancelGain();
        }
    }

    private TimelineViewModel? _timeline;

    protected override void OnDataContextChanged(EventArgs e)
    {
        base.OnDataContextChanged(e);
        if (_timeline is not null)
        {
            _timeline.ThumbnailsChanged -= OnThumbnailsChanged;
            _timeline.WaveformsChanged -= OnThumbnailsChanged;
        }
        _timeline = DataContext as TimelineViewModel;
        Lanes.Thumbnails = _timeline?.Thumbnails;
        Lanes.Waveforms = _timeline?.Waveforms;
        if (_timeline is not null)
        {
            _timeline.ThumbnailsChanged += OnThumbnailsChanged;
            _timeline.WaveformsChanged += OnThumbnailsChanged;
        }
    }

    private void OnThumbnailsChanged(object? sender, EventArgs e) => Lanes.ThumbnailsArrived();

    private void FollowLanes()
    {
        if (_syncing)
        {
            return;
        }
        _syncing = true;
        Ruler.Offset = LanesScroll.Offset.X;
        Lanes.ScrollOffset = LanesScroll.Offset;
        HeaderScroll.Offset = new Vector(0, LanesScroll.Offset.Y);
        _syncing = false;
    }

    // Wheel over the headers scrolls the lanes vertically, too.
    private void FollowHeaders()
    {
        if (_syncing)
        {
            return;
        }
        _syncing = true;
        LanesScroll.Offset = new Vector(LanesScroll.Offset.X, HeaderScroll.Offset.Y);
        _syncing = false;
    }
}

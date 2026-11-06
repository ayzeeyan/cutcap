using Avalonia;
using Avalonia.Controls;
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
    }

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

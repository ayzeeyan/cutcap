using System.Globalization;
using Avalonia;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media;

namespace CutCap.Desktop.Controls;

/// <summary>What the lanes need to know about one track; immutable per engine revision.</summary>
public readonly record struct LaneInfo(bool IsVideo, bool Selected);

/// <summary>
/// Custom-drawn track lanes: one band per track and the timeline playhead.
/// One visual for the whole surface, drawing only what intersects the viewport,
/// so future clip surfaces can be added without a control per clip.
/// </summary>
public sealed class TimelineLanes : Control
{
    public const double TrackHeight = 50;

    public static readonly StyledProperty<IReadOnlyList<LaneInfo>?> LanesProperty =
        AvaloniaProperty.Register<TimelineLanes, IReadOnlyList<LaneInfo>?>(nameof(Lanes));

    public static readonly StyledProperty<int> ZoomProperty =
        AvaloniaProperty.Register<TimelineLanes, int>(nameof(Zoom), 100);

    public static readonly StyledProperty<Size> ViewportProperty =
        AvaloniaProperty.Register<TimelineLanes, Size>(nameof(Viewport));

    private static readonly IBrush BandBrush = new SolidColorBrush(Color.Parse("#1E2124"));
    private static readonly IBrush SelectedBrush = new SolidColorBrush(Color.Parse("#102629"));
    private static readonly IPen SelectedPen = new Pen(new SolidColorBrush(Color.Parse("#7322D9E6")), 1);
    private static readonly IBrush LabelBrush = new SolidColorBrush(Color.Parse("#868E96"));
    private static readonly IPen PlayheadPen = new Pen(Brushes.White, 1);
    private static readonly Typeface Ui = new("fonts:Inter#Inter");

    /// <summary>Raised with the track index when a lane is clicked.</summary>
    public event EventHandler<int>? LanePressed;

    static TimelineLanes()
    {
        AffectsRender<TimelineLanes>(LanesProperty, ZoomProperty);
        AffectsMeasure<TimelineLanes>(LanesProperty, ZoomProperty, ViewportProperty);
    }

    public IReadOnlyList<LaneInfo>? Lanes
    {
        get => GetValue(LanesProperty);
        set => SetValue(LanesProperty, value);
    }

    public int Zoom
    {
        get => GetValue(ZoomProperty);
        set => SetValue(ZoomProperty, value);
    }

    /// <summary>The scroll viewport, so the surface always fills it.</summary>
    public Size Viewport
    {
        get => GetValue(ViewportProperty);
        set => SetValue(ViewportProperty, value);
    }

    protected override Size MeasureOverride(Size availableSize)
    {
        var scale = TimelineScale.ForZoom(Zoom);
        var count = Lanes?.Count ?? 0;
        return new Size(scale.ContentWidth(Viewport.Width), Math.Max(Viewport.Height, count * TrackHeight + 12));
    }

    protected override void OnPointerPressed(PointerPressedEventArgs e)
    {
        base.OnPointerPressed(e);
        var index = (int)Math.Floor(e.GetPosition(this).Y / TrackHeight);
        if (Lanes is { } lanes && index >= 0 && index < lanes.Count)
        {
            LanePressed?.Invoke(this, index);
            e.Handled = true;
        }
    }

    public override void Render(DrawingContext context)
    {
        var lanes = Lanes ?? [];
        var scale = TimelineScale.ForZoom(Zoom);
        var width = Bounds.Width;
        for (var index = 0; index < lanes.Count; index++)
        {
            var lane = lanes[index];
            var band = new Rect(2, index * TrackHeight + 5, Math.Max(0, width - 4), TrackHeight - 10);
            context.DrawRectangle(lane.Selected ? SelectedBrush : BandBrush, lane.Selected ? SelectedPen : null, band, 4, 4);
            var label = new FormattedText(lane.IsVideo ? "Empty video track" : "Empty audio track",
                CultureInfo.CurrentCulture, FlowDirection.LeftToRight, Ui, 11, LabelBrush);
            context.DrawText(label, new Point(scale.TimeToX(0) + 14, band.Y + (band.Height - label.Height) / 2));
        }
        var x = Math.Round(scale.TimeToX(0)) + 0.5;
        context.DrawLine(PlayheadPen, new Point(x, 0), new Point(x, Math.Max(TrackHeight, lanes.Count * TrackHeight)));
    }
}

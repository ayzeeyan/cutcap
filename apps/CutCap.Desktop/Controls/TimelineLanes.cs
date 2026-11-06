using System.Globalization;
using Avalonia;
using Avalonia.Automation.Peers;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media;
using CutCap.Interop;

namespace CutCap.Desktop.Controls;

/// <summary>What the lanes need to know about one track; immutable per engine revision.</summary>
public readonly record struct LaneInfo(bool IsVideo, bool Selected, string LaneLabel);

/// <summary>
/// Custom-drawn track lanes, clips and the timeline playhead. One visual for the
/// whole surface: only the lanes and clips that intersect the scroll viewport
/// are drawn, so cost follows the screen rather than the clip count. Clips are
/// engine data only: no thumbnails or waveforms are drawn because none exist.
/// </summary>
public sealed class TimelineLanes : Control
{
    public static readonly StyledProperty<long> PlayheadMsProperty = AvaloniaProperty.Register<TimelineLanes, long>(nameof(PlayheadMs));
    public long PlayheadMs { get => GetValue(PlayheadMsProperty); set => SetValue(PlayheadMsProperty, value); }
    public const double TrackHeight = 50;
    /// <summary>Space after the last clip so the end of the timeline is reachable.</summary>
    public const double TrailingSpace = 240;

    public static readonly StyledProperty<IReadOnlyList<LaneInfo>?> LanesProperty =
        AvaloniaProperty.Register<TimelineLanes, IReadOnlyList<LaneInfo>?>(nameof(Lanes));

    public static readonly StyledProperty<IReadOnlyList<ClipView>?> ClipsProperty =
        AvaloniaProperty.Register<TimelineLanes, IReadOnlyList<ClipView>?>(nameof(Clips));

    public static readonly StyledProperty<int> ZoomProperty =
        AvaloniaProperty.Register<TimelineLanes, int>(nameof(Zoom), 100);

    public static readonly StyledProperty<Size> ViewportProperty =
        AvaloniaProperty.Register<TimelineLanes, Size>(nameof(Viewport));

    public static readonly StyledProperty<Vector> ScrollOffsetProperty =
        AvaloniaProperty.Register<TimelineLanes, Vector>(nameof(ScrollOffset));

    private static readonly IBrush BandBrush = new SolidColorBrush(Color.Parse("#1E2124"));
    private static readonly IBrush SelectedBrush = new SolidColorBrush(Color.Parse("#102629"));
    private static readonly IPen SelectedPen = new Pen(new SolidColorBrush(Color.Parse("#7322D9E6")), 1);
    private static readonly IBrush LabelBrush = new SolidColorBrush(Color.Parse("#868E96"));
    private static readonly IPen PlayheadPen = new Pen(Brushes.White, 1);
    // Video clips use the teal track family; selection is the cyan accent.
    private static readonly IBrush ClipBrush = new SolidColorBrush(Color.Parse("#17616A"));
    private static readonly IBrush ClipHeaderBrush = new SolidColorBrush(Color.Parse("#1C7680"));
    private static readonly IPen ClipPen = new Pen(new SolidColorBrush(Color.Parse("#3CC4CF")), 1);
    private static readonly IPen ClipSelectedPen = new Pen(new SolidColorBrush(Color.Parse("#22D9E6")), 2);
    private static readonly IBrush ClipTextBrush = new SolidColorBrush(Color.Parse("#F1F3F4"));
    private static readonly IBrush ClipSubtleBrush = new SolidColorBrush(Color.Parse("#B8E9ED"));
    // Unavailable sources: a warm, desaturated clip with a dashed coral outline.
    private static readonly IBrush MissingBrush = new SolidColorBrush(Color.Parse("#3A2623"));
    private static readonly IPen MissingPen = new Pen(new SolidColorBrush(Color.Parse("#FF8F84")), 1, new DashStyle([3, 2], 0));
    private static readonly IBrush MissingTextBrush = new SolidColorBrush(Color.Parse("#FFC9C2"));
    private static readonly Typeface Ui = new("fonts:Inter#Inter");
    private static readonly Typeface UiBold = new("fonts:Inter#Inter", FontStyle.Normal, FontWeight.SemiBold);
    private static readonly Typeface Mono = new("Cascadia Mono, Consolas");

    private TimelineGeometry _geometry = TimelineGeometry.Empty;

    /// <summary>Raised with the track index when empty lane space is clicked.</summary>
    public event EventHandler<int>? LanePressed;

    /// <summary>Raised with the clip under the pointer.</summary>
    public event EventHandler<ClipView>? ClipPressed;

    static TimelineLanes()
    {
        AffectsRender<TimelineLanes>(LanesProperty, ClipsProperty, ZoomProperty, ScrollOffsetProperty, ViewportProperty, PlayheadMsProperty);
        AffectsMeasure<TimelineLanes>(LanesProperty, ClipsProperty, ZoomProperty, ViewportProperty);
    }

    public IReadOnlyList<LaneInfo>? Lanes
    {
        get => GetValue(LanesProperty);
        set => SetValue(LanesProperty, value);
    }

    public IReadOnlyList<ClipView>? Clips
    {
        get => GetValue(ClipsProperty);
        set => SetValue(ClipsProperty, value);
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

    /// <summary>The scroll offset, used to draw only what is visible.</summary>
    public Vector ScrollOffset
    {
        get => GetValue(ScrollOffsetProperty);
        set => SetValue(ScrollOffsetProperty, value);
    }

    /// <summary>Clip layout for the current clips and tracks (exposed for tests and diagnostics).</summary>
    public TimelineGeometry Geometry => _geometry;

    /// <summary>Clips drawn by the last render; proves viewport culling.</summary>
    public int LastDrawnClips { get; private set; }

    /// <summary>
    /// A plain Control has no automation peer, so the drawn clip surface was
    /// invisible to screen readers. Expose it as a named pane whose name the
    /// view model keeps current (clip count and the selected clip's range).
    /// </summary>
    protected override AutomationPeer OnCreateAutomationPeer() => new SurfacePeer(this);

    internal sealed class SurfacePeer(Control owner) : ControlAutomationPeer(owner)
    {
        protected override AutomationControlType GetAutomationControlTypeCore() => AutomationControlType.Pane;
        protected override bool IsContentElementCore() => true;
        protected override bool IsControlElementCore() => true;
    }

    protected override void OnPropertyChanged(AvaloniaPropertyChangedEventArgs change)
    {
        base.OnPropertyChanged(change);
        if (change.Property == ClipsProperty || change.Property == LanesProperty)
        {
            _geometry = new TimelineGeometry(Clips ?? [], Lanes?.Count ?? 0);
        }
    }

    public static double ContentWidth(TimelineScale scale, double viewportWidth, long endMs) =>
        Math.Max(scale.ContentWidth(viewportWidth), scale.TimeToX(endMs) + TrailingSpace);

    protected override Size MeasureOverride(Size availableSize)
    {
        var scale = TimelineScale.ForZoom(Zoom);
        var count = Lanes?.Count ?? 0;
        return new Size(ContentWidth(scale, Viewport.Width, _geometry.EndMs),
            Math.Max(Viewport.Height, count * TrackHeight + 12));
    }

    protected override void OnPointerPressed(PointerPressedEventArgs e)
    {
        base.OnPointerPressed(e);
        var point = e.GetPosition(this);
        var scale = TimelineScale.ForZoom(Zoom);
        if (_geometry.HitTest(scale, point, TrackHeight) is { } clip)
        {
            ClipPressed?.Invoke(this, clip);
            e.Handled = true;
            return;
        }
        var index = (int)Math.Floor(point.Y / TrackHeight);
        if (Lanes is { } lanes && index >= 0 && index < lanes.Count)
        {
            LanePressed?.Invoke(this, index);
            e.Handled = true;
        }
    }

    /// <summary>Opt-in diagnostic: CUTCAP_TRACE_TIMELINE=1 prints draw-call recording time per 30 renders.</summary>
    private static readonly bool TraceTimeline = Environment.GetEnvironmentVariable("CUTCAP_TRACE_TIMELINE") == "1";
    private readonly List<double> _renderMicroseconds = [];
    /// <summary>Opt-in draw-call recording diagnostic, not compositor raster/presentation time.</summary>
    public double LastDrawMicroseconds { get; private set; }
    public ulong DiagnosticRenderCount { get; private set; }

    public override void Render(DrawingContext context)
    {
        var started = TraceTimeline ? System.Diagnostics.Stopwatch.GetTimestamp() : 0;
        var lanes = Lanes ?? [];
        var scale = TimelineScale.ForZoom(Zoom);
        var width = Bounds.Width;
        // The visible window in content space (whole surface before the first layout).
        var viewport = Viewport.Width > 0 ? Viewport : Bounds.Size;
        var left = ScrollOffset.X;
        var right = left + viewport.Width;
        var firstLane = Math.Max(0, (int)Math.Floor(ScrollOffset.Y / TrackHeight));
        var lastLane = Math.Min(lanes.Count - 1, (int)Math.Floor((ScrollOffset.Y + viewport.Height) / TrackHeight));
        var drawn = 0;
        for (var index = firstLane; index <= lastLane; index++)
        {
            var lane = lanes[index];
            var band = new Rect(2, index * TrackHeight + TimelineGeometry.LaneInset, Math.Max(0, width - 4),
                TrackHeight - 2 * TimelineGeometry.LaneInset);
            context.DrawRectangle(lane.Selected ? SelectedBrush : BandBrush, lane.Selected ? SelectedPen : null, band, 4, 4);
            if (lane.LaneLabel.Length > 0)
            {
                var label = new FormattedText(lane.LaneLabel,
                    CultureInfo.CurrentCulture, FlowDirection.LeftToRight, Ui, 11, LabelBrush);
                context.DrawText(label, new Point(Math.Max(scale.TimeToX(0), left) + 14, band.Y + (band.Height - label.Height) / 2));
            }
            foreach (var clip in _geometry.Visible(index, scale, left, right))
            {
                DrawClip(context, scale, clip, left, right);
                drawn++;
            }
        }
        LastDrawnClips = drawn;
        var x = Math.Round(scale.TimeToX(PlayheadMs)) + 0.5;
        context.DrawLine(PlayheadPen, new Point(x, 0), new Point(x, Math.Max(TrackHeight, lanes.Count * TrackHeight)));
        if (TraceTimeline)
        {
            LastDrawMicroseconds = System.Diagnostics.Stopwatch.GetElapsedTime(started).TotalMicroseconds;
            DiagnosticRenderCount++;
            _renderMicroseconds.Add(LastDrawMicroseconds);
            if (_renderMicroseconds.Count == 30)
            {
                Console.WriteLine(FormattableString.Invariant($"timeline-render clips={_geometry.Count} drawn={drawn} avg_us={_renderMicroseconds.Average():F1} max_us={_renderMicroseconds.Max():F1} width_px={Bounds.Width:F0}"));
                _renderMicroseconds.Clear();
            }
        }
    }

    private static void DrawClip(DrawingContext context, TimelineScale scale, ClipView clip, double left, double right)
    {
        var rect = TimelineGeometry.ClipRect(scale, clip, TrackHeight).Deflate(new Thickness(0.5, 0.5));
        var ready = clip.SourceReady;
        var pen = clip.Selected ? ClipSelectedPen : ready ? ClipPen : MissingPen;
        context.DrawRectangle(ready ? ClipBrush : MissingBrush, pen, rect, 3, 3);
        if (rect.Width < 14)
        {
            return;
        }
        using (context.PushClip(rect.Deflate(new Thickness(1))))
        {
            if (ready)
            {
                context.FillRectangle(ClipHeaderBrush, new Rect(rect.X + 1, rect.Y + 1, rect.Width - 2, 16));
            }
            // Keep the name readable when the clip start is scrolled out of view.
            var textX = Math.Max(rect.X, left) + 6;
            var name = ready ? clip.Name : $"Missing · {clip.Name}";
            var title = new FormattedText(name, CultureInfo.CurrentCulture, FlowDirection.LeftToRight, UiBold, 11,
                ready ? ClipTextBrush : MissingTextBrush);
            context.DrawText(title, new Point(textX, rect.Y + 2));
            var range = new FormattedText($"{clip.StartText} – {clip.EndText} · {clip.DurationText}",
                CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Mono, 10, ready ? ClipSubtleBrush : MissingTextBrush);
            if (range.Width + 12 <= Math.Min(rect.Right, right) - textX + 6)
            {
                context.DrawText(range, new Point(textX, rect.Y + 20));
            }
        }
    }
}

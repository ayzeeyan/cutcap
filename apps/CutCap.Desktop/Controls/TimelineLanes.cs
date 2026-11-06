using System.Globalization;
using Avalonia;
using Avalonia.Automation.Peers;
using Avalonia.Controls;
using Avalonia.Input;
using Avalonia.Media;
using Avalonia.Threading;
using CutCap.Interop;

namespace CutCap.Desktop.Controls;

/// <summary>What the lanes need to know about one track; immutable per engine revision.</summary>
public readonly record struct LaneInfo(bool IsVideo, bool Selected, string LaneLabel, string Id = "");

/// <summary>
/// Custom-drawn track lanes, clips, the timeline playhead and editing
/// gestures. One visual for the whole surface: only the lanes and clips that
/// intersect the scroll viewport are drawn, so cost follows the screen rather
/// than the clip count. Clips are engine data only: no thumbnails or
/// waveforms are drawn because none exist.
/// <para>Editing (Phase 3E): a press on a clip selects it; dragging its body
/// moves it (also to another video track), dragging an edge handle trims it,
/// and library media can be dropped onto a video track. While a gesture runs
/// only a translucent ghost moves; the project is unchanged until release,
/// when exactly one edit is sent to the engine (one undo step). Escape or
/// losing the pointer cancels. Shift disables snapping.</para>
/// </summary>
public sealed class TimelineLanes : Control
{
    public static readonly StyledProperty<long> PlayheadMsProperty = AvaloniaProperty.Register<TimelineLanes, long>(nameof(PlayheadMs));
    public long PlayheadMs { get => GetValue(PlayheadMsProperty); set => SetValue(PlayheadMsProperty, value); }
    public static readonly StyledProperty<long> PlayheadTicksProperty = AvaloniaProperty.Register<TimelineLanes, long>(nameof(PlayheadTicks));
    /// <summary>Exact playhead (project ticks), used as a snap target.</summary>
    public long PlayheadTicks { get => GetValue(PlayheadTicksProperty); set => SetValue(PlayheadTicksProperty, value); }
    public const double TrackHeight = 50;
    /// <summary>Space after the last clip so the end of the timeline is reachable.</summary>
    public const double TrailingSpace = 240;
    /// <summary>Distance from a viewport edge (px) at which a drag scrolls the timeline.</summary>
    public const double AutoScrollMargin = 36;

    /// <summary>In-process drag format for library media (never a path or pixels).</summary>
    public static readonly DataFormat<MediaDrag> MediaDragFormat = DataFormat.CreateInProcessFormat<MediaDrag>("cutcap.media");

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
    private static readonly IBrush HandleBrush = new SolidColorBrush(Color.Parse("#22D9E6"));
    // Unavailable sources: a warm, desaturated clip with a dashed coral outline.
    private static readonly IBrush MissingBrush = new SolidColorBrush(Color.Parse("#3A2623"));
    private static readonly IPen MissingPen = new Pen(new SolidColorBrush(Color.Parse("#FF8F84")), 1, new DashStyle([3, 2], 0));
    private static readonly IBrush MissingTextBrush = new SolidColorBrush(Color.Parse("#FFC9C2"));
    // Gesture ghost: restrained translucent cyan when valid, coral when it would be rejected.
    private static readonly IBrush GhostBrush = new SolidColorBrush(Color.Parse("#3322D9E6"));
    private static readonly IPen GhostPen = new Pen(new SolidColorBrush(Color.Parse("#22D9E6")), 1.5, new DashStyle([4, 2], 0));
    private static readonly IBrush InvalidBrush = new SolidColorBrush(Color.Parse("#33FF8F84"));
    private static readonly IPen InvalidPen = new Pen(new SolidColorBrush(Color.Parse("#FF8F84")), 1.5, new DashStyle([4, 2], 0));
    private static readonly IPen SnapPen = new Pen(new SolidColorBrush(Color.Parse("#B322D9E6")), 1);
    private static readonly Typeface Ui = new("fonts:Inter#Inter");
    private static readonly Typeface UiBold = new("fonts:Inter#Inter", FontStyle.Normal, FontWeight.SemiBold);
    private static readonly Typeface Mono = new("Cascadia Mono, Consolas");
    // Cursors need the windowing platform, so they are created on first use (not in tests).
    private static Cursor? s_trimCursor;
    private static Cursor? s_moveCursor;
    private static Cursor TrimCursor => s_trimCursor ??= new Cursor(StandardCursorType.SizeWestEast);
    private static Cursor MoveCursor => s_moveCursor ??= new Cursor(StandardCursorType.Hand);

    private TimelineGeometry _geometry = TimelineGeometry.Empty;
    private TimelineSnapper _snapper = TimelineSnapper.Empty;

    // Gesture state (frontend preview only).
    private ClipView? _pressedClip;
    private ClipZone _pressedZone;
    private Point _pressedAt;
    private double _grabMs;
    private bool _active;
    private bool _noSnap;
    private Point _lastViewportPoint;
    private GesturePreview? _preview;
    private readonly DispatcherTimer _autoScroll;

    /// <summary>Raised with the track index when empty lane space is clicked.</summary>
    public event EventHandler<int>? LanePressed;

    /// <summary>Raised with the clip under the pointer.</summary>
    public event EventHandler<ClipView>? ClipPressed;

    /// <summary>Raised once per completed gesture with the edit to send to the engine.</summary>
    public event EventHandler<GesturePreview>? EditCommitted;

    /// <summary>The live gesture hint (null when no gesture is running).</summary>
    public event EventHandler<string?>? GestureHint;

    /// <summary>Asks the host scroll viewer to scroll horizontally by a pixel delta.</summary>
    public event EventHandler<double>? AutoScrollRequested;

    static TimelineLanes()
    {
        AffectsRender<TimelineLanes>(LanesProperty, ClipsProperty, ZoomProperty, ScrollOffsetProperty, ViewportProperty, PlayheadMsProperty);
        AffectsMeasure<TimelineLanes>(LanesProperty, ClipsProperty, ZoomProperty, ViewportProperty);
        FocusableProperty.OverrideDefaultValue<TimelineLanes>(true);
    }

    public TimelineLanes()
    {
        DragDrop.SetAllowDrop(this, true);
        AddHandler(DragDrop.DragOverEvent, OnDragOver);
        AddHandler(DragDrop.DragLeaveEvent, (_, _) => EndGesture());
        AddHandler(DragDrop.DropEvent, OnDrop);
        _autoScroll = new DispatcherTimer(TimeSpan.FromMilliseconds(16), DispatcherPriority.Input, (_, _) => AutoScrollTick());
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

    /// <summary>The running gesture's preview, if any (tests and diagnostics).</summary>
    public GesturePreview? Preview => _preview;

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
            _snapper = new TimelineSnapper(Clips ?? []);
            // A new engine clip list (an edit landed elsewhere) invalidates a running gesture.
            if (_active && _pressedClip is { } clip && Clips?.Any(c => c.Id == clip.Id) != true) EndGesture();
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

    // ---- Gestures -------------------------------------------------------------

    private SnapTarget Playhead => new(PlayheadMs, PlayheadTicks, null);
    private double ThresholdMs(TimelineScale scale) => TimelineSnapper.ThresholdPixels / scale.PixelsPerSecond * 1000;

    protected override void OnPointerPressed(PointerPressedEventArgs e)
    {
        base.OnPointerPressed(e);
        if (!e.GetCurrentPoint(this).Properties.IsLeftButtonPressed) return;
        var point = e.GetPosition(this);
        var scale = TimelineScale.ForZoom(Zoom);
        Focus();
        if (_geometry.HitTest(scale, point, TrackHeight) is { } clip)
        {
            ClipPressed?.Invoke(this, clip);
            _pressedClip = clip;
            _pressedZone = TimelineGestures.Zone(TimelineGeometry.ClipRect(scale, clip, TrackHeight), point.X);
            _pressedAt = point;
            _grabMs = scale.XToTime(point.X) - clip.StartMs;
            _lastViewportPoint = point - ScrollOffset;
            _noSnap = e.KeyModifiers.HasFlag(KeyModifiers.Shift);
            e.Pointer.Capture(this);
            if (_pressedZone != ClipZone.Body) StartGesture(point);
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

    protected override void OnPointerMoved(PointerEventArgs e)
    {
        base.OnPointerMoved(e);
        var point = e.GetPosition(this);
        if (_pressedClip is null)
        {
            var scale = TimelineScale.ForZoom(Zoom);
            Cursor = _geometry.HitTest(scale, point, TrackHeight) is { } hover
                ? TimelineGestures.Zone(TimelineGeometry.ClipRect(scale, hover, TrackHeight), point.X) == ClipZone.Body ? MoveCursor : TrimCursor
                : null;
            return;
        }
        _lastViewportPoint = point - ScrollOffset;
        _noSnap = e.KeyModifiers.HasFlag(KeyModifiers.Shift);
        if (!_active)
        {
            var travel = point - _pressedAt;
            if (Math.Abs(travel.X) < TimelineGestures.DragThreshold && Math.Abs(travel.Y) < TimelineGestures.DragThreshold) return;
            StartGesture(point);
        }
        UpdatePreview(point);
    }

    protected override void OnPointerReleased(PointerReleasedEventArgs e)
    {
        base.OnPointerReleased(e);
        if (_pressedClip is null) return;
        if (_active && _preview is { Valid: true, Changed: true } preview)
        {
            EditCommitted?.Invoke(this, preview);
        }
        e.Pointer.Capture(null);
        EndGesture();
        e.Handled = true;
    }

    protected override void OnPointerCaptureLost(PointerCaptureLostEventArgs e)
    {
        base.OnPointerCaptureLost(e);
        EndGesture();
    }

    protected override void OnKeyDown(KeyEventArgs e)
    {
        if (e.Key == Key.Escape && (_active || _pressedClip is not null))
        {
            EndGesture();
            e.Handled = true;
            return;
        }
        base.OnKeyDown(e);
    }

    /// <summary>Cancels a running gesture; the project is untouched.</summary>
    public void CancelGesture() => EndGesture();

    private void StartGesture(Point point)
    {
        _active = true;
        Cursor = _pressedZone == ClipZone.Body ? MoveCursor : TrimCursor;
        _autoScroll.Start();
        UpdatePreview(point);
    }

    private void EndGesture()
    {
        _autoScroll.Stop();
        var had = _active || _preview is not null;
        _pressedClip = null;
        _active = false;
        _preview = null;
        if (had)
        {
            GestureHint?.Invoke(this, null);
            InvalidateVisual();
        }
    }

    private void UpdatePreview(Point point)
    {
        if (_pressedClip is not { } clip || Lanes is not { } lanes) return;
        var scale = TimelineScale.ForZoom(Zoom);
        var pointerMs = scale.XToTime(point.X);
        _preview = _pressedZone switch
        {
            ClipZone.StartHandle => TimelineGestures.Trim(_geometry, clip, ClipEdge.Start, pointerMs, ThresholdMs(scale), _snapper, Playhead, !_noSnap),
            ClipZone.EndHandle => TimelineGestures.Trim(_geometry, clip, ClipEdge.End, pointerMs, ThresholdMs(scale), _snapper, Playhead, !_noSnap),
            _ => TimelineGestures.Move(_geometry, lanes, clip, pointerMs, _grabMs, (int)Math.Floor(point.Y / TrackHeight),
                ThresholdMs(scale), _snapper, Playhead, !_noSnap),
        };
        GestureHint?.Invoke(this, _preview.Reason);
        InvalidateVisual();
    }

    private void AutoScrollTick()
    {
        if (!_active || Viewport.Width <= 0) return;
        var x = _lastViewportPoint.X;
        var dx = x < AutoScrollMargin ? -(AutoScrollMargin - x) * 0.5
            : x > Viewport.Width - AutoScrollMargin ? (x - (Viewport.Width - AutoScrollMargin)) * 0.5
            : 0;
        if (dx == 0 || (dx < 0 && ScrollOffset.X <= 0)) return;
        AutoScrollRequested?.Invoke(this, dx);
        // The host has moved ScrollOffset; the pointer now covers a different time.
        if (_preview?.Kind == GestureKind.Place) return;
        UpdatePreview(_lastViewportPoint + ScrollOffset);
    }

    // ---- Library drops ---------------------------------------------------------

    private void OnDragOver(object? sender, DragEventArgs e)
    {
        if (e.DataTransfer.TryGetValue(MediaDragFormat) is not { } media || Lanes is not { } lanes)
        {
            e.DragEffects = DragDropEffects.None;
            return;
        }
        var point = e.GetPosition(this);
        var scale = TimelineScale.ForZoom(Zoom);
        _noSnap = e.KeyModifiers.HasFlag(KeyModifiers.Shift);
        _preview = TimelineGestures.Place(_geometry, lanes, media, scale.XToTime(point.X), (int)Math.Floor(point.Y / TrackHeight),
            ThresholdMs(scale), _snapper, Playhead, !_noSnap);
        e.DragEffects = _preview.Valid ? DragDropEffects.Copy : DragDropEffects.None;
        GestureHint?.Invoke(this, _preview.Reason);
        InvalidateVisual();
        e.Handled = true;
    }

    private void OnDrop(object? sender, DragEventArgs e)
    {
        OnDragOver(sender, e);
        if (_preview is { Kind: GestureKind.Place, Valid: true } preview)
        {
            EditCommitted?.Invoke(this, preview);
        }
        EndGesture();
        e.Handled = true;
    }

    // ---- Drawing ---------------------------------------------------------------

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
        var editing = _active && _preview is not null ? _preview.ClipId : null;
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
                if (clip.Id == editing)
                {
                    // The original stays in place, dimmed, until the engine accepts the edit.
                    using (context.PushOpacity(0.35)) DrawClip(context, scale, clip, left, right);
                }
                else
                {
                    DrawClip(context, scale, clip, left, right);
                }
                drawn++;
            }
        }
        LastDrawnClips = drawn;
        var height = Math.Max(TrackHeight, lanes.Count * TrackHeight);
        if (_preview is { } preview && (_active || preview.Kind == GestureKind.Place))
        {
            DrawGhost(context, scale, preview, left, right);
            if (preview.Snap is { } snap)
            {
                var sx = Math.Round(scale.TimeToX(snap.Ms)) + 0.5;
                context.DrawLine(SnapPen, new Point(sx, 0), new Point(sx, height));
            }
        }
        var x = Math.Round(scale.TimeToX(PlayheadMs)) + 0.5;
        context.DrawLine(PlayheadPen, new Point(x, 0), new Point(x, height));
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

    private static void DrawGhost(DrawingContext context, TimelineScale scale, GesturePreview preview, double left, double right)
    {
        var x1 = scale.TimeToX(preview.StartMs);
        var x2 = Math.Max(x1 + TimelineGeometry.MinClipWidth, scale.TimeToX(preview.EndMs));
        var rect = new Rect(x1, Math.Max(0, preview.TrackIndex) * TrackHeight + TimelineGeometry.LaneInset,
            x2 - x1, TrackHeight - 2 * TimelineGeometry.LaneInset).Deflate(new Thickness(0.75));
        context.DrawRectangle(preview.Valid ? GhostBrush : InvalidBrush, preview.Valid ? GhostPen : InvalidPen, rect, 3, 3);
        var text = new FormattedText(preview.Reason, CultureInfo.CurrentCulture, FlowDirection.LeftToRight, Ui, 10,
            preview.Valid ? ClipTextBrush : MissingTextBrush);
        var tx = Math.Max(rect.X, left) + 6;
        if (tx + 40 < Math.Min(rect.Right, right))
        {
            using (context.PushClip(rect)) context.DrawText(text, new Point(tx, rect.Bottom - text.Height - 3));
        }
    }

    private static void DrawClip(DrawingContext context, TimelineScale scale, ClipView clip, double left, double right)
    {
        var rect = TimelineGeometry.ClipRect(scale, clip, TrackHeight).Deflate(new Thickness(0.5, 0.5));
        var ready = clip.SourceReady;
        var pen = clip.Selected ? ClipSelectedPen : ready ? ClipPen : MissingPen;
        context.DrawRectangle(ready ? ClipBrush : MissingBrush, pen, rect, 3, 3);
        if (clip.Selected && rect.Width >= 10)
        {
            // Trim grips: the zones a press on the edge will trim.
            var grip = Math.Min(3, TimelineGestures.HandleWidth(rect.Width) / 2);
            context.FillRectangle(HandleBrush, new Rect(rect.X + 1, rect.Y + 12, grip, rect.Height - 24), 1);
            context.FillRectangle(HandleBrush, new Rect(rect.Right - 1 - grip, rect.Y + 12, grip, rect.Height - 24), 1);
        }
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

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
public readonly record struct LaneInfo(bool IsVideo, bool Selected, string LaneLabel, string Id = "", bool Audible = true);

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
    /// <summary>Lane height (DIPs). Phase 4A: 56 (was 50) so filmstrip frames are recognisable.</summary>
    public const double TrackHeight = 56;
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

    // Filmstrips: a dark teal base under the frames, lighter teal while a frame
    // is pending, hairline dividers, and a translucent band behind the name.
    private static readonly IBrush PlaceholderBrush = new SolidColorBrush(Color.Parse("#1B5960"));
    private static readonly IPen DividerPen = new Pen(new SolidColorBrush(Color.Parse("#66101417")), 1);
    private static readonly IBrush NameBandBrush = new SolidColorBrush(Color.Parse("#B30E1214"));

    private TimelineGeometry _geometry = TimelineGeometry.Empty;
    private readonly List<FilmCell> _cells = new(64);
    private readonly List<FilmCell> _planCells = new(64);
    private readonly DispatcherTimer _plan;
    private int _drawnThumbnails;
    private int _drawnPlaceholders;

    /// <summary>The shared thumbnail cache (set by the host view); null disables filmstrips.</summary>
    public Services.ThumbnailCache? Thumbnails { get; set; }

    /// <summary>Raised with the current filmstrip demand after the viewport settles.</summary>
    public event EventHandler<IReadOnlyList<ThumbnailWant>>? ThumbnailDemand;

    /// <summary>The shared waveform cache (set by the host view); null disables waveforms.</summary>
    public Services.WaveformCache? Waveforms { get; set; }

    /// <summary>Raised with the current waveform demand after the viewport settles.</summary>
    public event EventHandler<IReadOnlyList<WaveformWant>>? WaveformDemand;

    /// <summary>Pixel columns of audio clips drawn with peaks / still waiting, in the last render.</summary>
    public int LastDrawnWaveColumns { get; private set; }
    public int LastPendingWaveColumns { get; private set; }
    private int _waveColumns;
    private int _pendingColumns;
    private string? _selectedLink;
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
        // One-shot debounce: plan thumbnail demand 50 ms after the viewport settles.
        _plan = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(50) };
        _plan.Tick += (_, _) =>
        {
            _plan.Stop();
            ThumbnailDemand?.Invoke(this, PlanThumbnails());
            WaveformDemand?.Invoke(this, PlanWaveforms());
        };
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
        if (change.Property == ClipsProperty || change.Property == LanesProperty || change.Property == ZoomProperty
            || change.Property == ScrollOffsetProperty || change.Property == ViewportProperty)
        {
            SchedulePlan();
        }
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
        // The engine moves or trims a linked partner with the same timing on its
        // own track: preview its overlap too, so release never surprises.
        if (_preview is { Valid: true, Changed: true } p && Partner(clip.Id) is { } partner
            && _geometry.Overlaps(partner.TrackIndex, p.StartMs, p.EndMs, partner.Id))
        {
            _preview = p with
            {
                Valid = false,
                Reason = partner.IsAudio ? "The linked audio would overlap another clip" : "The linked video would overlap another clip",
            };
        }
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
        var traceStarted = Services.ThumbnailTrace.Enabled ? System.Diagnostics.Stopwatch.GetTimestamp() : 0;
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
        _drawnThumbnails = 0;
        _drawnPlaceholders = 0;
        _waveColumns = 0;
        _pendingColumns = 0;
        _selectedLink = null;
        foreach (var clip in Clips ?? [])
        {
            if (clip.Selected) { _selectedLink = clip.LinkId; break; }
        }
        _renderHeightPx = ThumbnailHeightPx; // one visual-tree lookup per render, not per clip
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
                else if (!lane.Audible)
                {
                    // A muted or un-soloed track keeps its clips and waveforms, dimmed.
                    using (context.PushOpacity(0.5)) DrawClip(context, scale, clip, left, right);
                }
                else
                {
                    DrawClip(context, scale, clip, left, right);
                }
                drawn++;
            }
        }
        LastDrawnClips = drawn;
        LastDrawnThumbnails = _drawnThumbnails;
        LastDrawnPlaceholders = _drawnPlaceholders;
        LastDrawnWaveColumns = _waveColumns;
        LastPendingWaveColumns = _pendingColumns;
        if (Services.ThumbnailTrace.Enabled)
        {
            Services.ThumbnailTrace.Line($"render thumbs={_drawnThumbnails} placeholders={_drawnPlaceholders} clips={drawn} wave_cols={_waveColumns} wave_pending={_pendingColumns} zoom={Zoom} scroll={ScrollOffset.X:F0} draw_us={System.Diagnostics.Stopwatch.GetElapsedTime(traceStarted).TotalMicroseconds:F0}");
        }
        var height = Math.Max(TrackHeight, lanes.Count * TrackHeight);
        if (_preview is { } preview && (_active || preview.Kind == GestureKind.Place))
        {
            DrawGhost(context, scale, preview, left, right);
            // A linked partner previews the same timing on its own track.
            if (preview.ClipId is { } moving && Partner(moving) is { } partner)
            {
                DrawGhost(context, scale, preview with { TrackIndex = partner.TrackIndex, Reason = "" }, left, right);
            }
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

    /// <summary>The other member of a linked pair (null when the clip is not linked).</summary>
    private ClipView? Partner(string clipId)
    {
        var clips = Clips;
        if (clips is null) return null;
        string? link = null;
        foreach (var c in clips)
        {
            if (c.Id == clipId) { link = c.LinkId; break; }
        }
        if (link is null) return null;
        foreach (var c in clips)
        {
            if (c.LinkId == link && c.Id != clipId) return c;
        }
        return null;
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

    // ---- Waveforms (Phase 4B) ---------------------------------------------------

    private static readonly IBrush AudioBrush = new SolidColorBrush(Color.Parse("#26487F"));
    private static readonly IPen AudioPen = new Pen(new SolidColorBrush(Color.Parse("#6E9BE0")), 1);
    private static readonly IPen WavePen = new Pen(new SolidColorBrush(Color.Parse("#A9CBFF")), 1);
    private static readonly IPen WavePendingPen = new Pen(new SolidColorBrush(Color.Parse("#4D7FB8F0")), 1);
    private static readonly IPen LinkedPen = new Pen(new SolidColorBrush(Color.Parse("#B322D9E6")), 1.5);

    /// <summary>Most waveform chunks requested per plan (visible ones first).</summary>
    public const int MaxWaveDemand = 64;

    /// <summary>
    /// Viewport-driven waveform demand: the chunks under visible audio clips,
    /// then a small overscan, skipping chunks already cached. Display-only
    /// doubles map pixels to the clip's exact sample interval from Rust.
    /// </summary>
    public IReadOnlyList<WaveformWant> PlanWaveforms()
    {
        var cache = Waveforms;
        if (cache is null || Lanes is not { } lanes) return [];
        var scale = TimelineScale.ForZoom(Zoom);
        var viewport = Viewport.Width > 0 ? Viewport : Bounds.Size;
        var left = ScrollOffset.X;
        var right = left + viewport.Width;
        var margin = viewport.Width * Overscan;
        var firstLane = Math.Max(0, (int)Math.Floor(ScrollOffset.Y / TrackHeight));
        var lastLane = Math.Min(lanes.Count - 1, (int)Math.Floor((ScrollOffset.Y + viewport.Height) / TrackHeight));
        var visible = new List<WaveformWant>();
        var overscan = new List<WaveformWant>();
        var seen = new HashSet<Services.WaveKey>();
        for (var lane = firstLane; lane <= lastLane; lane++)
        {
            if (lanes[lane].IsVideo) continue;
            foreach (var clip in _geometry.Visible(lane, scale, left - margin, right + margin))
            {
                if (!clip.IsAudio || !clip.SourceReady || clip.AudioKey.Length == 0 || clip.AudioRate == 0 || clip.AudioStream is not { } stream) continue;
                var rect = TimelineGeometry.ClipRect(scale, clip, TrackHeight);
                var perPixel = clip.AudioRate / scale.PixelsPerSecond;
                long Sample(double x) => clip.SourceInSamples + (long)Math.Floor((x - rect.X) * perPixel);
                void Add(double x0, double x1, List<WaveformWant> into)
                {
                    var from = Math.Max(x0, rect.X);
                    var to = Math.Min(x1, rect.Right);
                    if (to <= from) return;
                    var s0 = Math.Max(clip.SourceInSamples, Sample(from));
                    var s1 = Math.Min(clip.SourceEndSamples, Sample(to) + 1);
                    if (s1 <= s0) return;
                    for (var chunk = FloorDiv(s0, ChunkFrames); chunk <= FloorDiv(s1 - 1, ChunkFrames); chunk++)
                    {
                        if (chunk < 0) continue;
                        var key = new Services.WaveKey(clip.AudioKey, chunk);
                        if (cache.Contains(key) || !seen.Add(key)) continue;
                        into.Add(new WaveformWant(cache.TokenFor(key), clip.MediaId, clip.AudioKey, stream, chunk));
                    }
                }
                Add(left, right, visible);
                Add(left - margin, left, overscan);
                Add(right, right + margin, overscan);
            }
        }
        var wants = visible.Concat(overscan).Take(MaxWaveDemand).ToArray();
        cache.RetainPending(wants.Select(w => w.Token).ToHashSet());
        return wants;
    }

    /// <summary>Native-rate frames per analysis chunk (Rust: PEAK_CHUNK_FRAMES).</summary>
    public const long ChunkFrames = 256 * 1024;

    private static long FloorDiv(long a, long b) => a / b - (a % b != 0 && (a < 0) != (b < 0) ? 1 : 0);

    /// <summary>
    /// Draws measured peaks: each visible pixel column takes the minimum and
    /// maximum of the analysis buckets it covers (cached chunks only; nothing
    /// is decoded or allocated here). Columns still waiting show a faint line.
    /// </summary>
    private void DrawWaveform(DrawingContext context, TimelineScale scale, ClipView clip, Rect raw, Rect inner, double left, double right)
    {
        var cache = Waveforms!;
        var top = inner.Y + 16;
        var height = inner.Bottom - top - 1;
        if (height < 4) return;
        var mid = top + height / 2;
        var half = height / 2;
        var perPixel = clip.AudioRate / scale.PixelsPerSecond;
        var from = Math.Floor(Math.Max(inner.X, left));
        var to = Math.Min(inner.Right, right);
        Services.WaveChunk? chunk = null;
        long chunkIndex = long.MinValue;
        using (context.PushClip(inner))
        {
            for (var x = from; x < to; x++)
            {
                var s0 = clip.SourceInSamples + (long)Math.Floor((x - raw.X) * perPixel);
                var s1 = clip.SourceInSamples + (long)Math.Floor((x + 1 - raw.X) * perPixel);
                s0 = Math.Max(s0, clip.SourceInSamples);
                s1 = Math.Min(Math.Max(s1, s0 + 1), clip.SourceEndSamples);
                if (s1 <= s0) continue;
                var lo = float.PositiveInfinity;
                var hi = float.NegativeInfinity;
                var pending = false;
                var bucketFrames = chunk?.BucketFrames is > 0 and var f ? f : 256;
                for (var b = FloorDiv(s0, bucketFrames); b <= FloorDiv(s1 - 1, bucketFrames); b++)
                {
                    var index = FloorDiv(b * bucketFrames, ChunkFrames);
                    if (index != chunkIndex)
                    {
                        chunkIndex = index;
                        chunk = cache.TryGet(new Services.WaveKey(clip.AudioKey, index), out var found) ? found : null;
                    }
                    if (chunk?.Peaks is not { } peaks)
                    {
                        pending |= chunk is null;
                        continue;
                    }
                    var bucket = (int)(b - index * chunk.Buckets);
                    if (bucket < 0 || bucket >= chunk.Buckets) continue;
                    var (min, max) = (peaks[2 * bucket], peaks[2 * bucket + 1]);
                    if (min > max) continue; // no decoded samples in this bucket
                    lo = Math.Min(lo, min);
                    hi = Math.Max(hi, max);
                }
                var cx = x + 0.5;
                if (hi >= lo)
                {
                    var y1 = mid - Math.Clamp(hi, -1, 1) * half;
                    var y2 = mid - Math.Clamp(lo, -1, 1) * half;
                    if (y2 - y1 < 1) (y1, y2) = (mid - 0.5, mid + 0.5);
                    context.DrawLine(WavePen, new Point(cx, y1), new Point(cx, y2));
                    _waveColumns++;
                }
                else if (pending)
                {
                    context.DrawLine(WavePendingPen, new Point(cx, mid - 0.5), new Point(cx, mid + 0.5));
                    _pendingColumns++;
                }
            }
        }
    }

    // ---- Filmstrips (Phase 4A) --------------------------------------------------

    /// <summary>Height (DIPs) of the image strip inside a clip's border.</summary>
    public static double StripHeight => TrackHeight - 2 * TimelineGeometry.LaneInset - 3;

    /// <summary>The overscan (fraction of the viewport width each side) the planner prefetches.</summary>
    public const double Overscan = 0.25;

    /// <summary>Most samples requested per plan (visible ones first).</summary>
    public const int MaxDemand = 96;

    private double RenderScaling => TopLevel.GetTopLevel(this)?.RenderScaling ?? 1;
    private int ThumbnailHeightPx => (int)Math.Round(StripHeight * RenderScaling);

    private void SchedulePlan()
    {
        if (Thumbnails is null) return;
        _plan.Stop();
        _plan.Start();
    }

    /// <summary>New thumbnails were cached: redraw, then ask for what is still missing.</summary>
    public void ThumbnailsArrived()
    {
        InvalidateVisual();
        SchedulePlan();
    }

    /// <summary>
    /// Viewport-driven demand: the cells of visible video clips, then a small
    /// overscan, skipping samples already cached (or known to have no frame).
    /// Runs on a short debounce after scroll/zoom/clip changes, never in Render.
    /// </summary>
    public IReadOnlyList<ThumbnailWant> PlanThumbnails()
    {
        var cache = Thumbnails;
        if (cache is null || Lanes is not { } lanes) return [];
        var planStarted = Services.ThumbnailTrace.Enabled ? System.Diagnostics.Stopwatch.GetTimestamp() : 0;
        var scale = TimelineScale.ForZoom(Zoom);
        var viewport = Viewport.Width > 0 ? Viewport : Bounds.Size;
        var left = ScrollOffset.X;
        var right = left + viewport.Width;
        var margin = viewport.Width * Overscan;
        var firstLane = Math.Max(0, (int)Math.Floor(ScrollOffset.Y / TrackHeight));
        var lastLane = Math.Min(lanes.Count - 1, (int)Math.Floor((ScrollOffset.Y + viewport.Height) / TrackHeight));
        var height = ThumbnailHeightPx;
        var visible = new List<ThumbnailWant>();
        var overscan = new List<ThumbnailWant>();
        var seen = new HashSet<ThumbKey>();
        var examined = 0;
        for (var lane = firstLane; lane <= lastLane; lane++)
        {
            if (!lanes[lane].IsVideo) continue;
            foreach (var clip in _geometry.Visible(lane, scale, left - margin, right + margin))
            {
                examined++;
                if (!clip.SourceReady || clip.SourceKey.Length == 0) continue;
                var rect = TimelineGeometry.ClipRect(scale, clip, TrackHeight);
                var cellWidth = FilmstripLayout.CellWidth(StripHeight, clip.ThumbAspect);
                if (rect.Width < FilmstripLayout.MinStripWidth || cellWidth <= 0) continue;
                _planCells.Clear();
                FilmstripLayout.Cells(clip, rect.X, rect.Right, scale.PixelsPerSecond,
                    FilmstripLayout.StepUs(cellWidth, scale.PixelsPerSecond), left - margin, right + margin, left, right, _planCells);
                foreach (var cell in _planCells)
                {
                    var key = new ThumbKey(clip.SourceKey, cell.SampleUs, height);
                    if (cache.Contains(key) || !seen.Add(key)) continue;
                    (cell.Visible ? visible : overscan).Add(new ThumbnailWant(cache.TokenFor(key), clip.MediaId, clip.SourceKey, cell.SampleUs, height));
                }
            }
        }
        var wants = visible.Concat(overscan).Take(MaxDemand).ToArray();
        cache.RetainPending(wants.Select(w => w.Token).ToHashSet());
        LastPlannedVisible = visible.Count;
        LastPlannedClipsExamined = examined;
        Services.ThumbnailTrace.Line($"plan visible={visible.Count} overscan={overscan.Count} sent={wants.Length} plan_us={(Services.ThumbnailTrace.Enabled ? System.Diagnostics.Stopwatch.GetElapsedTime(planStarted).TotalMicroseconds : 0):F0}");
        return wants;
    }

    /// <summary>Visible samples still missing at the last plan (tests, diagnostics).</summary>
    public int LastPlannedVisible { get; private set; }

    /// <summary>Clips the last plan looked at: the viewport's reach, whatever the project's size.</summary>
    public int LastPlannedClipsExamined { get; private set; }

    private int _renderHeightPx;

    /// <summary>Cells drawn with a real thumbnail / as a placeholder in the last render.</summary>
    public int LastDrawnThumbnails { get; private set; }
    public int LastDrawnPlaceholders { get; private set; }

    private void DrawClip(DrawingContext context, TimelineScale scale, ClipView clip, double left, double right)
    {
        var raw = TimelineGeometry.ClipRect(scale, clip, TrackHeight);
        var rect = raw.Deflate(new Thickness(0.5, 0.5));
        var ready = clip.SourceReady;
        var audio = clip.IsAudio;
        var linkedToSelection = !clip.Selected && clip.LinkId is not null && clip.LinkId == _selectedLink;
        var pen = clip.Selected ? ClipSelectedPen
            : !ready ? MissingPen
            : linkedToSelection ? LinkedPen
            : audio ? AudioPen : ClipPen;
        context.DrawRectangle(ready ? (audio ? AudioBrush : ClipBrush) : MissingBrush, null, rect, 3, 3);
        var inner = rect.Deflate(new Thickness(1));
        var filmstrip = ready && Thumbnails is not null && clip.SourceKey.Length > 0
            && rect.Width >= FilmstripLayout.MinStripWidth && clip.ThumbAspect > 0;
        if (filmstrip)
        {
            DrawFilmstrip(context, scale, clip, raw, inner, left, right);
        }
        if (audio && ready && Waveforms is not null && clip.AudioKey.Length > 0 && clip.AudioRate > 0)
        {
            DrawWaveform(context, scale, clip, raw, inner, left, right);
        }
        if (rect.Width >= 14)
        {
            using (context.PushClip(inner))
            {
                // Keep the name readable when the clip start is scrolled out of view.
                var textX = Math.Max(rect.X, left) + 6;
                var name = ready ? clip.Name : $"Missing · {clip.Name}";
                var title = new FormattedText(name, CultureInfo.CurrentCulture, FlowDirection.LeftToRight, UiBold, 11,
                    ready ? ClipTextBrush : MissingTextBrush);
                var range = new FormattedText($"{clip.StartText} – {clip.EndText} · {clip.DurationText}",
                    CultureInfo.InvariantCulture, FlowDirection.LeftToRight, Mono, 10, ready ? ClipSubtleBrush : MissingTextBrush);
                var visibleRight = Math.Min(rect.Right, right);
                var showRange = textX + title.Width + 12 + range.Width <= visibleRight - 4;
                if (ready)
                {
                    // A slim translucent name band: the frames stay visible beneath it.
                    var band = Math.Min(inner.Width, title.Width + (showRange ? range.Width + 12 : 0) + 12);
                    var banded = filmstrip || audio;
                    context.FillRectangle(banded ? NameBandBrush : ClipHeaderBrush,
                        new Rect(textX - 6, inner.Y, banded ? band : inner.Width, 16));
                }
                context.DrawText(title, new Point(textX, rect.Y + 2));
                if (showRange)
                {
                    context.DrawText(range, new Point(textX + title.Width + 12, rect.Y + 3));
                }
            }
        }
        // Outline and grips last, above the images.
        context.DrawRectangle(null, pen, rect, 3, 3);
        if (clip.Selected && rect.Width >= 10)
        {
            // Trim grips: the zones a press on the edge will trim.
            var grip = Math.Min(3, TimelineGestures.HandleWidth(rect.Width) / 2);
            context.FillRectangle(HandleBrush, new Rect(rect.X + 1, rect.Y + 14, grip, rect.Height - 26), 1);
            context.FillRectangle(HandleBrush, new Rect(rect.Right - 1 - grip, rect.Y + 14, grip, rect.Height - 26), 1);
        }
    }

    /// <summary>
    /// Real source frames across the visible part of a clip. Each cell shows its
    /// cached thumbnail only if the frame's actual PTS lies inside this clip's
    /// exact source interval; otherwise (pending, no frame, failed) a restrained
    /// placeholder. No decoding, I/O or allocation per image here.
    /// </summary>
    private void DrawFilmstrip(DrawingContext context, TimelineScale scale, ClipView clip, Rect raw, Rect inner, double left, double right)
    {
        var cache = Thumbnails!;
        var cellWidth = FilmstripLayout.CellWidth(StripHeight, clip.ThumbAspect);
        var height = _renderHeightPx;
        _cells.Clear();
        FilmstripLayout.Cells(clip, raw.X, raw.Right, scale.PixelsPerSecond, FilmstripLayout.StepUs(cellWidth, scale.PixelsPerSecond),
            left, right, left, right, _cells);
        // A rectangular clip (cheaper than a rounded geometry); the outline drawn
        // last covers the corners.
        using (context.PushClip(inner))
        {
            for (var i = 0; i < _cells.Count; i++)
            {
                var cell = _cells[i];
                var dest = new Rect(cell.X, inner.Y, Math.Min(cellWidth, cell.Width), inner.Height);
                if (cache.TryGet(new ThumbKey(clip.SourceKey, cell.SampleUs, height), out var entry)
                    && entry is { State: Services.ThumbnailState.Ready, Bitmap: { } bitmap }
                    && FilmstripLayout.Belongs(clip, entry.ActualTicks))
                {
                    var size = bitmap.PixelSize;
                    // Partial cells show the left part of the frame, never a stretched one.
                    context.DrawImage(bitmap, new Rect(0, 0, size.Width * dest.Width / cellWidth, size.Height), dest);
                    _drawnThumbnails++;
                }
                else
                {
                    context.FillRectangle(PlaceholderBrush, dest.Deflate(new Thickness(0.5, 0)));
                    _drawnPlaceholders++;
                }
                if (cell.X > raw.X + 1)
                {
                    context.DrawLine(DividerPen, new Point(Math.Round(cell.X) + 0.5, inner.Top), new Point(Math.Round(cell.X) + 0.5, inner.Bottom));
                }
            }
        }
    }
}

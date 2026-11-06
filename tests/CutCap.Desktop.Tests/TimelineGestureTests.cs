using Avalonia;
using CutCap.Desktop.Controls;
using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Phase 3E gesture previews, snapping and hit zones, without a running engine.</summary>
public sealed class TimelineGestureTests
{
    private const long Tick = 720; // project ticks per millisecond (1/720000 s)

    private static ClipView Clip(string id, int track, long startMs, long durationMs, long? minStartMs = null, long? maxEndMs = null) =>
        new(id, $"t{track}", track, "m1", $"{id}.mkv", startMs, durationMs, "", "", "", "ready", false,
            startMs * Tick, (startMs + durationMs) * Tick, (minStartMs ?? startMs) * Tick, minStartMs ?? startMs,
            maxEndMs is { } m ? m * Tick : (startMs + durationMs) * Tick, maxEndMs ?? startMs + durationMs);

    private static readonly LaneInfo[] Lanes = [new(true, false, "", "t0"), new(false, false, "", "t1"), new(true, false, "", "t2")];

    private static (TimelineGeometry, TimelineSnapper) Build(params ClipView[] clips) =>
        (new TimelineGeometry(clips, Lanes.Length), new TimelineSnapper(clips));

    private static readonly SnapTarget Playhead = new(10_000, 10_000 * Tick, null);

    [Fact]
    public void SnapperFindsTheNearestExactEdgeWithinThresholdAndIgnoresTheEditedClip()
    {
        var snapper = new TimelineSnapper([Clip("a", 0, 1000, 2000), Clip("b", 2, 5000, 1000)]);
        Assert.Equal(4, snapper.Count);
        Assert.Equal(new SnapTarget(3000, 3000 * Tick, "a"), snapper.Snap(3030, 50, null, Playhead));
        Assert.Null(snapper.Snap(3100, 50, null, Playhead));
        Assert.Equal(6000 * Tick, snapper.Snap(5990, 50, "a", Playhead)?.Ticks);
        // The edited clip's own edges are not targets; zero and the playhead always are.
        Assert.Null(snapper.Snap(1010, 50, "a", Playhead));
        Assert.Equal(0, snapper.Snap(30, 50, "a", Playhead)?.Ticks);
        Assert.Equal(Playhead, snapper.Snap(9_960, 50, null, Playhead));
        // The nearer of two candidates wins.
        Assert.Equal(5000 * Tick, snapper.Snap(4990, 50, null, new SnapTarget(4960, 4960 * Tick, null))?.Ticks);
    }

    [Fact]
    public void MoveSnapsEitherEdgeCommitsExactTicksAndRejectsOverlapAndAudio()
    {
        var a = Clip("a", 0, 0, 2000);
        var b = Clip("b", 0, 5000, 1000);
        var (geometry, snapper) = Build(a, b);
        // Grab a's middle; its end lands 20 ms before b: the end edge snaps to b's start.
        var end = TimelineGestures.Move(geometry, Lanes, a, 4000 - 20, 1000, 0, 50, snapper, Playhead, snap: true);
        Assert.True(end.Valid && end.Changed);
        Assert.Equal((ClipEdge.End, TimelinePosition.Ticks(5000 * Tick)), (end.Anchor, end.Position));
        Assert.Equal(3000, end.StartMs);
        Assert.NotNull(end.Snap);
        // Without snapping it commits whole milliseconds by its start.
        var free = TimelineGestures.Move(geometry, Lanes, a, 4000 - 20, 1000, 0, 50, snapper, Playhead, snap: false);
        Assert.Equal((ClipEdge.Start, TimelinePosition.Milliseconds(2980)), (free.Anchor, free.Position));
        Assert.Null(free.Snap);
        // Overlapping b is invalid before anything is sent.
        var overlap = TimelineGestures.Move(geometry, Lanes, a, 5500, 1000, 0, 50, snapper, Playhead, snap: false);
        Assert.False(overlap.Valid);
        Assert.Contains("overlap", overlap.Reason);
        // The audio lane and a lane past the last track are invalid; another video track is fine.
        Assert.False(TimelineGestures.Move(geometry, Lanes, a, 3000, 1000, 1, 50, snapper, Playhead, false).Valid);
        Assert.False(TimelineGestures.Move(geometry, Lanes, a, 3000, 1000, 7, 50, snapper, Playhead, false).Valid);
        var across = TimelineGestures.Move(geometry, Lanes, a, 5500, 1000, 2, 50, snapper, Playhead, false);
        Assert.True(across.Valid && across.Changed);
        Assert.Equal("t2", across.TrackId);
        // Releasing where it started is not an edit; the start is clamped at zero.
        Assert.False(TimelineGestures.Move(geometry, Lanes, a, 1000, 1000, 0, 50, snapper, Playhead, false).Changed);
        Assert.Equal(0, TimelineGestures.Move(geometry, Lanes, b, 100, 900, 0, 50, snapper, Playhead, false).StartMs);
    }

    [Fact]
    public void TrimClampsToTheSourceRangeAndNeighboursWithExactBoundaries()
    {
        var left = Clip("left", 0, 0, 1000);
        var clip = Clip("c", 0, 3000, 2000, minStartMs: 500, maxEndMs: 9000);
        var right = Clip("right", 0, 7000, 1000);
        var (geometry, snapper) = Build(left, clip, right);
        // Start: the previous clip's end (1000) is later than the source limit (500).
        var start = TimelineGestures.Trim(geometry, clip, ClipEdge.Start, 200, 50, snapper, Playhead, snap: false);
        Assert.Equal((1000.0, TimelinePosition.Ticks(1000 * Tick)), (start.StartMs, start.Position));
        Assert.Contains("previous clip", start.Reason);
        var mid = TimelineGestures.Trim(geometry, clip, ClipEdge.Start, 3500.4, 50, snapper, Playhead, snap: false);
        Assert.Equal(TimelinePosition.Milliseconds(3500), mid.Position);
        Assert.True(mid.Changed);
        var collapse = TimelineGestures.Trim(geometry, clip, ClipEdge.Start, 9000, 50, snapper, Playhead, false);
        Assert.Equal(4999, collapse.StartMs);
        // End: the next clip's start (7000) comes before the source end (9000).
        var end = TimelineGestures.Trim(geometry, clip, ClipEdge.End, 8000, 50, snapper, Playhead, false);
        Assert.Equal((7000.0, TimelinePosition.Ticks(7000 * Tick)), (end.EndMs, end.Position));
        Assert.Contains("next clip", end.Reason);
        // Snapping to the playhead commits its exact ticks.
        var snapped = TimelineGestures.Trim(geometry, Clip("p", 0, 8500, 3000), ClipEdge.Start, 9_980, 50, snapper, Playhead, true);
        Assert.Equal(TimelinePosition.Ticks(10_000 * Tick), snapped.Position);
        // Unknown source end: no extension past the current end.
        var unknown = Clip("u", 2, 0, 1000) with { MaxEndTicks = null, MaxEndMs = null };
        var grow = TimelineGestures.Trim(new TimelineGeometry([unknown], 3), unknown, ClipEdge.End, 5000, 50, TimelineSnapper.Empty, null, false);
        Assert.Equal(1000, grow.EndMs);
        Assert.False(grow.Changed);
    }

    [Fact]
    public void PlacementPreviewsTheWholeSourceAndExplainsRejections()
    {
        var (geometry, snapper) = Build(Clip("a", 0, 2000, 1000));
        var media = new MediaDrag("m", "take.mkv", 3000, true);
        var ok = TimelineGestures.Place(geometry, Lanes, media, 5000, 0, 50, snapper, Playhead, true);
        Assert.True(ok.Valid);
        Assert.Equal((5000.0, 8000.0, "t0"), (ok.StartMs, ok.EndMs, ok.TrackId));
        Assert.Equal(TimelinePosition.Milliseconds(5000), ok.Position);
        var snapped = TimelineGestures.Place(geometry, Lanes, media, 3020, 0, 50, snapper, Playhead, true);
        Assert.Equal(TimelinePosition.Ticks(3000 * Tick), snapped.Position);
        Assert.Contains("overlap", TimelineGestures.Place(geometry, Lanes, media, 0, 0, 50, snapper, Playhead, false).Reason);
        Assert.False(TimelineGestures.Place(geometry, Lanes, media, 5000, 1, 50, snapper, Playhead, false).Valid);
        Assert.Contains("unavailable", TimelineGestures.Place(geometry, Lanes, media with { Ready = false }, 5000, 0, 50, snapper, Playhead, false).Reason);
        Assert.Contains("duration", TimelineGestures.Place(geometry, Lanes, media with { DurationMs = null }, 5000, 0, 50, snapper, Playhead, false).Reason);
    }

    [Fact]
    public void HandlesStayUsableOnSmallClipsWithoutSwallowingTheBody()
    {
        Assert.Equal(8, TimelineGestures.HandleWidth(400));
        Assert.Equal(3, TimelineGestures.HandleWidth(6));
        var rect = new Rect(100, 5, 200, 40);
        Assert.Equal(ClipZone.StartHandle, TimelineGestures.Zone(rect, 103));
        Assert.Equal(ClipZone.Body, TimelineGestures.Zone(rect, 200));
        Assert.Equal(ClipZone.EndHandle, TimelineGestures.Zone(rect, 296));
        // At 50% zoom a 300 ms clip is 3 px wide: still hit-testable, mostly handles.
        var scale = TimelineScale.ForZoom(50);
        var tiny = Clip("t", 0, 1000, 300);
        var tinyRect = TimelineGeometry.ClipRect(scale, tiny, TimelineLanes.TrackHeight);
        Assert.Equal(TimelineGeometry.MinClipWidth, tinyRect.Width);
        Assert.Equal("t", new TimelineGeometry([tiny], 1).HitTest(scale, tinyRect.Center, TimelineLanes.TrackHeight)?.Id);
    }

    [Fact]
    public void OverlapAndNeighbourLookupsFollowTheSortedLane()
    {
        var clips = Enumerable.Range(0, 5000).Select(i => Clip($"c{i}", 0, i * 1000L, 1000)).Reverse().ToArray();
        var geometry = new TimelineGeometry(clips, 1);
        Assert.True(geometry.Overlaps(0, 2500, 2600, null));
        Assert.False(geometry.Overlaps(0, 2500, 2600, "c2"));
        Assert.False(geometry.Overlaps(0, 5_000_000, 5_001_000, null));
        Assert.False(geometry.Overlaps(0, 3000, 3000.5, "c3"), "touching edges are not an overlap");
        Assert.Equal("c2", geometry.Previous(0, 3000, "c3")?.Id);
        Assert.Equal("c4", geometry.Next(0, 4000, "c3")?.Id);
        Assert.Null(geometry.Next(0, 5_000_000, null));
    }

    [Fact]
    public void SplitIsEnabledOnlyWhenRustSaysThePlayheadIsInsideTheSelectedClip()
    {
        var timeline = new TimelineViewModel(new EngineHost());
        Assert.False(timeline.SplitCommand.CanExecute(null));
        Assert.Contains("Ctrl+B", timeline.SplitTip);
        var selected = Clip("a", 0, 0, 3000) with { Selected = true };
        var view = new SessionView(1, true, new ProjectView("p", "Untitled", false, 1), new StatusView("Ready", false), 0,
            [new TrackView("t0", "Video 1", "video", "V1", false)], [], new ImportView(false, false, ""),
            new InspectorView("clip", "a.mkv", "", []), new SourceView(false, "", 0, 0, false, false, false, "", "", ""), null,
            new PreviewMetrics(null, null, null, 0), null, 1, new HistoryStatus(false, false, null, null, 0),
            Editing: new EditingView(false, "Place the playhead inside the clip to split it."));
        timeline.Apply(view, new TimelineView(1, [selected]));
        Assert.False(timeline.SplitCommand.CanExecute(null));
        Assert.Contains("inside the clip", timeline.SplitTip);
        timeline.ApplyEditing(new EditingView(true, "Split the selected clip at the playhead (Ctrl+B)"));
        Assert.True(timeline.SplitCommand.CanExecute(null));
        timeline.ShowGestureHint("Move to 00:01.000");
        Assert.Equal("Move to 00:01.000", timeline.EditHint);
        timeline.ShowGestureHint(null);
        Assert.Equal(TimelineViewModel.DefaultHint, timeline.EditHint);
    }

    [Fact]
    [Trait("Category", "Measurement")]
    public void SnapAndOverlapLookupsStayLogarithmicAtTheClipLimit()
    {
        var clips = Enumerable.Range(0, 100_000).Select(i => Clip($"c{i}", i % 4 == 1 ? 0 : i % 4, i / 4 * 3000L, 3000)).ToArray();
        var watch = System.Diagnostics.Stopwatch.StartNew();
        var snapper = new TimelineSnapper(clips);
        var geometry = new TimelineGeometry(clips, 4);
        var build = watch.Elapsed;
        watch.Restart();
        var hits = 0;
        for (var i = 0; i < 10_000; i++)
        {
            var ms = i * 7_501.3 % 75_000_000;
            if (snapper.Snap(ms, 40, null, Playhead) is not null) hits++;
            if (geometry.Overlaps(2, ms, ms + 1500, null)) hits++;
        }
        var perMove = watch.Elapsed / 10_000;
        TestContext.Current.TestOutputHelper?.WriteLine(
            $"clips=100000 build={build.TotalMilliseconds:F1} ms snap+overlap per pointer move={perMove.TotalMicroseconds:F2} us hits={hits}");
        Assert.True(perMove < TimeSpan.FromMilliseconds(1), $"{perMove.TotalMicroseconds} us per move");
    }
}

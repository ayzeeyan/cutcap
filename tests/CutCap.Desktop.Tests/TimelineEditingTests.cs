using Avalonia;
using CutCap.Desktop.Controls;
using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Clip layout, hit-testing, culling and timeline view-model state, without a running engine.</summary>
public sealed class TimelineEditingTests
{
    private static ClipView Clip(string id, int track, long start, long duration, bool selected = false, string availability = "ready") =>
        new(id, $"t{track}", track, "m1", $"{id}.mkv", start, duration, "", "", "", availability, selected);

    private static readonly HistoryStatus NoHistory = new(false, false, null, null, 0);

    private static SessionView View(IReadOnlyList<TrackView>? tracks = null, ulong generation = 1, HistoryStatus? history = null,
        bool mediaSelected = true, string availability = "ready") =>
        new(1, true, new ProjectView("p", "Untitled", false, 0), new StatusView("Ready", false), 0,
            tracks ?? [new TrackView("t0", "Video 1", "video", "V1", false), new TrackView("t1", "Audio 1", "audio", "A1", false)],
            [new MediaView("m1", "a.mkv", "320 × 180", "FFV1", "3.0 s", 3000, true, false, mediaSelected, availability)],
            new ImportView(false, false, ""), new InspectorView("project", "Untitled", "", []),
            new SourceView(true, "a.mkv", 0, 3000, false, false, true, "", "", ""), null,
            new PreviewMetrics(null, null, null, 0), null, generation, history ?? NoHistory);

    [Fact]
    public void ClipRectanglesFollowZoomExactlyAndKeepAMinimumWidth()
    {
        var scale = TimelineScale.ForZoom(100); // 20 px/s, origin 12
        var rect = TimelineGeometry.ClipRect(scale, Clip("a", 1, 3000, 1500), TimelineLanes.TrackHeight);
        Assert.Equal(new Rect(72, 55, 30, 40), rect);
        var doubled = TimelineGeometry.ClipRect(TimelineScale.ForZoom(200), Clip("a", 1, 3000, 1500), TimelineLanes.TrackHeight);
        Assert.Equal((132, 60), (doubled.X, doubled.Width));
        var tiny = TimelineGeometry.ClipRect(scale, Clip("t", 0, 0, 1), TimelineLanes.TrackHeight);
        Assert.Equal(TimelineGeometry.MinClipWidth, tiny.Width);
    }

    [Fact]
    public void HitTestingFindsTheClipUnderThePointerOnItsOwnTrack()
    {
        var geometry = new TimelineGeometry([Clip("a", 0, 0, 3000), Clip("b", 0, 3000, 3000), Clip("c", 1, 1000, 1000)], 2);
        var scale = TimelineScale.ForZoom(100);
        Assert.Equal("a", geometry.HitTest(scale, new Point(scale.TimeToX(2999), 25), 50)?.Id);
        Assert.Equal("b", geometry.HitTest(scale, new Point(scale.TimeToX(3001), 25), 50)?.Id);
        Assert.Equal("c", geometry.HitTest(scale, new Point(scale.TimeToX(1500), 75), 50)?.Id);
        Assert.Null(geometry.HitTest(scale, new Point(scale.TimeToX(1500), 2), 50)); // lane inset
        Assert.Null(geometry.HitTest(scale, new Point(scale.TimeToX(2500), 75), 50)); // empty lane space
        Assert.Null(geometry.HitTest(scale, new Point(scale.TimeToX(500), 175), 50)); // no such track
        Assert.Equal(6000, geometry.EndMs);
    }

    [Fact]
    public void ViewportCullingVisitsOnlyClipsInsideTheVisibleRange()
    {
        // 10,000 one-second clips on one track, inserted out of order.
        var clips = Enumerable.Range(0, 10_000).Reverse().Select(i => Clip($"c{i}", 0, i * 1000L, 1000)).ToArray();
        var geometry = new TimelineGeometry(clips, 1);
        var scale = TimelineScale.ForZoom(100);
        var left = scale.TimeToX(5_000_000);
        var visible = geometry.Visible(0, scale, left, left + 400).Select(c => c.StartMs).ToArray();
        Assert.Equal(Enumerable.Range(4999, 22).Select(i => i * 1000L), visible);
        Assert.Empty(geometry.Visible(0, scale, scale.TimeToX(20_000_000), scale.TimeToX(20_000_100)));
        Assert.Empty(geometry.Visible(3, scale, 0, 1000));
    }

    [Fact]
    public void ContentExtendsPastTheLastClipAtEveryZoom()
    {
        foreach (var zoom in new[] { 50, 100, 200 })
        {
            var scale = TimelineScale.ForZoom(zoom);
            var width = TimelineLanes.ContentWidth(scale, 800, 10 * 60 * 1000);
            Assert.Equal(scale.TimeToX(600_000) + TimelineLanes.TrailingSpace, width);
            Assert.Equal(scale.ContentWidth(800), TimelineLanes.ContentWidth(scale, 800, 0));
        }
    }

    [Fact]
    public void InsertionTargetPrefersTheSelectedVideoTrackThenTheClipTrackThenTheFirstVideoTrack()
    {
        TrackViewModel Track(string id, string kind, bool selected = false) => new(new TrackView(id, id, kind, "", selected));
        var tracks = new[] { Track("t0", "video"), Track("t1", "audio"), Track("t2", "video") };
        Assert.Equal("t0", TimelineViewModel.ChooseTarget(tracks, null)?.Id);
        Assert.Equal("t2", TimelineViewModel.ChooseTarget(tracks, Clip("a", 2, 0, 1) with { TrackId = "t2" })?.Id);
        var selectedVideo = new[] { Track("t0", "video"), Track("t1", "audio"), Track("t2", "video", true) };
        Assert.Equal("t2", TimelineViewModel.ChooseTarget(selectedVideo, null)?.Id);
        var selectedAudio = new[] { Track("t0", "video"), Track("t1", "audio", true) };
        Assert.Equal("t0", TimelineViewModel.ChooseTarget(selectedAudio, null)?.Id);
        Assert.Null(TimelineViewModel.ChooseTarget([Track("t1", "audio")], null));
    }

    [Fact]
    public void ViewModelEnablesCommandsOnlyForWhatTheEngineReports()
    {
        var timeline = new TimelineViewModel(new EngineHost());
        timeline.Apply(View(), new TimelineView(1, []));
        Assert.True(timeline.AddClipCommand.CanExecute(null));
        Assert.Equal("Add a.mkv to the end of Video 1", timeline.AddClipTip);
        Assert.False(timeline.RemoveClipCommand.CanExecute(null));
        Assert.False(timeline.UndoCommand.CanExecute(null));
        Assert.Equal("0 clips", timeline.ClipCountText);

        var history = new HistoryStatus(true, false, "add a.mkv", null, 1);
        timeline.Apply(View(generation: 2, history: history), new TimelineView(2, [Clip("a", 0, 0, 3000, selected: true)]));
        Assert.Equal("a", timeline.SelectedClip?.Id);
        Assert.True(timeline.RemoveClipCommand.CanExecute(null));
        Assert.True(timeline.UndoCommand.CanExecute(null));
        Assert.False(timeline.RedoCommand.CanExecute(null));
        Assert.Equal("Undo add a.mkv (Ctrl+Z)", timeline.UndoTip);
        Assert.Contains("Selected clip a.mkv", timeline.LanesAccessibleName);

        timeline.Apply(View(generation: 3, mediaSelected: false), new TimelineView(3, [Clip("a", 0, 0, 3000)]));
        Assert.False(timeline.AddClipCommand.CanExecute(null));
        Assert.Null(timeline.SelectedClip);
        timeline.Apply(View(generation: 4, availability: "missing"), new TimelineView(4, [Clip("a", 0, 0, 3000, availability: "missing")]));
        Assert.False(timeline.AddClipCommand.CanExecute(null));
        Assert.Contains("unavailable", timeline.AddClipTip);
        Assert.False(timeline.Clips[0].SourceReady);
        timeline.Apply(View(tracks: [new TrackView("t1", "Audio 1", "audio", "A1", false)], generation: 5), new TimelineView(5, []));
        Assert.Equal("Add a video track first", timeline.AddClipTip);
    }

    /// <summary>Opt-in measurement (dotnet test --filter ... -- xUnit.Explicit=on): layout, culling and hit-test cost at the core clip limit.</summary>
    [Fact(Explicit = true)]
    public void MeasureLayoutCullingAndHitTestAtTheClipLimit()
    {
        var clips = Enumerable.Range(0, 100_000).Select(i => Clip($"c{i}", i % 4, i / 4 * 3000L, 3000)).ToArray();
        var watch = System.Diagnostics.Stopwatch.StartNew();
        var geometry = new TimelineGeometry(clips, 4);
        var build = watch.Elapsed;
        var scale = TimelineScale.ForZoom(100);
        var left = scale.TimeToX(geometry.EndMs / 2.0);
        watch.Restart();
        var visible = 0;
        for (var frame = 0; frame < 1000; frame++)
        {
            for (var track = 0; track < 4; track++) { visible += geometry.Visible(track, scale, left, left + 1600).Count(); }
        }
        var cull = watch.Elapsed / 1000;
        watch.Restart();
        for (var i = 0; i < 1000; i++) { geometry.HitTest(scale, new Point(left + i, 75), TimelineLanes.TrackHeight); }
        var hit = watch.Elapsed / 1000;
        TestContext.Current.TestOutputHelper?.WriteLine(
            $"clips=100000 build={build.TotalMilliseconds:F2} ms cull_per_frame={cull.TotalMicroseconds:F1} us visible_per_frame={visible / 1000} hit_test={hit.TotalMicroseconds:F2} us");
        Assert.InRange(visible / 1000, 4, 4 * 30);
    }

    [Fact]
    public void TimelineReappliesOnClipHistoryOrMediaChangesButNotOnPlaybackTicks()
    {
        var first = View();
        Assert.Equal(new ViewChanges(false, false, false), ViewChanges.Between(first, first with { Revision = 2 }));
        Assert.True(ViewChanges.Between(first, View(generation: 2)).Timeline);
        Assert.True(ViewChanges.Between(first, View(history: new HistoryStatus(true, false, "add a.mkv", null, 1))).Timeline);
        Assert.True(ViewChanges.Between(first, View(mediaSelected: false)).Timeline);
    }
}

using Avalonia;
using CutCap.Desktop.Controls;
using CutCap.Desktop.Services;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Phase 4A filmstrip layout, cache policy and viewport planning, without a running engine.</summary>
public sealed class FilmstripTests
{
    private const double Pps = 20; // 100% zoom

    /// <summary>A ready 16:9 clip at <paramref name="startMs"/> whose source interval is [inUs, endUs).</summary>
    private static ClipView Clip(string id, long startMs, long inUs, long endUs, int track = 0, string key = "src", string availability = "ready") =>
        new(id, $"t{track}", track, "m1", $"{id}.mp4", startMs, (endUs - inUs) / 1000, "", "", "", availability, false,
            startMs * 720, (startMs + (endUs - inUs) / 1000) * 720, 0, 0, null, null, startMs + (endUs - inUs) / 1000,
            key, inUs / 1000, endUs / 1000, inUs, endUs, 16.0 / 9);

    private static List<FilmCell> Cells(ClipView clip, double left, double right, long step = 1_000_000)
    {
        var scale = TimelineScale.ForZoom(100);
        var rect = TimelineGeometry.ClipRect(scale, clip, TimelineLanes.TrackHeight);
        var cells = new List<FilmCell>();
        FilmstripLayout.Cells(clip, rect.X, rect.Right, Pps, step, left, right, left, right, cells);
        return cells;
    }

    [Fact]
    public void CellsSitOnAFixedSourceGridSoMovesSplitsAndScrollsReuseSamples()
    {
        // Source in-point 10.5 s: a leading partial cell samples the in-point, then the 1 s grid.
        var clip = Clip("a", 5000, 10_500_000, 15_000_000);
        var cells = Cells(clip, 0, 10_000);
        Assert.Equal([10_500_000L, 11_000_000, 12_000_000, 13_000_000, 14_000_000], cells.Select(c => c.SampleUs));
        var scale = TimelineScale.ForZoom(100);
        Assert.Equal(scale.TimeToX(5000), cells[0].X, 6);
        Assert.Equal(10, cells[0].Width, 6); // half a second at 20 px/s
        Assert.Equal(scale.TimeToX(5500), cells[1].X, 6);
        // Moving the clip moves the cells, not the samples.
        var moved = Cells(clip with { StartMs = 40_000, StartTicks = 40_000 * 720, ExactEndMs = 44_500 }, 0, 10_000);
        Assert.Equal(cells.Select(c => c.SampleUs), moved.Select(c => c.SampleUs));
        Assert.Equal(scale.TimeToX(40_000), moved[0].X, 6);
        // A split's right part (source 12.25-15 s) reuses the same grid samples.
        var right = Cells(Clip("b", 7000, 12_250_000, 15_000_000), 0, 10_000);
        Assert.Equal([12_250_000L, 13_000_000, 14_000_000], right.Select(c => c.SampleUs));
        // No sample before the in-point, none at or after the end.
        Assert.All(cells, c => Assert.InRange(c.SampleUs, 10_500_000, 14_999_999));
    }

    [Fact]
    public void OnlyCellsInsideTheRequestedWindowAreLaidOutAndVisibilityIsMarked()
    {
        var clip = Clip("long", 0, 0, 600_000_000); // ten minutes, 12,000 px
        var scale = TimelineScale.ForZoom(100);
        var cells = new List<FilmCell>();
        FilmstripLayout.Cells(clip, scale.TimeToX(0), scale.TimeToX(600_000), Pps, 4_000_000, 2000, 3600, 2400, 3200, cells);
        Assert.InRange(cells.Count, 20, 22); // 1600 px / 80 px cells, not 150 cells for the whole clip
        Assert.All(cells, c => Assert.True(c.X < 3600 && c.X + c.Width > 2000));
        Assert.Contains(cells, c => !c.Visible);
        Assert.Equal(cells.Count(c => c.X < 3200 && c.X + c.Width > 2400), cells.Count(c => c.Visible));
        // Zooming out halves the pixel rate: the 4 s grid now needs half the cells per pixel range.
        var zoomedOut = new List<FilmCell>();
        FilmstripLayout.Cells(clip, 12, 12 + 6000, 10, 8_000_000, 2000, 3600, 2400, 3200, zoomedOut);
        Assert.InRange(zoomedOut.Count, 20, 22);
        Assert.Equal(8_000_000, zoomedOut[1].SampleUs - zoomedOut[0].SampleUs);
    }

    [Fact]
    public void StepsFollowTheCellWidthAndZoomAndShortClipsStillGetOneSample()
    {
        var width = FilmstripLayout.CellWidth(TimelineLanes.StripHeight, 16.0 / 9);
        Assert.Equal(TimelineLanes.StripHeight * 16 / 9, width, 6);
        Assert.Equal(0, FilmstripLayout.CellWidth(40, 0));
        Assert.Equal((long)Math.Round(width / 20 * 1e6), FilmstripLayout.StepUs(width, 20));
        Assert.Equal((long)Math.Round(width / 40 * 1e6), FilmstripLayout.StepUs(width, 40));
        // A 200 ms clip narrower than one cell: one sample at its in-point.
        var tiny = Cells(Clip("t", 0, 1_100_000, 1_300_000), 0, 1000, FilmstripLayout.StepUs(width, 20));
        Assert.Single(tiny);
        Assert.Equal(1_100_000, tiny[0].SampleUs);
        Assert.Equal(4, tiny[0].Width, 6);
        Assert.Empty(Cells(Clip("e", 0, 5, 5), 0, 1000));
    }

    [Fact]
    public void FramesAreDrawnOnlyInsideTheClipSourceInterval()
    {
        var clip = Clip("a", 0, 2_000_000, 4_000_000) with { SourceInTicks = 2000, SourceEndTicks = 4000 };
        Assert.True(FilmstripLayout.Belongs(clip, 2000));
        Assert.True(FilmstripLayout.Belongs(clip, 3999));
        Assert.False(FilmstripLayout.Belongs(clip, 4000), "a successor clip's frame never leaks in");
        Assert.False(FilmstripLayout.Belongs(clip, 1999));
    }

    private sealed class Image : IDisposable
    {
        public int Disposed { get; private set; }
        public void Dispose() => Disposed++;
    }

    [Fact]
    public void CacheRemembersNegativeResultsWithoutBytesAndIgnoresUndemandedResults()
    {
        using var cache = new ThumbnailCache(1024 * 1024); // minimum budget: 1 MiB
        Assert.Equal(1024 * 1024, new ThumbnailCache(1).BudgetBytes);
        var keys = Enumerable.Range(0, 6).Select(i => new ThumbKey("src", i * 1_000_000L, 43)).ToArray();
        var tokens = keys.Select(cache.TokenFor).ToArray();
        Assert.Equal(tokens[2], cache.TokenFor(keys[2])); // stable while pending
        Assert.Equal(6, cache.PendingCount);
        for (var i = 0; i < 5; i++) Assert.True(cache.Complete(tokens[i], ThumbnailState.NoFrame, null, i, 300 * 1024));
        Assert.Equal(0, cache.ResidentBytes); // known no-frame/failed samples hold no bytes
        Assert.True(cache.TryGet(keys[0], out var first));
        Assert.Equal(ThumbnailState.NoFrame, first.State);
        Assert.False(first.HasImage);
        // A result for a token no longer demanded is dropped and its image disposed.
        cache.RetainPending(new HashSet<ulong>());
        var late = new Image();
        Assert.False(cache.Complete(tokens[5], ThumbnailState.Ready, late, 0, 1));
        Assert.Equal(1, late.Disposed);
        Assert.False(cache.Contains(keys[5]));
        Assert.NotEqual(tokens[5], cache.TokenFor(keys[5])); // re-demanding issues a new token
        Assert.Equal(1, cache.Hits);
        Assert.False(cache.TryGet(new ThumbKey("other", 0, 43), out _));
        Assert.Equal(1, cache.Misses);
        // Project replacement voids pending tokens but keeps source-keyed results.
        cache.ClearPending();
        Assert.Equal(0, cache.PendingCount);
        Assert.True(cache.Contains(keys[1]));
    }

    [Fact]
    public void ImagesAreBudgetedEvictedLeastRecentlyUsedFirstAndDisposed()
    {
        const int size = 256 * 1024;
        var cache = new ThumbnailCache(1024 * 1024);
        var images = new List<Image>();
        for (var i = 0; i < 6; i++)
        {
            var image = new Image();
            images.Add(image);
            Assert.True(cache.Complete(cache.TokenFor(new ThumbKey("src", i, 43)), ThumbnailState.Ready, image, i, size));
            if (i == 3) cache.TryGet(new ThumbKey("src", 0, 43), out _); // key 0 becomes the most recently used
        }
        // 6 x 256 KiB > 1 MiB: two evictions, least recently used first (keys 1 and 2), never key 0.
        Assert.Equal(2, cache.Evictions);
        Assert.Equal(4 * size, cache.ResidentBytes);
        Assert.Equal(5 * size, cache.PeakResidentBytes);
        Assert.True(cache.Contains(new ThumbKey("src", 0, 43)));
        Assert.False(cache.Contains(new ThumbKey("src", 1, 43)));
        Assert.False(cache.Contains(new ThumbKey("src", 2, 43)));
        Assert.Equal([0, 1, 1, 0, 0, 0], images.Select(i => i.Disposed));
        // Replacing a resident key frees the old image; disposing the cache frees the rest exactly once.
        var replacement = new Image();
        cache.Complete(cache.TokenFor(new ThumbKey("src", 5, 43)), ThumbnailState.Ready, replacement, 5, size);
        Assert.Equal(1, images[5].Disposed);
        cache.Dispose();
        Assert.Equal(0, cache.ResidentBytes);
        Assert.Equal([1, 1, 1, 1, 1, 1], images.Select(i => i.Disposed));
        Assert.Equal(1, replacement.Disposed);
    }

    [Fact]
    public void PlannerDemandsVisibleSamplesFirstSkipsCachedOnesAndIgnoresUnavailableClips()
    {
        var lanes = new TimelineLanes
        {
            Lanes = [new LaneInfo(true, false, "", "t0"), new LaneInfo(false, false, "", "t1")],
            Viewport = new Size(800, 300),
            Thumbnails = new ThumbnailCache(ThumbnailCache.DefaultBudgetBytes),
            Clips =
            [
                Clip("a", 0, 0, 300_000_000),
                Clip("missing", 900_000, 0, 30_000_000, key: "", availability: "missing"),
            ],
        };
        var wants = lanes.PlanThumbnails();
        Assert.NotEmpty(wants);
        Assert.InRange(wants.Count, 10, TimelineLanes.MaxDemand);
        Assert.True(lanes.LastPlannedVisible > 0);
        Assert.All(wants, w => Assert.Equal("src", w.SourceKey));
        // Visible samples come first, in left-to-right order; overscan after.
        var visibleSamples = wants.Take(lanes.LastPlannedVisible).Select(w => w.SampleUs).ToArray();
        Assert.Equal(visibleSamples.OrderBy(s => s), visibleSamples);
        Assert.Equal(0, visibleSamples[0]);
        // Scrolling right: the demand moves with the viewport and old samples are not re-sent.
        lanes.ScrollOffset = new Vector(4000, 0);
        var scrolled = lanes.PlanThumbnails();
        Assert.True(scrolled[0].SampleUs > wants[lanes.LastPlannedVisible - 1].SampleUs);
        Assert.DoesNotContain(scrolled, w => w.SampleUs == 0);
        // Cached samples are skipped; tokens are stable per sample while pending.
        var cache = lanes.Thumbnails!;
        cache.Complete(scrolled[0].Token, ThumbnailState.NoFrame, null, 0, 0);
        var again = lanes.PlanThumbnails();
        Assert.DoesNotContain(again, w => w.SampleUs == scrolled[0].SampleUs);
        Assert.Equal(scrolled[1].Token, again.First(w => w.SampleUs == scrolled[1].SampleUs).Token);
    }

    /// <summary>A lane of adjacent 3 s clips, scrolled well inside it (clip 2000 at 100% zoom).</summary>
    private static TimelineLanes LaneOfThreeSecondClips(int count) => new()
    {
        Lanes = [new LaneInfo(true, false, "", "t0")],
        Viewport = new Size(1600, 200),
        Thumbnails = new ThumbnailCache(ThumbnailCache.DefaultBudgetBytes),
        Clips = Enumerable.Range(0, count).Select(i => Clip($"c{i}", i * 3000L, 0, 3_000_000)).ToArray(),
        ScrollOffset = new Vector(120_000, 0),
    };

    [Fact]
    public void FiveThousandClipsPlanOnlyTheViewport()
    {
        var lanes = LaneOfThreeSecondClips(5000);
        var wants = lanes.PlanThumbnails();
        // 3 s clips share samples 0 and the in-point: the viewport needs only a handful of distinct samples.
        Assert.InRange(wants.Count, 1, 8);
        // The viewport and its overscan span 2400 px, and each clip is 60 px wide at 100%
        // zoom: 40 clips, plus one partly visible at each edge and the culling allowance.
        Assert.InRange(lanes.LastPlannedClipsExamined, 40, 44);
    }

    [Fact]
    public void PlanningWorkFollowsTheViewportNotTheProject()
    {
        var modest = LaneOfThreeSecondClips(5_000);
        var huge = LaneOfThreeSecondClips(100_000);
        var wants = modest.PlanThumbnails();
        Assert.Equal(wants.Select(w => w.SampleUs), huge.PlanThumbnails().Select(w => w.SampleUs));
        Assert.Equal(modest.LastPlannedClipsExamined, huge.LastPlannedClipsExamined);
    }

    /// <summary>
    /// Opt-in measurement (dotnet test --filter ... -- xUnit.Explicit=on). A single cold
    /// call times the JIT and the machine's load, so this warms up and reports a
    /// distribution, and it never fails on a duration.
    /// </summary>
    [Fact(Explicit = true)]
    public void MeasureThumbnailPlanningAtFiveThousandClips()
    {
        var lanes = LaneOfThreeSecondClips(5000);
        var watch = System.Diagnostics.Stopwatch.StartNew();
        lanes.PlanThumbnails();
        var cold = watch.Elapsed;
        for (var i = 0; i < 2000; i++) { lanes.PlanThumbnails(); }
        var samples = new double[2000];
        for (var i = 0; i < samples.Length; i++)
        {
            watch.Restart();
            lanes.PlanThumbnails();
            samples[i] = watch.Elapsed.TotalMicroseconds;
        }
        Array.Sort(samples);
        TestContext.Current.TestOutputHelper?.WriteLine(
            $"plan 5000 clips: cold={cold.TotalMicroseconds:F0} us warm median={samples[samples.Length / 2]:F1} us p99={samples[samples.Length * 99 / 100]:F1} us max={samples[^1]:F1} us examined={lanes.LastPlannedClipsExamined}");
    }
}

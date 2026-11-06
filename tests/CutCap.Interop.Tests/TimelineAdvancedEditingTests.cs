using System.Diagnostics;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Phase 3E edits over the real ABI with real FFmpeg media: the frames a
/// timeline position shows must not change when the clip under it is split,
/// trimmed (retained part) or moved (shifted position), including VFR and
/// nonzero-start sources; timeline playback continues on the edited timeline.
/// </summary>
[Collection("engine")]
public sealed class TimelineAdvancedEditingTests
{
    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-advanced-edit-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }

    private static string Fixture(string name) =>
        Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../../tests/fixtures", name));

    private static SessionView Until(EngineSession s, Func<SessionView, bool> done, int seconds = 25, bool copy = false)
    {
        var watch = Stopwatch.StartNew();
        while (true)
        {
            s.Pump();
            // Like the frontend: copying acknowledges delivery and advances the copied clip.
            if (copy && s.TryGetFrameInfo(out var frame))
            {
                var bytes = new byte[checked((int)(frame.Stride * frame.Height))];
                s.CopyFrame(frame.Generation, bytes, (int)frame.Stride);
            }
            var view = s.ReadView();
            if (done(view)) return view;
            Assert.True(watch.Elapsed < TimeSpan.FromSeconds(seconds),
                $"{view.Status}; {view.TimelinePreview?.State} {view.TimelinePreview?.PositionMs}; {view.TimelinePlayback?.State}");
            Thread.Sleep(5);
        }
    }

    private static SessionView Settled(EngineSession s, int clips) =>
        Until(s, v => v.Pending == 0 && v.Project?.ClipCount == clips);

    private static (string Media, string Track, ClipView Clip) Placed(EngineSession s, string fixture, long atMs)
    {
        InteropTests.RequireFfmpeg();
        Until(s, v => v.EngineReady && v.Pending == 0);
        s.Import(Fixture(fixture));
        var imported = Until(s, v => v.Media.Count == 1 && !v.Import.Busy && v.Frame is not null && !v.Source.Busy);
        var media = imported.Media[0].Id;
        var track = imported.Tracks[0].Id;
        s.PlaceClip(media, track, TimelinePosition.Milliseconds(atMs));
        Settled(s, 1);
        return (media, track, s.ReadTimeline().Clips[0]);
    }

    /// <summary>
    /// What the timeline shows at a position: the exactly mapped source tick
    /// (<c>Requested</c>) and the actual PTS of the frame presented, if any.
    /// </summary>
    private static (long? Requested, long? Actual) Shown(EngineSession s, long ms)
    {
        s.SeekTimelineMilliseconds(ms);
        var v = Until(s, v => v.TimelinePreview is { Busy: false } p && p.PositionMs == ms
            && p.State is "ready" or "blank" or "no_frame");
        var p = v.TimelinePreview!;
        return (p.Requested?.Ticks, p.State == "ready" ? v.Frame?.Actual?.Ticks : null);
    }

    /// <summary>
    /// Continuity after an edit: the mapped source tick is identical, and so is
    /// the frame, unless the still policy's ceiling frame now lies past the end
    /// of the shorter part (Phase 3C: a successor frame never leaks across a cut).
    /// </summary>
    private static void SameSource((long? Requested, long? Actual) before, (long? Requested, long? Actual) after, long? partEnd, string where)
    {
        Assert.Equal(before.Requested, after.Requested);
        if (after.Actual is null && partEnd is { } end && before.Actual >= end) return;
        Assert.True(before.Actual == after.Actual, $"{where}: {before} -> {after}");
    }

    [Theory]
    [InlineData("colors-vfr.mkv")]
    [InlineData("offset.mkv")]
    public void SplitTrimAndMoveKeepTheExactFramesUnderEveryRetainedPosition(string fixture)
    {
        using var s = EngineSession.Start();
        var (_, track, clip) = Placed(s, fixture, 0);
        var samples = Enumerable.Range(0, 14).Select(i => i * 137L).Where(ms => ms < clip.DurationMs).ToArray();
        var before = samples.Select(ms => Shown(s, ms)).ToArray();
        Assert.Contains(before, p => p.Actual is not null);

        // Split off-grid in the middle.
        var cut = clip.DurationMs / 2 + 1;
        s.SplitClip(clip.Id, TimelinePosition.Milliseconds(cut));
        Settled(s, 2);
        var parts = s.ReadTimeline().Clips.OrderBy(c => c.StartTicks).ToArray();
        Assert.Equal(clip.Id, parts[0].Id);
        Assert.Equal(parts[0].EndTicks, parts[1].StartTicks);
        var leftEnd = Shown(s, parts[1].StartMs + 1).Requested;
        for (var i = 0; i < samples.Length; i++)
        {
            var inLeft = samples[i] < parts[1].StartMs;
            SameSource(before[i], Shown(s, samples[i]), inLeft ? leftEnd : null, $"split {samples[i]} ms");
        }

        // Trim the left part's start: retained positions keep their mapping; trimmed ones are blank.
        s.TrimClip(parts[0].Id, ClipEdge.Start, TimelinePosition.Milliseconds(200));
        Until(s, v => v.Pending == 0 && s.ReadTimeline().Clips.Any(c => c.Id == clip.Id && c.StartMs >= 200));
        var left = s.ReadTimeline().Clips.Single(c => c.Id == clip.Id);
        for (var i = 0; i < samples.Length; i++)
        {
            if (samples[i] > left.StartMs)
                SameSource(before[i], Shown(s, samples[i]), samples[i] < parts[1].StartMs ? leftEnd : null, $"trim {samples[i]} ms");
        }
        Assert.Null(Shown(s, 50).Actual);

        // Move the right part 4 s later: its frames shift with it; the gap is blank.
        var right = s.ReadTimeline().Clips.Single(c => c.Id != clip.Id);
        s.MoveClip(right.Id, track, TimelinePosition.Ticks(right.StartTicks + 4 * 720_000), ClipEdge.Start);
        Until(s, v => v.Pending == 0 && s.ReadTimeline().Clips.Single(c => c.Id == right.Id).StartTicks == right.StartTicks + 4 * 720_000);
        for (var i = 0; i < samples.Length; i++)
        {
            if (samples[i] > right.StartMs) SameSource(before[i], Shown(s, samples[i] + 4000), null, $"move {samples[i]} ms");
        }
        Assert.Null(Shown(s, right.StartMs + 2000).Actual);

        // One undo per gesture, back to the single original clip.
        for (var i = 0; i < 3; i++) s.Undo();
        Until(s, v => v.Pending == 0 && v.Project?.ClipCount == 1 && v.History?.Depth == 1);
        var restored = s.ReadTimeline().Clips[0];
        Assert.Equal((clip.Id, clip.StartTicks, clip.EndTicks, clip.MinStartTicks, clip.MaxEndTicks),
            (restored.Id, restored.StartTicks, restored.EndTicks, restored.MinStartTicks, restored.MaxEndTicks));
    }
    [Fact]
    public void TimelinePlaybackRunsAcrossAnEditedCutAndGapAfterTheEditStopsIt()
    {
        using var s = EngineSession.Start();
        var (_, track, clip) = Placed(s, "colors-cfr.mkv", 0);
        s.SplitClip(clip.Id, TimelinePosition.Milliseconds(1000));
        Settled(s, 2);
        var right = s.ReadTimeline().Clips.Single(c => c.Id != clip.Id);
        s.MoveClip(right.Id, track, TimelinePosition.Milliseconds(1500), ClipEdge.Start);
        Until(s, v => v.Pending == 0 && s.ReadTimeline().Clips.Single(c => c.Id == right.Id).StartMs == 1500);
        Shown(s, 600);
        s.PlayTimeline();
        var crossed = Until(s, v => v.TimelinePlayback is { State: PlaybackState.Playing } p && p.CopiedClipId == right.Id, 20, copy: true);
        Assert.Equal(right.Id, crossed.TimelinePlayback!.CopiedClipId);
        Assert.True(crossed.TimelinePlayback.Copied > 0);
        // A structural edit while playing stops the transport and does not resume.
        s.TrimClip(right.Id, ClipEdge.End, TimelinePosition.Milliseconds(2800));
        var stopped = Until(s, v => v.Pending == 0 && v.TimelinePlayback?.State != PlaybackState.Playing && s.ReadTimeline().Clips.Single(c => c.Id == right.Id).EndTicks == 2800 * 720);
        Assert.False(stopped.TimelinePlayback!.DesiredPlaying);
        // Recovery: Play again on the edited timeline until a frame is copied (measured).
        var recovery = Stopwatch.StartNew();
        s.PlayTimeline();
        Until(s, v => v.TimelinePlayback is { State: PlaybackState.Playing, Copied: > 0 }, 20, copy: true);
        TestContext.Current.TestOutputHelper?.WriteLine($"transport recovery after edit: {recovery.ElapsedMilliseconds} ms to the first copied frame");
        s.StopTimeline();
    }

    [Fact]
    public void DropPlacementSaveAndReopenPreserveIdentitiesAndSourceRanges()
    {
        using var files = new Workspace();
        var project = files.At("advanced.cutcap");
        IReadOnlyList<ClipView> edited;
        using (var s = EngineSession.Start())
        {
            var (media, track, clip) = Placed(s, "offset.mkv", 2000);
            Assert.Equal(2000, clip.StartMs);
            s.AddTrack(TrackKind.Video);
            var tracks = Until(s, v => v.Tracks.Count == 3 && v.Pending == 0).Tracks;
            s.MoveClip(clip.Id, tracks[2].Id, TimelinePosition.Milliseconds(500), ClipEdge.Start);
            s.PlaceClip(media, track, TimelinePosition.Ticks(0));
            Settled(s, 2);
            var placed = s.ReadTimeline().Clips.Single(c => c.Id != clip.Id);
            s.TrimClip(placed.Id, ClipEdge.Start, TimelinePosition.Milliseconds(700));
            s.TrimClip(placed.Id, ClipEdge.End, TimelinePosition.Milliseconds(1900));
            Until(s, v => v.Pending == 0 && v.History?.Depth == 5);
            // Overlapping drop: rejected, nothing changes.
            s.PlaceClip(media, track, TimelinePosition.Milliseconds(1000));
            var rejected = Until(s, v => v.Status.Error && v.Pending == 0);
            Assert.Contains("overlap", rejected.Status.Text);
            edited = s.ReadTimeline().Clips;
            Assert.Equal(2, edited.Count);
            s.SaveProjectAs(project);
            Until(s, v => v.Project?.Document?.State == "saved" && v.Pending == 0);
        }
        using (var s = EngineSession.Start())
        {
            Until(s, v => v.EngineReady && v.Pending == 0);
            s.OpenProject(project);
            var opened = Until(s, v => v.Project?.Document?.State == "opened" && v.Pending == 0
                && v.Media.All(m => m.Availability == "ready"));
            var reopened = s.ReadTimeline().Clips;
            Assert.Equal(edited.Select(c => (c.Id, c.TrackId, c.StartTicks, c.EndTicks, c.MinStartTicks, c.MaxEndTicks)),
                reopened.Select(c => (c.Id, c.TrackId, c.StartTicks, c.EndTicks, c.MinStartTicks, c.MaxEndTicks)));
            Assert.False(opened.History!.CanUndo);
            Assert.False(opened.Project!.Document!.Dirty);
            // Source playback is independent of the edited timeline.
            s.SelectMedia(opened.Media[0].Id);
            Until(s, v => v.Frame is not null && !v.Source.Busy && v.Source.Ready);
            s.SetPreviewMode(PreviewMode.Source);
            s.Play();
            Until(s, v => v.Playback is { Offered: > 2 });
            Assert.Equal(reopened, s.ReadTimeline().Clips);
            s.Stop();
        }
    }
}

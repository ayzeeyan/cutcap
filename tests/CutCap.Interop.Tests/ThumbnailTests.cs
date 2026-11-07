using System.Diagnostics;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Phase 4A filmstrip thumbnails over the real ABI 8 with real FFmpeg: actual
/// decoded PTS and pixels, the source interval of trimmed/split clips, stale
/// results after relink or project replacement, and that thumbnail work never
/// changes the document. The CFR fixture is 4 fps: red 0-1 s, green 1-2 s,
/// blue 2-3 s.
/// </summary>
[Collection("engine")]
public sealed class ThumbnailTests
{
    private const uint Ready = 0, NoFrame = 1;
    private const int Height = 40;

    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-thumbnails-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }

    private sealed record Result(NativeThumbnailInfo Info, byte[]? Pixels)
    {
        public long ActualMs => Info.ActualTicks * Info.TimebaseNumerator * 1000 / Info.TimebaseDenominator;

        /// <summary>Mean RGB of the thumbnail's centre quarter.</summary>
        public (int R, int G, int B) Centre()
        {
            long r = 0, g = 0, b = 0, n = 0;
            var (w, h, stride) = ((int)Info.Width, (int)Info.Height, (int)Info.Stride);
            for (var y = h / 4; y < 3 * h / 4; y++)
            {
                for (var x = w / 4; x < 3 * w / 4; x++)
                {
                    var o = y * stride + x * 4;
                    r += Pixels![o]; g += Pixels[o + 1]; b += Pixels[o + 2]; n++;
                }
            }
            return ((int)(r / n), (int)(g / n), (int)(b / n));
        }
    }

    private static string Fixture(string name) =>
        Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../../tests/fixtures", name));

    private static SessionView Until(EngineSession s, Func<SessionView, bool> done, int seconds = 25)
    {
        var watch = Stopwatch.StartNew();
        while (true)
        {
            s.Pump();
            var view = s.ReadView();
            if (done(view)) return view;
            Assert.True(watch.Elapsed < TimeSpan.FromSeconds(seconds), $"{view.Status}; thumbnails {view.Thumbnails}");
            Thread.Sleep(5);
        }
    }

    /// <summary>Like the frontend: pump, then announce and copy every waiting thumbnail.</summary>
    private static Dictionary<ulong, Result> Drain(EngineSession s, int expected, int milliseconds = 20_000)
    {
        var results = new Dictionary<ulong, Result>();
        var watch = Stopwatch.StartNew();
        while (results.Count < expected && watch.ElapsedMilliseconds < milliseconds)
        {
            s.Pump();
            while (s.TryNextThumbnail(out var info))
            {
                byte[]? pixels = null;
                if (info.Status == Ready)
                {
                    pixels = new byte[checked((int)(info.Stride * info.Height))];
                    Assert.True(s.CopyThumbnail(info.Token, pixels, (int)info.Stride));
                    // The slot is released by the copy: a second copy of the same token is stale.
                    Assert.False(s.CopyThumbnail(info.Token, pixels, (int)info.Stride));
                }
                results[info.Token] = new Result(info, pixels);
            }
            Thread.Sleep(5);
        }
        return results;
    }

    private static (string Media, string Track) Imported(EngineSession s, string path)
    {
        InteropTests.RequireFfmpeg();
        Until(s, v => v.EngineReady && v.Pending == 0);
        s.Import(path);
        var view = Until(s, v => v.Media.Count == 1 && !v.Import.Busy && v.Frame is not null && !v.Source.Busy);
        return (view.Media[0].Id, view.Tracks[0].Id);
    }

    private static ClipView[] Clips(EngineSession s, int count)
    {
        Until(s, v => v.Pending == 0 && v.Project?.ClipCount == count);
        return [.. s.ReadTimeline().Clips.OrderBy(c => c.StartMs)];
    }

    private static ThumbnailWant Want(ulong token, ClipView clip, long sampleUs) =>
        new(token, clip.MediaId, clip.SourceKey, sampleUs, Height);

    private static void AssertColour((int R, int G, int B) c, char expected, string where)
    {
        var ok = expected switch
        {
            'r' => c.R > 180 && c.G < 70 && c.B < 70,
            'g' => c.G > 180 && c.R < 70 && c.B < 70,
            'b' => c.B > 180 && c.R < 70 && c.G < 70,
            _ => false,
        };
        Assert.True(ok, $"{where}: expected {expected}, got {c}");
    }

    [Fact]
    public void RealThumbnailsCarryTheActualDecodedPtsAndPixelsWithoutChangingTheDocument()
    {
        using var workspace = new Workspace();
        using var session = EngineSession.Start();
        var (media, track) = Imported(session, Fixture("colors-cfr.mkv"));
        session.PlaceClip(media, track, TimelinePosition.Milliseconds(0));
        var clip = Clips(session, 1)[0];
        Assert.NotEmpty(clip.SourceKey);
        Assert.Equal(16.0 / 9, clip.ThumbAspect, 3);
        Assert.Equal((0L, 3_000_000L), (clip.SourceInUs, clip.SourceEndUs));
        session.SaveProjectAs(workspace.At("thumbs.cutcap"));
        var saved = Until(session, v => v.Project?.Document is { Dirty: false, State: not "saving" } && v.Pending == 0);
        var revision = saved.Project!.Document!.Revision;

        // Nominal 4 fps would put 1.1 s on frame 4 (1.0 s): the policy is the first actual PTS >= the sample.
        long[] samples = [0, 100_000, 1_100_000, 2_100_000, 2_750_000, 3_500_000];
        session.RequestThumbnails([.. samples.Select((us, i) => Want((ulong)i + 1, clip, us))]);
        var results = Drain(session, samples.Length);
        Assert.Equal(samples.Length, results.Count);
        long[] actualMs = [0, 250, 1250, 2250, 2750];
        var colours = "rrgbb";
        for (var i = 0; i < actualMs.Length; i++)
        {
            var result = results[(ulong)i + 1];
            Assert.Equal(Ready, result.Info.Status);
            Assert.Equal(actualMs[i], result.ActualMs);
            Assert.Equal((uint)Height, result.Info.Height);
            Assert.InRange(result.Info.Width, 70u, 72u); // 16:9 at 40 px
            Assert.True(result.Info.Stride >= result.Info.Width * 4);
            Assert.True(FilmstripBelongs(clip, result.Info.ActualTicks));
            AssertColour(result.Centre(), colours[i], $"sample {samples[i]} us");
        }
        Assert.Equal(NoFrame, results[6].Info.Status); // past the source end
        Assert.Null(results[6].Pixels);

        var after = Until(session, v => v.Thumbnails is { Pending: 0, InFlight: 0 });
        Assert.False(after.Project!.Document!.Dirty);
        Assert.Equal(revision, after.Project.Document.Revision);
        Assert.True(after.Thumbnails!.Decoded >= 5, $"{after.Thumbnails}");
        Assert.InRange(after.Thumbnails.Batches, 1ul, 2ul); // six samples of one source: one batch (≤ 6)
        Assert.NotNull(after.Thumbnails.LastProcessId);
    }

    private static bool FilmstripBelongs(ClipView clip, long ticks) => ticks >= clip.SourceInTicks && ticks < clip.SourceEndTicks;

    [Fact]
    public void SplitAndTrimRowsBoundEachFilmstripToItsOwnSourceInterval()
    {
        using var session = EngineSession.Start();
        var (media, track) = Imported(session, Fixture("colors-cfr.mkv"));
        session.PlaceClip(media, track, TimelinePosition.Milliseconds(2000));
        var whole = Clips(session, 1)[0];
        session.SplitClip(whole.Id, TimelinePosition.Milliseconds(3500));
        var parts = Clips(session, 2);
        var (left, right) = (parts[0], parts[1]);
        Assert.Equal(left.SourceEndTicks, right.SourceInTicks); // contiguous, exclusive end
        Assert.Equal(1_500_000, right.SourceInUs);
        Assert.Equal(whole.SourceEndTicks, right.SourceEndTicks);
        Assert.Equal(whole.SourceKey, right.SourceKey); // split parts share decoded thumbnails

        session.TrimClip(left.Id, ClipEdge.Start, TimelinePosition.Milliseconds(2600));
        var trimmed = Clips(session, 2)[0];
        // The trimmed in-point lies on the source tick grid; the first cell samples it (ceiling to µs).
        Assert.InRange(trimmed.SourceInUs, 600_000, 601_000);
        Assert.Equal(left.SourceEndTicks, trimmed.SourceEndTicks);

        session.RequestThumbnails(
        [
            Want(1, trimmed, trimmed.SourceInUs),
            Want(2, right, right.SourceInUs),
            Want(3, trimmed, trimmed.SourceEndUs - 1), // last instant of the left part: its own last frame or none
        ]);
        var results = Drain(session, 3);
        Assert.Equal(750, results[1].ActualMs); // first frame at/after 0.6 s
        Assert.True(FilmstripBelongs(trimmed, results[1].Info.ActualTicks));
        AssertColour(results[1].Centre(), 'r', "trimmed left in-point");
        Assert.Equal(1500, results[2].ActualMs);
        Assert.True(FilmstripBelongs(right, results[2].Info.ActualTicks));
        AssertColour(results[2].Centre(), 'g', "right part in-point");
        // The frame after the left part's last instant is the right part's first frame: never drawn on the left.
        Assert.Equal(1500, results[3].ActualMs);
        Assert.False(FilmstripBelongs(trimmed, results[3].Info.ActualTicks));
    }

    [Fact]
    public void ResultsForARelinkedOrReplacedSourceAreNeverDelivered()
    {
        using var workspace = new Workspace();
        var original = workspace.At("original.mkv");
        var copy = workspace.At("relinked copy.mkv");
        File.Copy(Fixture("colors-cfr.mkv"), original);
        File.Copy(original, copy);
        using var session = EngineSession.Start();
        var (media, track) = Imported(session, original);
        session.PlaceClip(media, track, TimelinePosition.Milliseconds(0));
        var clip = Clips(session, 1)[0];

        // A want naming an unknown key is dropped before any decoding.
        session.RequestThumbnails([Want(9, clip with { SourceKey = clip.SourceKey + "x" }, 0)]);
        Assert.Empty(Drain(session, 1, 1500));

        // Queue work for the old key, then relink before taking any result.
        session.RequestThumbnails([Want(1, clip, 0), Want(2, clip, 1_100_000), Want(3, clip, 2_100_000)]);
        session.RelinkMedia(media, copy);
        Until(session, v => v.Media[0].Availability == "ready" && s(session) != clip.SourceKey);
        var relinked = session.ReadTimeline().Clips[0];
        Assert.NotEqual(clip.SourceKey, relinked.SourceKey);
        Assert.Empty(Drain(session, 1, 2500)); // finished or not, old-key results are discarded

        session.RequestThumbnails([Want(4, relinked, 1_100_000)]);
        var fresh = Drain(session, 1);
        Assert.Equal(1250, fresh[4].ActualMs);
        AssertColour(fresh[4].Centre(), 'g', "after relink");

        // A new project voids everything still queued.
        session.RequestThumbnails([Want(5, relinked, 0), Want(6, relinked, 2_100_000)]);
        session.NewProject();
        Until(session, v => v.Media.Count == 0 && v.Pending == 0);
        Assert.Empty(Drain(session, 1, 2500));
        session.RequestThumbnails([Want(7, relinked, 0)]);
        Assert.Empty(Drain(session, 1, 1500));
        Assert.Equal(0, session.ReadView().Thumbnails!.Pending);
    }

    private static string s(EngineSession session) => session.ReadTimeline().Clips.FirstOrDefault()?.SourceKey ?? "";

    /// <summary>
    /// Opt-in measurement (CUTCAP_PERF_SOURCE = a long-GOP 1080p file; run with
    /// <c>-- xUnit.Explicit=on</c>): timeline playback start-up (Play → Playing)
    /// with the thumbnail worker idle versus with a batch of distant samples
    /// decoding, alternated to cancel out caching effects.
    /// </summary>
    [Fact(Explicit = true)]
    public void MeasurePlaybackStartupWhileThumbnailsDecode()
    {
        var path = Environment.GetEnvironmentVariable("CUTCAP_PERF_SOURCE");
        Assert.SkipUnless(path is not null && File.Exists(path), "Set CUTCAP_PERF_SOURCE to a long 1080p source.");
        using var session = EngineSession.Start();
        var (media, track) = Imported(session, path!);
        session.PlaceClip(media, track, TimelinePosition.Milliseconds(0));
        var clip = Clips(session, 1)[0];
        session.SetPreviewMode(PreviewMode.Timeline);
        ulong token = 100;
        var rows = new List<string>();
        // Same positions for both variants (start-up depends on the distance to
        // the previous keyframe), alternating which runs first; a distant seek
        // in between keeps each start cold.
        (long At, bool Busy)[] plan = [(30_000, false), (30_000, true), (70_000, true), (70_000, false),
            (110_000, false), (110_000, true), (150_000, true), (150_000, false)];
        foreach (var (at, busy) in plan)
        {
            session.SeekTimelineMilliseconds(at - 13_000);
            Until(session, v => v.TimelinePreview is { Busy: false } p && p.PositionMs == at - 13_000);
            session.SeekTimelineMilliseconds(at);
            Until(session, v => v.TimelinePreview is { Busy: false } p && p.PositionMs == at);
            if (busy)
            {
                // Six samples far from the playhead, never cached before.
                session.RequestThumbnails([.. Enumerable.Range(0, 6).Select(i =>
                    Want(token++, clip, 175_000_000L - rows.Count * 1_000_000L - i * 130_000L))]);
                Until(session, v => v.Thumbnails is { InFlight: > 0 });
            }
            var watch = Stopwatch.StartNew();
            session.PlayTimeline();
            var playing = Until(session, v => v.TimelinePlayback?.State == PlaybackState.Playing);
            var startup = watch.ElapsedMilliseconds;
            var stats = playing.Thumbnails!;
            rows.Add($"busy={busy} at={at} ms play_to_playing_ms={startup} request_to_ready_us={playing.TimelinePlayback!.RequestToReadyUs} thumbs_in_flight={stats.InFlight} throttled={stats.Throttled}");
            session.StopTimeline();
            Until(session, v => v.TimelinePlayback?.State != PlaybackState.Playing);
            Until(session, v => v.Thumbnails is { InFlight: 0, Pending: 0 }, 60);
            Drain(session, 6, 500);
        }
        // First play of a fresh session at 5 s (like the native run), idle versus busy, alternated.
        foreach (var busy in new[] { false, true, true, false, false, true })
        {
            using var fresh = EngineSession.Start();
            var (m, t) = Imported(fresh, path!);
            fresh.PlaceClip(m, t, TimelinePosition.Milliseconds(0));
            var c = Clips(fresh, 1)[0];
            fresh.SetPreviewMode(PreviewMode.Timeline);
            fresh.SeekTimelineMilliseconds(5000);
            Until(fresh, v => v.TimelinePreview is { Busy: false, PositionMs: 5000 });
            if (busy)
            {
                fresh.RequestThumbnails([.. Enumerable.Range(0, 6).Select(i => Want(token++, c, 60_000_000L + i * 17_000_000L))]);
                Until(fresh, v => v.Thumbnails is { InFlight: > 0 });
            }
            var watch = Stopwatch.StartNew();
            fresh.PlayTimeline();
            Until(fresh, v => v.TimelinePlayback?.State == PlaybackState.Playing);
            rows.Add($"fresh session first play busy={busy} at=5000 ms play_to_playing_ms={watch.ElapsedMilliseconds}");
        }
        foreach (var row in rows) TestContext.Current.TestOutputHelper?.WriteLine(row);
    }

    [Fact]
    public void MalformedAndOversizedDemandsAreRejectedAtTheBoundary()
    {
        using var session = EngineSession.Start();
        Until(session, v => v.EngineReady);
        var tooMany = Enumerable.Range(0, 129).Select(i => new ThumbnailWant((ulong)i + 1, "m", "k", 0, Height)).ToArray();
        Assert.Throws<EngineException>(() => session.RequestThumbnails(tooMany));
        Assert.False(session.TryNextThumbnail(out _));
        Assert.False(session.CopyThumbnail(42, new byte[16], 4)); // nothing announced: stale token
    }
}

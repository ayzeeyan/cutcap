using Avalonia;
using CutCap.Desktop.Controls;
using CutCap.Desktop.Services;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Phase 4B waveform cache policy and viewport planning, without a running engine.</summary>
public sealed class WaveformTests
{
    /// <summary>A ready linked audio clip on lane 1 at <paramref name="startMs"/> covering source samples [inS, endS) at 48 kHz.</summary>
    private static ClipView Audio(string id, long startMs, long inS, long endS, string key = "aud", string availability = "ready")
    {
        var durationMs = (endS - inS) * 1000 / 48_000;
        return new(id, "t1", 1, "m1", $"{id}.mp4", startMs, durationMs, "", "", "", availability, false,
            startMs * 720, (startMs + durationMs) * 720, 0, 0, null, null, startMs + durationMs,
            "", 0, 0, 0, 0, 0,
            Stream: "audio", AudioStream: 1, LinkId: "link-" + id, AudioKey: key, AudioRate: 48_000,
            SourceInSamples: inS, SourceEndSamples: endS);
    }

    private static WaveChunk Chunk(int buckets = 1024) => new(new float[buckets * 2], buckets, 256, 48_000);

    [Fact]
    public void CacheIsAByteBoundedLruWithStableTokensAndIgnoresUndemandedResults()
    {
        var cache = new WaveformCache(256 * 1024); // floor: 32 chunks of 8 KiB
        var a = new WaveKey("aud", 0);
        var token = cache.TokenFor(a);
        Assert.Equal(token, cache.TokenFor(a)); // stable while pending
        Assert.Equal(1, cache.PendingCount);
        Assert.False(cache.TryGet(a, out _));
        Assert.True(cache.Complete(token, Chunk()));
        Assert.False(cache.Complete(token, Chunk())); // a token completes once
        Assert.True(cache.TryGet(a, out var hit));
        Assert.Equal(1024, hit.Buckets);
        Assert.Equal((1L, 1L), (cache.Hits, cache.Misses));

        // Results nobody demands any more are dropped, not cached.
        var stale = cache.TokenFor(new WaveKey("aud", 1));
        cache.RetainPending(new HashSet<ulong>());
        Assert.False(cache.Complete(stale, Chunk()));
        Assert.False(cache.Contains(new WaveKey("aud", 1)));

        // Budget: least recently used chunks are evicted first; resident bytes never exceed it.
        for (var i = 2; i < 40; i++)
        {
            cache.TryGet(a, out _); // keep chunk 0 hot
            Assert.True(cache.Complete(cache.TokenFor(new WaveKey("aud", i)), Chunk()));
            Assert.True(cache.ResidentBytes <= cache.BudgetBytes);
        }
        Assert.True(cache.Evictions > 0);
        Assert.True(cache.Contains(a));
        Assert.False(cache.Contains(new WaveKey("aud", 2)));
        Assert.True(cache.Contains(new WaveKey("aud", 39)));

        // Project replacement voids pending tokens; analysed peaks stay (keyed by source identity).
        var pending = cache.TokenFor(new WaveKey("aud", 99));
        cache.ClearPending();
        Assert.False(cache.Complete(pending, Chunk()));
        Assert.True(cache.Contains(a));

        // A failed analysis is remembered as an empty chunk so it is not re-requested every frame.
        var failed = new WaveKey("bad", 0);
        Assert.True(cache.Complete(cache.TokenFor(failed), new WaveChunk(null, 0, 0, 0)));
        Assert.True(cache.Contains(failed));
    }

    [Fact]
    public void PlannerDemandsVisibleAudioChunksFirstAndSkipsCachedVideoAndUnavailableClips()
    {
        var cache = new WaveformCache(WaveformCache.DefaultBudgetBytes);
        // 30 minutes of audio at 48 kHz from t=0 (≈ 330 chunks of 262 144 frames); 100% zoom = 20 px/s.
        var lanes = new TimelineLanes
        {
            Lanes = [new LaneInfo(true, false, "", "t0"), new LaneInfo(false, false, "", "t1")],
            Viewport = new Size(800, 300),
            Waveforms = cache,
            Clips =
            [
                Audio("long", 0, 0, 48_000L * 1800),
                Audio("missing", 2_000_000, 0, 48_000L * 60, key: "gone", availability: "missing"),
            ],
        };
        var wants = lanes.PlanWaveforms();
        Assert.NotEmpty(wants);
        Assert.True(wants.Count <= TimelineLanes.MaxWaveDemand);
        Assert.All(wants, w => Assert.Equal(("aud", 1u), (w.AudioKey, w.Stream)));
        // 800 px at 20 px/s is 40 s = 1.92 M frames: chunks 0..7 are visible and come first, in order.
        Assert.Equal([0L, 1, 2, 3, 4, 5, 6, 7], wants.Take(8).Select(w => w.Chunk));
        Assert.Equal(wants.Count, wants.Select(w => w.Chunk).Distinct().Count());
        Assert.Equal(wants.Count, cache.PendingCount);

        // Cached chunks are skipped; tokens are stable while pending.
        Assert.True(cache.Complete(wants[0].Token, Chunk()));
        var again = lanes.PlanWaveforms();
        Assert.DoesNotContain(again, w => w.Chunk == 0);
        Assert.Equal(wants[1].Token, again.First(w => w.Chunk == 1).Token);

        // Scrolling far right: demand follows the viewport; chunks left behind are no longer pending.
        lanes.ScrollOffset = new Vector(20_000, 0); // 1000 s
        var scrolled = lanes.PlanWaveforms();
        var first = (long)Math.Floor((20_000 - 12) / 20.0 * 48_000 / TimelineLanes.ChunkFrames);
        Assert.InRange(scrolled[0].Chunk, first - 1, first + 1);
        Assert.DoesNotContain(scrolled, w => w.Chunk < 100);
        Assert.Equal(scrolled.Count, cache.PendingCount);
    }

    [Fact]
    public void ATrimmedClipOnlyDemandsTheChunksOfItsOwnSampleInterval()
    {
        var cache = new WaveformCache(WaveformCache.DefaultBudgetBytes);
        var inS = TimelineLanes.ChunkFrames * 3 + 10; // starts inside chunk 3
        var lanes = new TimelineLanes
        {
            Lanes = [new LaneInfo(true, false, "", "t0"), new LaneInfo(false, false, "", "t1")],
            Viewport = new Size(800, 300),
            Waveforms = cache,
            Clips = [Audio("trimmed", 1000, inS, inS + 48_000 * 5)], // 5 s: chunk 3 only (and maybe 4)
        };
        var chunks = lanes.PlanWaveforms().Select(w => w.Chunk).ToArray();
        Assert.Equal(3, chunks[0]);
        Assert.All(chunks, c => Assert.InRange(c, 3, 4));
    }
}

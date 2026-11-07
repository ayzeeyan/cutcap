using System.Diagnostics;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Phase 4B over the real ABI 9 with real FFmpeg: linked placement creates a
/// video/audio pair whose rows carry the audio stream identity; edits move
/// both members as one transaction; waveform peaks match min/max computed
/// independently from PCM decoded by a separate FFmpeg run, and analysis
/// never changes the document. The fixture audio-video.mp4 is 3 s of H.264
/// plus 3 s of mono 48 kHz AAC.
/// </summary>
[Collection("engine")]
public sealed class AudioWaveformTests
{
    private const uint Ready = 0;
    private const int BucketFrames = 256;

    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-waveforms-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
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
            Assert.True(watch.Elapsed < TimeSpan.FromSeconds(seconds), $"{view.Status}; waveforms {view.Waveforms}");
            Thread.Sleep(5);
        }
    }

    private static (string Media, string Video, string Audio) Imported(EngineSession s, string path)
    {
        InteropTests.RequireFfmpeg();
        Until(s, v => v.EngineReady && v.Pending == 0);
        s.Import(path);
        var view = Until(s, v => v.Media.Count == 1 && !v.Import.Busy && v.Frame is not null && !v.Source.Busy);
        return (view.Media[0].Id, view.Tracks.First(t => t.Kind == "video").Id, view.Tracks.First(t => t.Kind == "audio").Id);
    }

    private static ClipView[] Clips(EngineSession s, int count)
    {
        Until(s, v => v.Pending == 0 && v.Project?.ClipCount == count);
        return [.. s.ReadTimeline().Clips.OrderBy(c => c.StartMs).ThenBy(c => c.TrackIndex)];
    }

    /// <summary>Native-rate interleaved f32 PCM of one stream, decoded by a separate FFmpeg run.</summary>
    private static (float[] Samples, int Channels) IndependentPcm(string path, int stream)
    {
        var raw = Path.GetTempFileName();
        try
        {
            var info = new ProcessStartInfo("ffmpeg")
            {
                RedirectStandardError = true,
                UseShellExecute = false,
            };
            foreach (var a in new[] { "-hide_banner", "-v", "error", "-y", "-i", path, "-map", $"0:{stream}", "-f", "f32le", "-c:a", "pcm_f32le", raw })
            {
                info.ArgumentList.Add(a);
            }
            using var ffmpeg = Process.Start(info)!;
            var error = ffmpeg.StandardError.ReadToEnd();
            ffmpeg.WaitForExit();
            Assert.True(ffmpeg.ExitCode == 0, error);
            var bytes = File.ReadAllBytes(raw);
            var samples = new float[bytes.Length / 4];
            Buffer.BlockCopy(bytes, 0, samples, 0, samples.Length * 4);
            var probe = new ProcessStartInfo("ffprobe") { RedirectStandardOutput = true, UseShellExecute = false };
            foreach (var a in new[] { "-v", "error", "-select_streams", $"{stream}", "-show_entries", "stream=channels", "-of", "csv=p=0", path })
            {
                probe.ArgumentList.Add(a);
            }
            using var ffprobe = Process.Start(probe)!;
            var channels = int.Parse(ffprobe.StandardOutput.ReadToEnd().Trim().Split(',')[0]);
            ffprobe.WaitForExit();
            return (samples, channels);
        }
        finally
        {
            File.Delete(raw);
        }
    }

    [Fact]
    public void LinkedPlacementCarriesTheAudioStreamIdentityAndEditsMoveThePair()
    {
        using var session = EngineSession.Start();
        var (media, video, _) = Imported(session, Fixture("audio-video.mp4"));
        session.PlaceClip(media, video, TimelinePosition.Milliseconds(1000));
        var pair = Clips(session, 2);
        var v = Assert.Single(pair, c => !c.IsAudio);
        var a = Assert.Single(pair, c => c.IsAudio);
        Assert.Equal("video", v.Stream);
        Assert.Equal(1u, a.AudioStream);
        Assert.NotNull(v.LinkId);
        Assert.Equal(v.LinkId, a.LinkId);
        Assert.Equal((v.StartTicks, v.EndTicks), (a.StartTicks, a.EndTicks));
        Assert.Equal(48_000u, a.AudioRate);
        Assert.NotEmpty(a.AudioKey);
        Assert.Equal(0, a.SourceInSamples);
        Assert.Equal(144_000, a.SourceEndSamples); // 3 s at 48 kHz
        Assert.Equal(1, a.TrackIndex); // Audio 1

        // Move the audio member: both members move, in one undo step.
        session.MoveClip(a.Id, a.TrackId, TimelinePosition.Milliseconds(2000), ClipEdge.Start);
        var moved = Clips(session, 2);
        Assert.All(moved, c => Assert.Equal(2000, c.StartMs));
        // Trim the video member's start by 0.5 s: the audio in-point follows on the joint grid.
        session.TrimClip(moved.Single(c => !c.IsAudio).Id, ClipEdge.Start, TimelinePosition.Milliseconds(2500));
        var trimmed = Clips(session, 2);
        Assert.All(trimmed, c => Assert.Equal((2500L, 2500L), (c.StartMs, c.DurationMs)));
        Assert.Equal(24_000, trimmed.Single(c => c.IsAudio).SourceInSamples);
        // Split both at 3.5 s: two pairs with distinct links.
        session.SplitClip(trimmed.Single(c => c.IsAudio).Id, TimelinePosition.Milliseconds(3500));
        var split = Clips(session, 4);
        Assert.Equal(2, split.Select(c => c.LinkId).Distinct().Count());
        Assert.All(split.GroupBy(c => c.LinkId), g => Assert.Single(g.Select(c => (c.StartTicks, c.EndTicks)).Distinct()));
        var right = split.Where(c => c.StartMs == 3500).ToArray();
        Assert.Equal(72_000, right.Single(c => c.IsAudio).SourceInSamples);
        // Removing one member removes its partner.
        session.RemoveClip(right.Single(c => !c.IsAudio).Id);
        Assert.Equal(2, Clips(session, 2).Length);
        // Undo walks back one gesture at a time, pairs intact.
        session.Undo(); Assert.Equal(4, Clips(session, 4).Length);
        session.Undo(); Assert.Equal(2, Clips(session, 2).Length);
        session.Undo(); Assert.All(Clips(session, 2), c => Assert.Equal((2000L, 3000L), (c.StartMs, c.DurationMs)));
        session.Undo(); Assert.All(Clips(session, 2), c => Assert.Equal(1000, c.StartMs));
        session.Redo(); Assert.All(Clips(session, 2), c => Assert.Equal(2000, c.StartMs));
    }

    [Fact]
    public void RealWaveformPeaksMatchIndependentPcmAndNeverDirtyTheDocument()
    {
        using var workspace = new Workspace();
        using var session = EngineSession.Start();
        var path = Fixture("audio-video.mp4");
        var (media, video, _) = Imported(session, path);
        session.PlaceClip(media, video, TimelinePosition.Milliseconds(0));
        var audio = Clips(session, 2).Single(c => c.IsAudio);
        session.SaveProjectAs(workspace.At("waves.cutcap"));
        var saved = Until(session, v => v.Project?.Document is { Dirty: false, State: not "saving" } && v.Pending == 0);
        var revision = saved.Project!.Document!.Revision;

        session.RequestWaveforms([new WaveformWant(7, media, audio.AudioKey, audio.AudioStream!.Value, 0)]);
        NativeWaveformInfo info = default;
        float[]? peaks = null;
        var watch = Stopwatch.StartNew();
        while (peaks is null && watch.Elapsed < TimeSpan.FromSeconds(20))
        {
            session.Pump();
            if (session.TryNextWaveform(out info))
            {
                Assert.Equal(7ul, info.Token);
                Assert.Equal(Ready, info.Status);
                peaks = new float[info.Buckets * 2];
                Assert.True(session.CopyWaveform(info.Token, peaks));
                Assert.False(session.CopyWaveform(info.Token, peaks)); // released by the copy
            }
            Thread.Sleep(5);
        }
        Assert.NotNull(peaks);
        Assert.Equal((0L, 48_000u, 1u, (uint)BucketFrames), (info.Chunk, info.Rate, info.Channels, info.BucketFrames));

        var (pcm, channels) = IndependentPcm(path, 1);
        Assert.Equal(1, channels);
        var frames = pcm.Length / channels;
        var buckets = (frames + BucketFrames - 1) / BucketFrames;
        var compared = 0;
        for (var b = 0; b < info.Buckets; b++)
        {
            var (lo, hi) = (peaks![2 * b], peaks[2 * b + 1]);
            if (b >= buckets)
            {
                Assert.True(lo > hi, $"bucket {b} past the end must be empty");
                continue;
            }
            var expectedLo = float.MaxValue; var expectedHi = float.MinValue;
            for (var i = b * BucketFrames * channels; i < Math.Min(pcm.Length, (b + 1) * BucketFrames * channels); i++)
            {
                expectedLo = Math.Min(expectedLo, pcm[i]);
                expectedHi = Math.Max(expectedHi, pcm[i]);
            }
            Assert.Equal(expectedLo, lo, 4);
            Assert.Equal(expectedHi, hi, 4);
            compared++;
        }
        Assert.Equal(buckets, compared);
        Assert.Contains(peaks!, p => Math.Abs(p) > 0.05f); // real signal, not silence

        var after = Until(session, v => v.Waveforms is { Pending: 0, InFlight: 0 });
        Assert.False(after.Project!.Document!.Dirty);
        Assert.Equal(revision, after.Project.Document.Revision);
        Assert.Equal(1ul, after.Waveforms!.Analysed);
        Assert.NotNull(after.Waveforms.LastProcessId);
    }

    [Fact]
    public void TheAudioViewReportsTheOutputHonestly()
    {
        using var session = EngineSession.Start();
        var view = Until(session, v => v.EngineReady && v.Pending == 0);
        Assert.NotNull(view.Audio);
        // Either a real endpoint (any state but failed at rest) or an explicit unavailable state with a reason.
        Assert.Contains(view.Audio!.Output.State, new[] { "idle", "unavailable" });
        if (view.Audio.Output.State == "unavailable")
        {
            Assert.False(string.IsNullOrWhiteSpace(view.Audio.Detail));
        }
        else
        {
            Assert.False(string.IsNullOrWhiteSpace(view.Audio.Output.Device));
            Assert.InRange(view.Audio.Output.Rate, 8_000u, 384_000u);
        }
        Assert.Equal(0ul, view.Audio.Output.Underruns);
    }
}

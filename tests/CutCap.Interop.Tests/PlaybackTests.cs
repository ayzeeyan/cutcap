using System.Diagnostics;
using CutCap.Interop;
using Xunit;

namespace CutCap.Interop.Tests;

[CollectionDefinition("engine", DisableParallelization = true)]
public sealed class EngineCollection;

[Collection("engine")]
public sealed class PlaybackTests
{
    private static string Fixture(string file) => Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../../tests/fixtures", file));
    private static SessionView Until(EngineSession session, Func<SessionView, bool> done)
    {
        var deadline = Stopwatch.StartNew();
        while (deadline.Elapsed < TimeSpan.FromSeconds(15))
        {
            session.Pump(); var view = session.ReadView();
            if (done(view)) return view;
            Thread.Sleep(5);
        }
        throw new TimeoutException("Playback did not reach its expected state.");
    }
    private static void Copy(EngineSession session)
    {
        if (session.TryGetFrameInfo(out var frame))
        {
            var bytes = new byte[checked((int)(frame.Stride * frame.Height))];
            Assert.Equal(FrameCopyResult.Copied, session.CopyFrame(frame.Generation, bytes, (int)frame.Stride));
        }
    }
    [Fact]
    public void PlaybackWithoutSourceIsRejectedAndPauseStopAreIdempotent()
    {
        using var session = EngineSession.Start(); Until(session, v => v.EngineReady);
        Assert.Equal(NativeStatus.InvalidArgument, Assert.Throws<EngineException>(session.Play).Status);
        Assert.Equal(NativeStatus.InvalidArgument, Assert.Throws<EngineException>(() => session.Seek(0)).Status);
        Assert.Equal(NativeStatus.InvalidArgument, Assert.Throws<EngineException>(() => session.Seek(-1)).Status);
        session.Pause(); session.Stop(); Assert.Equal(PlaybackState.Stopped, session.ReadView().Playback!.State);
    }
    [Fact]
    public void RealVfrPlaybackPauseResumeSeekEndAndStop()
    {
        InteropTests.RequireFfmpeg();
        using var session = EngineSession.Start(); Until(session, v => v.EngineReady); session.Import(Fixture("colors-vfr.mkv"));
        Until(session, v => v.Frame is not null && !v.Source.Busy); Copy(session);
        session.Play();
        var first = Until(session, v => v.Playback!.Offered > 0); Copy(session);
        Assert.Equal(0, first.Playback!.OfferedPts!.Ticks);
        var epoch = first.Playback.Epoch;
        session.Pause();
        var paused = session.ReadView(); var generation = paused.Frame!.Generation;
        Thread.Sleep(100); session.Pump(); var later = session.ReadView();
        Assert.Equal(PlaybackState.Paused, later.Playback!.State); Assert.Equal(generation, later.Frame!.Generation);
        Assert.Equal(paused.Playback!.ClockMs, later.Playback.ClockMs);
        session.Resume(); Assert.Equal(epoch, session.ReadView().Playback!.Epoch);
        session.Seek(201); var sought = Until(session, v => v.Playback!.OfferedPts?.Ticks == 700);
        Assert.NotEqual(epoch, sought.Playback!.Epoch); Copy(session);
        var copied = session.ReadView(); Assert.Equal(700ul, copied.Playback!.CopiedMs);
        var pixels = new byte[checked((int)(sought.Frame!.Width * sought.Frame.Height * 4))];
        Assert.Equal(FrameCopyResult.Copied, session.CopyFrame(sought.Frame.Generation, pixels, (int)sought.Frame.Width * 4));
        Assert.True(pixels[2] > 250 && pixels[0] < 5 && pixels[1] < 5);
        Until(session, v => { Copy(session); return v.Playback!.State == PlaybackState.Ended; });
        Assert.Equal(2300ul, session.ReadView().Playback!.CopiedMs);
        session.Stop(); Until(session, v => v.Frame?.Generation != sought.Frame.Generation && !v.Source.Busy);
        Assert.Equal(PlaybackState.Stopped, session.ReadView().Playback!.State);
    }

    [Fact]
    public void RapidPlayingSeeksPausedSeekReplacementAndShutdown()
    {
        InteropTests.RequireFfmpeg();
        using var session = EngineSession.Start(); Until(session, v => v.EngineReady); session.Import(Fixture("colors-cfr.mkv"));
        Until(session, v => v.Frame is not null && !v.Source.Busy); Copy(session); session.Play();
        var first = Until(session, v => v.Playback!.Offered > 0); Copy(session);
        var epoch = first.Playback!.Epoch;
        for (var iteration = 0; iteration < 8; iteration++) { session.Pause(); session.Pump(); session.Resume(); Assert.Equal(epoch, session.ReadView().Playback!.Epoch); }
        foreach (var ms in new long[] { 2600, 750, 2000, 1000 }) session.Seek(ms);
        var sought = Until(session, v => v.Playback!.RequestedMs == 1000 && v.Playback.OfferedPts?.Ticks == 1000); Copy(session);
        Assert.NotEqual(epoch, sought.Playback!.Epoch);
        session.Pause(); session.Seek(1250); var paused = Until(session, v => v.Playback!.State == PlaybackState.Paused && v.Playback.OfferedPts?.Ticks == 1250); Copy(session);
        Assert.False(paused.Playback!.DesiredPlaying);
        var oldGeneration = paused.Frame!.Generation;
        session.Resume(); session.Import(Fixture("audio-video.mp4"));
        var replacement = Until(session, v => v.Source.Title == "audio-video.mp4" && v.Frame is not null && !v.Source.Busy);
        Assert.Equal(PlaybackState.Stopped, replacement.Playback!.State);
        Assert.Equal(FrameCopyResult.Stale, session.CopyFrame(oldGeneration, new byte[320 * 180 * 4], 320 * 4));
        session.Play(); Until(session, v => v.Playback!.Offered > 0); session.Pause();
        var watch = Stopwatch.StartNew(); session.Dispose(); Assert.True(watch.Elapsed < TimeSpan.FromSeconds(3));
    }

    [Fact]
    public void MissingSourceDuringPlaybackReportsFailureWithoutCrashing()
    {
        InteropTests.RequireFfmpeg();
        var path = Path.Combine(Path.GetTempPath(), $"cutcap-playback-{Guid.NewGuid():N}.mkv");
        File.Copy(Fixture("colors-cfr.mkv"), path);
        try
        {
            using var session = EngineSession.Start(); Until(session, v => v.EngineReady); session.Import(path);
            Until(session, v => v.Frame is not null && !v.Source.Busy); Copy(session); File.Delete(path);
            session.Play(); var failed = Until(session, v => v.Playback!.State == PlaybackState.Failed);
            Assert.NotEmpty(failed.Playback!.Error!); Assert.True(failed.Source.HasFrame);
            Assert.False(failed.Playback.DesiredPlaying);
        }
        finally { File.Delete(path); }
    }

    [Fact]
    public void DisposeDuringContinuousSeekReapsOwnedDecoder()
    {
        InteropTests.RequireFfmpeg();
        using var session = EngineSession.Start(); Until(session, v => v.EngineReady);
        session.Import(Fixture("long-gop-bframes.mp4")); Until(session, v => v.Frame is not null && !v.Source.Busy);
        session.Play(); var playing = Until(session, v => v.Playback!.Offered > 0); Copy(session);
        var pid = playing.Playback!.ProcessId;
        session.Seek(2500);
        var watch = Stopwatch.StartNew(); session.Dispose();
        Assert.True(watch.Elapsed < TimeSpan.FromSeconds(3), $"Dispose took {watch.Elapsed}");
        if (pid is > 0)
        {
            try { using var child = Process.GetProcessById((int)pid.Value); Assert.True(child.HasExited); }
            catch (ArgumentException) { /* Already reaped. */ }
        }
    }
}

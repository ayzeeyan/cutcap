using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Playback state → controls, readouts and seek decisions, without a running engine.</summary>
public sealed class TransportTests
{
    private static SourceView Source(bool ready = true, long position = 0, bool busy = false, string title = "a.mkv") =>
        new(ready, title, position, 3000, false, busy, true, "", "", "");

    private static PlaybackView Playback(PlaybackState state, bool desired, long requested = 0, ulong? copied = null) =>
        new(state, desired, "epoch-1", requested, 0, copied, copied, null, null, 0, 0, 0, 0, 0, 0, null,
            state == PlaybackState.Failed ? "decoder exited" : null, "", "", "", null);

    [Fact]
    public void NoSourceOrEngineMeansNothingToPlay()
    {
        var none = Transport.Map(Source(ready: false), null, engineReady: true);
        Assert.False(none.CanPlay);
        Assert.False(none.CanStop);
        Assert.Equal("Play", none.PlayName);
        Assert.False(Transport.Map(Source(), null, engineReady: false).CanPlay);
    }

    [Theory]
    [InlineData(PlaybackState.Stopped, false, false, null)]
    [InlineData(PlaybackState.Starting, true, true, "Starting…")]
    [InlineData(PlaybackState.Playing, true, true, null)]
    [InlineData(PlaybackState.Seeking, true, true, "Seeking…")]
    [InlineData(PlaybackState.Seeking, false, false, "Seeking…")]
    [InlineData(PlaybackState.Buffering, true, true, "Buffering…")]
    [InlineData(PlaybackState.Paused, false, false, null)]
    [InlineData(PlaybackState.Ended, false, false, "End of source")]
    [InlineData(PlaybackState.Failed, false, false, "Playback failed")]
    public void PlayPauseFollowsEngineStateAndIntent(PlaybackState state, bool desired, bool showPause, string? chip)
    {
        var transport = Transport.Map(Source(), Playback(state, desired), engineReady: true);
        Assert.Equal(showPause, transport.ShowPause);
        Assert.Equal(showPause ? "Pause" : "Play", transport.PlayName);
        Assert.Equal(chip, transport.Chip);
        Assert.True(transport.CanPlay);
        Assert.Equal(state != PlaybackState.Stopped, transport.CanStop);
    }

    [Fact]
    public void EndedOffersReplayAndFailedCarriesTheError()
    {
        var ended = Transport.Map(Source(), Playback(PlaybackState.Ended, false, copied: 2900), true);
        Assert.Contains("again", ended.PlayHint);
        Assert.Equal(2900, ended.DisplayMs);
        var failed = Transport.Map(Source(), Playback(PlaybackState.Failed, false), true);
        Assert.Equal("decoder exited", failed.Error);
        Assert.True(failed.CanPlay);
    }

    [Fact]
    public void ReadoutIsTheCopiedFrameNotTheRequestOrAClock()
    {
        // Playing: requested stays at the epoch start; copied advances with delivered frames.
        var playing = Transport.Map(Source(position: 0), Playback(PlaybackState.Playing, true, requested: 0, copied: 1733), true);
        Assert.Equal(1733, playing.DisplayMs);
        Assert.True(playing.DisplayIsFrame);
        Assert.Equal(1733, playing.SliderMs);
        // A new epoch has not delivered yet: show the request, labelled as such.
        var seeking = Transport.Map(Source(), Playback(PlaybackState.Seeking, true, requested: 2500), true);
        Assert.Equal(2500, seeking.DisplayMs);
        Assert.False(seeking.DisplayIsFrame);
        Assert.Equal(2500, seeking.SliderMs);
        // Stopped: Phase 1 meaning of source.position_ms (requested).
        var stopped = Transport.Map(Source(position: 1500, busy: true), Playback(PlaybackState.Stopped, false, copied: 900), true);
        Assert.Equal(1500, stopped.DisplayMs);
        Assert.False(stopped.DisplayIsFrame);
        Assert.True(stopped.ShowIsolatedBusy);
    }

    [Fact]
    public void BusySourceDuringPlaybackIsNotTheIsolatedDecodeBanner()
    {
        var buffering = Transport.Map(Source(busy: true), Playback(PlaybackState.Buffering, true, copied: 400), true);
        Assert.False(buffering.ShowIsolatedBusy);
        Assert.True(buffering.ChipBusy);
    }

    [Fact]
    public void KeyboardScrubSeeksImmediately()
    {
        var seeks = new List<long>();
        var scrub = new ScrubController(() => 0, seeks.Add);
        scrub.Move(100);
        scrub.Move(200);
        Assert.Equal([100, 200], seeks);
    }

    [Fact]
    public void DragSeeksAreThrottledLatestWinsAndFinalOnRelease()
    {
        long now = 0;
        var seeks = new List<long>();
        var scrub = new ScrubController(() => now, seeks.Add);
        scrub.Begin();
        scrub.Move(100);          // first move seeks at once
        now = 30; scrub.Move(150);
        now = 60; scrub.Move(180); // within the interval: held
        Assert.Equal([100], seeks);
        now = 130; scrub.Move(220); // interval elapsed: latest wins
        Assert.Equal([100, 220], seeks);
        now = 140; scrub.Move(260);
        scrub.End();
        Assert.Equal([100, 220, 260], seeks);
        Assert.False(scrub.IsScrubbing);
    }

    [Fact]
    public void ReleaseAtTheLastIssuedTargetDoesNotSeekAgainAndCancelDropsTheGesture()
    {
        long now = 0;
        var seeks = new List<long>();
        var scrub = new ScrubController(() => now, seeks.Add);
        scrub.Begin();
        scrub.Move(500);
        now = 10; scrub.Move(500);
        scrub.End();
        Assert.Equal([500], seeks);
        scrub.Begin();
        now = 500; scrub.Move(700);
        now = 510; scrub.Move(900);
        scrub.Cancel();
        scrub.End();
        Assert.Equal([500, 700], seeks);
    }

    private static SessionView View(PlaybackView? playback = null, IReadOnlyList<MediaView>? media = null, string inspectorTitle = "a.mkv") =>
        new(1, true, new ProjectView("p", "Untitled", false, 0), new StatusView("Ready", false), 0,
            [new TrackView("t1", "Video 1", "video", "V1", false)],
            media ?? [new MediaView("m1", "a.mkv", "320 × 180", "FFV1", "3.0 s", 3000, true, false, true)],
            new ImportView(false, false, "Imported a.mkv."),
            new InspectorView("source", inspectorTitle, "Source video", [new SectionView("Video", [new RowView("Codec", "FFV1")])]),
            Source(), null, new PreviewMetrics(null, null, null, 0), playback);

    [Fact]
    public void PlaybackTicksUpdateOnlyThePlayerNotLibraryInspectorOrTimeline()
    {
        var first = View(Playback(PlaybackState.Playing, true, copied: 100));
        var tick = View(Playback(PlaybackState.Playing, true, copied: 133));
        Assert.Equal(new ViewChanges(false, false, false), ViewChanges.Between(first, tick));
        Assert.Equal(ViewChanges.All, ViewChanges.Between(null, first));
        var imported = View(media: [new MediaView("m1", "a.mkv", "320 × 180", "FFV1", "3.0 s", 3000, true, false, true),
            new MediaView("m2", "b.mkv", "320 × 180", "FFV1", "2.0 s", 2000, true, false, false)]);
        Assert.True(ViewChanges.Between(first, imported).Library);
        Assert.True(ViewChanges.Between(first, View(inspectorTitle: "b.mkv")).Inspector);
    }

    [Fact]
    public void PlayerTracksEngineStateAndKeepsTheGoToFieldOnTheRequest()
    {
        var player = new PlayerViewModel(new EngineHost());
        player.Apply(Source(position: 0), Playback(PlaybackState.Playing, true, requested: 0, copied: 500), true, true);
        Assert.True(player.ShowPause);
        Assert.Equal("00:00.500", player.PositionText);
        Assert.Equal("Shown frame source time", player.PositionName);
        Assert.Equal("00:00.000", player.GoToText);
        player.GoToText = "1.5s"; // the user is typing
        player.Apply(Source(position: 0), Playback(PlaybackState.Playing, true, requested: 0, copied: 533), true, true);
        Assert.Equal("1.5s", player.GoToText);
        player.Apply(Source(position: 0), Playback(PlaybackState.Paused, false, requested: 0, copied: 533), true, true);
        Assert.False(player.ShowPause);
        Assert.True(player.CanStop);
    }

    [Fact]
    public void ScrubberIsNotOverwrittenMidDragAndSourceSwitchEndsTheDrag()
    {
        var player = new PlayerViewModel(new EngineHost());
        player.Apply(Source(), Playback(PlaybackState.Playing, true, copied: 500), true, true);
        player.BeginScrub();
        player.Apply(Source(), Playback(PlaybackState.Playing, true, copied: 800), true, true);
        Assert.Equal(500, player.ScrubValue);
        Assert.True(player.IsScrubbing);
        player.Apply(Source(title: "b.mkv"), null, true, true);
        Assert.False(player.IsScrubbing);
        Assert.Equal(0, player.ScrubValue);
    }
}

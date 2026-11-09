using CutCap.Desktop.Controls;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Phase 4C track-control presentation rules, without a running engine.</summary>
public sealed class MixerControlTests
{
    private static TrackView Audio(int gain = 0, bool muted = false, bool soloed = false, bool audible = true, string text = "0.0 dB") =>
        new("t1", "Audio 1", "audio", "A1", false, "", gain, text, muted, soloed, audible);

    [Fact]
    public void TheSliderShowsTheEngineValueUntilADragAndAgainAfterIt()
    {
        var track = new TrackViewModel(Audio(-600, text: "−6.0 dB"));
        Assert.Equal(-6.0, track.GainDb);
        Assert.Equal("−6.0 dB", track.GainText);
        // A drag snaps to half decibels and clamps to the supported range.
        track.GainDb = -12.3;
        Assert.Equal(-12.5, track.GainDb);
        track.GainDb = 99;
        Assert.Equal(6, track.GainDb);
        // Engine updates during a drag do not fight the pointer...
        track.Track = Audio(-100);
        Assert.Equal(6, track.GainDb);
        // ...and the engine's value returns when the gesture ends.
        track.CommitGain();
        Assert.Equal(-1.0, track.GainDb);
    }

    [Fact]
    public void CommitAndCancelOnlyActDuringAGesture()
    {
        var track = new TrackViewModel(Audio());
        track.CommitGain();
        Assert.False(track.CancelGain());
        track.GainDb = -3;
        Assert.True(track.CancelGain());
        Assert.Equal(0, track.GainDb);
        Assert.False(track.CancelGain());
    }

    [Fact]
    public void AControlsNamesAndTipsFollowTheEngineState()
    {
        var track = new TrackViewModel(Audio());
        Assert.Equal("Mute Audio 1", track.MuteName);
        Assert.Equal("Solo Audio 1", track.SoloName);
        track.Track = Audio(muted: true, soloed: true, audible: false);
        Assert.Equal("Unmute Audio 1", track.MuteName);
        Assert.Equal("Remove solo from Audio 1", track.SoloName);
        Assert.Contains("Mute still wins", track.SoloTip);
        Assert.False(track.Audible);
    }

    [Fact]
    public void AddToTimelineTargetsAudioTracksForAudioOnlySources()
    {
        TrackViewModel Track(string id, string kind, bool selected = false) => new(new TrackView(id, id, kind, "", selected));
        var tracks = new[] { Track("t0", "video"), Track("t1", "audio"), Track("t2", "audio", true) };
        Assert.Equal("t2", TimelineViewModel.ChooseTarget(tracks, null, audioOnly: true)!.Id);
        Assert.Equal("t0", TimelineViewModel.ChooseTarget(tracks, null)!.Id);
        Assert.Equal("t1", TimelineViewModel.ChooseTarget([tracks[0], tracks[1]], null, audioOnly: true)!.Id);
        Assert.Null(TimelineViewModel.ChooseTarget([tracks[0]], null, audioOnly: true));
    }

    [Fact]
    public void AudioOnlySourcesDropOnAudioLanesOnly()
    {
        LaneInfo[] lanes = [new(true, false, "", "v"), new(false, false, "", "a")];
        var clips = new TimelineGeometry([], 2);
        var snapper = new TimelineSnapper([]);
        var song = new MediaDrag("m", "song.mp3", 3000, true, AudioOnly: true);
        var onAudio = TimelineGestures.Place(clips, lanes, song, 1000, 1, 50, snapper, null, false);
        Assert.True(onAudio.Valid);
        Assert.Equal("a", onAudio.TrackId);
        var onVideo = TimelineGestures.Place(clips, lanes, song, 1000, 0, 50, snapper, null, false);
        Assert.False(onVideo.Valid);
        Assert.Contains("audio track", onVideo.Reason);
        var film = new MediaDrag("f", "film.mp4", 3000, true);
        Assert.False(TimelineGestures.Place(clips, lanes, film, 1000, 1, 50, snapper, null, false).Valid);
    }

    private static SessionView View(IReadOnlyList<MediaView> media) => new(
        1, true, null, new StatusView("", false), 0, [], media, new ImportView(false, false, ""),
        new InspectorView("project", "", "", []), new SourceView(false, "", 0, 0, false, false, false, "", "", ""),
        null, new PreviewMetrics(null, null, null, 0));

    private static MediaView Media(params AudioStreamView[] streams) =>
        new("m", "a", "", "", "", null, false, true, false, AudioStreams: streams);

    [Fact]
    public void FreshStreamListsWithEqualValuesDoNotReapplyTheLibraryEveryFrame()
    {
        var same = ViewChanges.Between(View([Media(new AudioStreamView(1, "x", true))]), View([Media(new AudioStreamView(1, "x", true))]));
        Assert.False(same.Library);
        Assert.False(same.Timeline);
        var different = ViewChanges.Between(View([Media(new AudioStreamView(1, "x", true))]), View([Media(new AudioStreamView(2, "x", true))]));
        Assert.True(different.Library);
    }
}

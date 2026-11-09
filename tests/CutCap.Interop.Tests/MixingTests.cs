using System.Diagnostics;
using System.Text.Json.Nodes;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Phase 4C over the real ABI 10 with real FFmpeg: audio-only sources, chosen
/// streams, Unlink/Link, track controls, undo and persistence. Files are
/// generated per test; nothing depends on the audio device.
/// </summary>
[Collection("engine")]
public sealed class MixingTests
{
    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-mixing-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }

    private static string Ffmpeg(params string[] arguments)
    {
        var info = new ProcessStartInfo("ffmpeg") { RedirectStandardError = true, UseShellExecute = false };
        foreach (var a in new[] { "-hide_banner", "-v", "error", "-y" }.Concat(arguments)) info.ArgumentList.Add(a);
        using var process = Process.Start(info)!;
        var error = process.StandardError.ReadToEnd();
        process.WaitForExit();
        Assert.True(process.ExitCode == 0, error);
        return arguments[^1];
    }

    private static string Tone(Workspace files, string name, int frequency, string[] encoder, double seconds = 2) =>
        Ffmpeg([.. new[] { "-f", "lavfi", "-i", $"sine=frequency={frequency}:sample_rate=48000:duration={seconds}" }, .. encoder, files.At(name)]);

    /// <summary>Video plus two audio streams: primary 440 Hz, alternate 1320 Hz.</summary>
    private static string TwoStreams(Workspace files, string name) =>
        Ffmpeg("-f", "lavfi", "-i", "color=c=gray:s=160x90:r=25:d=2",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2",
            "-f", "lavfi", "-i", "sine=frequency=1320:sample_rate=48000:duration=2",
            "-map", "0:v", "-map", "1:a", "-map", "2:a", "-c:v", "ffv1", "-c:a", "pcm_s16le", files.At(name));

    private static SessionView Until(EngineSession s, Func<SessionView, bool> done, int seconds = 25)
    {
        var watch = Stopwatch.StartNew();
        while (true)
        {
            s.Pump();
            var view = s.ReadView();
            if (done(view)) return view;
            Assert.True(watch.Elapsed < TimeSpan.FromSeconds(seconds), view.Status.Text);
            Thread.Sleep(5);
        }
    }

    private static SessionView Import(EngineSession s, string path, int count = 1)
    {
        s.Import(path);
        return Until(s, v => v.Media.Count == count && !v.Import.Busy && v.Pending == 0);
    }

    private static ClipView[] Clips(EngineSession s, int count)
    {
        Until(s, v => v.Pending == 0 && v.Project?.ClipCount == count);
        return [.. s.ReadTimeline().Clips.OrderBy(c => c.TrackIndex).ThenBy(c => c.StartMs)];
    }

    private static EngineSession Ready()
    {
        InteropTests.RequireFfmpeg();
        var session = EngineSession.Start();
        Until(session, v => v.EngineReady && v.Pending == 0);
        return session;
    }

    private static string AudioTrack(SessionView v, int n = 0) => v.Tracks.Where(t => !t.IsVideo).ElementAt(n).Id;

    [Fact]
    public void AnAudioOnlyFileIsImportedHonestlyAndPlacedOnAnAudioTrackWithARealWaveform()
    {
        using var files = new Workspace();
        using var s = Ready();
        var view = Import(s, Tone(files, "tone.wav", 880, ["-c:a", "pcm_s16le"]));
        var media = view.Media[0];
        Assert.True(media.AudioOnly);
        Assert.True(media.HasAudio);
        Assert.Equal("Audio only", media.Resolution);
        var stream = Assert.Single(media.AudioStreams!);
        Assert.True(stream.Primary);
        Assert.Equal("Audio", view.Inspector.Badge);
        // No picture to preview, and honest about it.
        Assert.False(view.Source.Ready);
        Assert.Contains("Audio-only", view.Source.Status);
        Assert.Null(view.Frame);

        s.PlaceClip(media.Id, view.Tracks.First(t => t.IsVideo).Id, TimelinePosition.Milliseconds(0));
        Until(s, v => v.Pending == 0 && v.Status.Error);
        Assert.Equal(0, s.ReadView().Project!.ClipCount);

        s.PlaceClip(media.Id, AudioTrack(view), TimelinePosition.Milliseconds(500));
        var clip = Assert.Single(Clips(s, 1));
        Assert.True(clip.IsAudio);
        Assert.Null(clip.LinkId);
        Assert.Equal(500, clip.StartMs);
        Assert.Equal(2000, clip.DurationMs);
        Assert.Equal(96_000, clip.SourceEndSamples);
        Assert.Equal(48_000u, clip.AudioRate);
        Assert.Empty(clip.SourceKey); // no filmstrip for a clip with no picture

        s.RequestWaveforms([new WaveformWant(1, media.Id, clip.AudioKey, clip.AudioStream!.Value, 0)]);
        var watch = Stopwatch.StartNew();
        NativeWaveformInfo info = default;
        while (!s.TryNextWaveform(out info))
        {
            Assert.True(watch.Elapsed < TimeSpan.FromSeconds(20));
            s.Pump();
            Thread.Sleep(5);
        }
        Assert.Equal(0u, info.Status);
        var peaks = new float[info.Buckets * 2];
        Assert.True(s.CopyWaveform(info.Token, peaks));
        Assert.Contains(peaks, p => Math.Abs(p) > 0.05f);

        s.Undo();
        Assert.Empty(Clips(s, 0));
    }

    [Fact]
    public void AChosenStreamKeepsItsIdentityAndItsOwnWaveformKey()
    {
        using var files = new Workspace();
        using var s = Ready();
        var view = Import(s, TwoStreams(files, "two.mkv"));
        var media = view.Media[0];
        Assert.False(media.AudioOnly);
        Assert.Equal(2, media.AudioStreams!.Count);
        var (first, second) = (media.AudioStreams[0], media.AudioStreams[1]);
        s.PlaceAudioStream(media.Id, second.Index, null, TimelinePosition.Milliseconds(0));
        var alone = Assert.Single(Clips(s, 1));
        Assert.Equal(second.Index, alone.AudioStream);
        Assert.Null(alone.LinkId);
        s.AddTrack(TrackKind.Audio);
        Until(s, v => v.Tracks.Count == 3 && v.Pending == 0);
        s.PlaceAudioStream(media.Id, first.Index, null, TimelinePosition.Milliseconds(0));
        var both = Clips(s, 2);
        Assert.NotEqual(both[0].TrackId, both[1].TrackId);
        Assert.NotEqual(both[0].AudioKey, both[1].AudioKey);
    }

    [Fact]
    public void UnlinkFreesTheMembersAndUndoRestoresThePair()
    {
        using var files = new Workspace();
        using var s = Ready();
        var view = Import(s, TwoStreams(files, "two.mkv"));
        s.PlaceClip(view.Media[0].Id, view.Tracks[0].Id, TimelinePosition.Milliseconds(0));
        var pair = Clips(s, 2);
        var (video, audio) = (pair.Single(c => !c.IsAudio), pair.Single(c => c.IsAudio));
        Assert.NotNull(audio.LinkId);
        s.SelectClip(audio.Id);
        Assert.True(Until(s, v => v.Editing!.CanUnlink && v.Pending == 0).Editing!.CanUnlink);
        s.UnlinkClip(audio.Id);
        var free = Clips(s, 2);
        Assert.All(free, c => Assert.Null(c.LinkId));
        Assert.Equal(video.Id, Until(s, v => v.Editing!.LinkCandidate is not null).Editing!.LinkCandidate);
        s.MoveClip(audio.Id, audio.TrackId, TimelinePosition.Milliseconds(1000), ClipEdge.Start);
        Until(s, v => v.Pending == 0);
        var moved = s.ReadTimeline().Clips;
        Assert.Equal(1000, moved.Single(c => c.IsAudio).StartMs);
        Assert.Equal(0, moved.Single(c => !c.IsAudio).StartMs);
        s.Undo();
        Until(s, v => v.Pending == 0);
        Assert.All(s.ReadTimeline().Clips, c => Assert.Equal(0, c.StartMs));
        s.Undo();
        Until(s, v => v.Pending == 0);
        var restored = s.ReadTimeline().Clips;
        Assert.All(restored, c => Assert.Equal(audio.LinkId, c.LinkId));
    }

    [Fact]
    public void TrackControlsAreEngineStateUndoableAndSurviveSaveAndOpen()
    {
        using var files = new Workspace();
        using var s = Ready();
        var view = Until(s, v => v.Tracks.Count == 2);
        var track = AudioTrack(view);
        var depth = view.History!.Depth;
        Assert.Throws<EngineException>(() => s.PreviewTrackGain(track, 601));
        Assert.Throws<EngineException>(() => s.SetTrackMute("not-a-track", true));

        foreach (var cdb in new[] { -100, -300, -725 }) s.PreviewTrackGain(track, cdb);
        s.Pump();
        var drag = s.ReadView();
        Assert.Equal(-725, drag.Tracks.Single(t => t.Id == track).GainCdb);
        Assert.Equal(depth, drag.History!.Depth);
        s.CommitTrackGain(track, -725);
        var committed = Until(s, v => v.Pending == 0 && v.History!.Depth == depth + 1);
        Assert.True(committed.History!.UndoTrack);
        Assert.Equal("−7.2 dB".Length, committed.Tracks.Single(t => t.Id == track).GainText.Length);

        s.SetTrackMute(track, true);
        s.SetTrackSolo(track, true);
        var set = Until(s, v => v.Pending == 0 && v.History!.Depth == depth + 3);
        var row = set.Tracks.Single(t => t.Id == track);
        Assert.True(row is { Muted: true, Soloed: true, Audible: false });
        s.Undo();
        Assert.False(Until(s, v => v.Pending == 0 && v.History!.Depth == depth + 2).Tracks.Single(t => t.Id == track).Soloed);
        s.Redo();
        Until(s, v => v.Pending == 0 && v.History!.Depth == depth + 3);

        var path = files.At("mix.cutcap");
        s.SaveProjectAs(path);
        Until(s, v => v.Project!.Document!.State == "saved" && v.Pending == 0);
        var saved = JsonNode.Parse(File.ReadAllText(path))!["project"]!;
        Assert.Equal(3, saved["schema_version"]!.GetValue<int>());
        var controls = saved["tracks"]!.AsArray().Single(t => t!["kind"]!.GetValue<string>() == "Audio")!["controls"]!;
        Assert.Equal(-725, controls["gain_cdb"]!.GetValue<int>());
        Assert.True(controls["mute"]!.GetValue<bool>());
        Assert.True(controls["solo"]!.GetValue<bool>());

        using var reopened = Ready();
        reopened.OpenProject(path);
        var opened = Until(reopened, v => v.Project!.Document!.State == "opened" && v.Pending == 0);
        var back = opened.Tracks.Single(t => !t.IsVideo);
        Assert.Equal((-725, true, true), (back.GainCdb, back.Muted, back.Soloed));
        Assert.False(opened.Project!.Document!.Dirty);
    }

    [Fact]
    public void TheMixViewIsPublishedWithTheAudioView()
    {
        using var s = Ready();
        var view = Until(s, v => v.Audio is not null);
        Assert.NotNull(view.Audio);
        Assert.Contains(view.Audio!.Output.State, new[] { "idle", "unavailable" });
    }
}

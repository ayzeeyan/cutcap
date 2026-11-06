using System.Diagnostics;
using System.Text.Json.Nodes;
using Xunit;

namespace CutCap.Interop.Tests;

[Collection("engine")]
public sealed class TimelinePlaybackTests
{
    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-timeline-play-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }
    private static string Fixture(string name) => Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../../tests/fixtures", name));
    private static SessionView Until(EngineSession s, Func<SessionView, bool> done, Action<SessionView>? observe = null)
    {
        var wait = Stopwatch.StartNew();
        while (true)
        {
            s.Pump(); var v = s.ReadView(); observe?.Invoke(v);
            if (done(v)) return v;
            Assert.True(wait.Elapsed < TimeSpan.FromSeconds(25), $"{v.Status} / {v.TimelinePlayback} / {v.TimelinePreview}");
            Thread.Sleep(5);
        }
    }
    private static byte[] Copy(EngineSession s)
    {
        Assert.True(s.TryGetFrameInfo(out var info));
        var bytes = new byte[checked((int)(info.Stride * info.Height))];
        Assert.Equal(FrameCopyResult.Copied, s.CopyFrame(info.Generation, bytes, checked((int)info.Stride)));
        return bytes;
    }
    private static (string A, string B, string Track) Prepare(EngineSession s)
    {
        InteropTests.RequireFfmpeg(); Until(s, v => v.EngineReady);
        s.Import(Fixture("colors-cfr.mkv")); var a = Until(s, v => v.Media.Count == 1 && !v.Import.Busy && v.Frame is not null);
        var first = a.Media[0].Id; var track = a.Tracks[0].Id;
        s.InsertClip(first, track); Until(s, v => v.Project!.ClipCount == 1 && v.Pending == 0);
        s.Import(Fixture("offset.mkv")); var b = Until(s, v => v.Media.Count == 2 && !v.Import.Busy && v.Frame is not null);
        var second = b.Media[1].Id;
        s.InsertClip(second, track); Until(s, v => v.Project!.ClipCount == 2 && v.Pending == 0);
        s.SetPreviewMode(PreviewMode.Timeline);
        Until(s, v => v.TimelinePreview is { State: "ready", PositionMs: 0 });
        return (first, second, track);
    }
    private static void Playing(EngineSession s) => Until(s, v => v.TimelinePlayback?.State == PlaybackState.Playing);

    [Fact]
    public void RealContinuousCutsPauseSeekStopReplayAndSourceIndependence()
    {
        using var files = new Workspace(); using var s = EngineSession.Start();
        var (a, b, _) = Prepare(s);
        s.SaveProjectAs(files.At("two.cutcap")); Until(s, v => v.Project!.Document!.State == "saved" && v.Pending == 0);
        s.SeekTimelineMilliseconds(2500); Until(s, v => v.TimelinePreview is { State: "ready", PositionMs: 2500 });
        var sourcePosition = s.ReadView().Source.PositionMs; var documentGeneration = s.ReadView().TimelineGeneration;
        s.PlayTimeline(); Playing(s);
        var generations = new HashSet<ulong>(); var sawA = false; var sawB = false;
        var crossed = Until(s, v => v.TimelinePlayback!.ClockMs >= 3500, v =>
        {
            Assert.NotEqual(PlaybackState.Failed, v.TimelinePlayback!.State);
            if (v.Frame is not { } f) return;
            if (generations.Add(f.Generation)) Copy(s);
            if (v.TimelinePlayback.ClockMs < 3000) { sawA = true; Assert.Equal(a, v.TimelinePreview!.MediaId); Assert.InRange(f.Actual!.Ticks, 2500, 2999); }
            else { sawB = true; Assert.Equal(b, v.TimelinePreview!.MediaId); Assert.InRange(f.Actual!.Ticks, 5000, 7999); }
        });
        Assert.True(sawA && sawB); Assert.True(generations.Count >= 4); Assert.Equal(1UL, crossed.TimelinePlayback!.Handoffs);
        Assert.True(crossed.TimelinePlayback.DecoderStarts <= 2); Assert.Equal(sourcePosition, crossed.Source.PositionMs);
        Assert.Equal(documentGeneration, crossed.TimelineGeneration); Assert.False(crossed.Project!.Document!.Dirty);
        s.PauseTimeline(); var paused = s.ReadView(); var position = paused.TimelinePlayback!.ClockTicks;
        for (var i = 0; i < 5; i++) { Thread.Sleep(10); s.Pump(); Assert.Equal(position, s.ReadView().TimelinePlayback!.ClockTicks); }
        s.SeekTimelineMilliseconds(500); var sought = Until(s, v => v.TimelinePlayback is { State: PlaybackState.Paused, ClockMs: 500 } && v.Frame is not null);
        Assert.Equal(a, sought.TimelinePreview!.MediaId); Assert.Equal(500, sought.Frame!.Actual!.Ticks);
        s.ResumeTimeline(); Playing(s);
        s.SeekTimelineMilliseconds(3250); Until(s, v => v.TimelinePlayback is { State: PlaybackState.Playing } t && t.ClockMs >= 3250 && v.Frame is not null);
        Assert.Equal(b, s.ReadView().TimelinePreview!.MediaId);
        foreach (var target in new long[] { 100, 3100, 200, 3200, 750 }) { s.SeekTimelineMilliseconds(target); s.Pump(); }
        var latest = Until(s, v => v.TimelinePlayback is { State: PlaybackState.Playing } t && t.ClockMs >= 750 && t.ClockMs < 1000 && v.Frame is not null);
        Assert.Equal(a, latest.TimelinePreview!.MediaId); Assert.InRange(latest.Frame!.Actual!.Ticks, 750, 999);
        s.StopTimeline(); Until(s, v => v.TimelinePlayback?.State == PlaybackState.Stopped && v.TimelinePreview is { State: "ready", PositionMs: 0 });
        s.SeekTimelineMilliseconds(5900); Until(s, v => v.TimelinePreview is { Busy: false, PositionMs: 5900 }); s.PlayTimeline();
        Until(s, v => v.TimelinePlayback?.State == PlaybackState.Ended);
        Assert.Equal(6000, s.ReadView().TimelinePlayback!.ClockMs);
        s.PlayTimeline(); var replay = Until(s, v => v.TimelinePlayback is { State: PlaybackState.Playing, ClockMs: < 500 } && v.Frame is not null);
        Assert.Equal(a, replay.TimelinePreview!.MediaId);
        s.SetPreviewMode(PreviewMode.Source); Assert.Equal(PlaybackState.Paused, s.ReadView().TimelinePlayback!.State);
        Assert.Equal(sourcePosition, s.ReadView().Source.PositionMs);
        s.Play(); Until(s, v => v.Playback?.State == PlaybackState.Playing); s.Pause();
        var frozen = s.ReadView().TimelinePlayback!.ClockTicks;
        s.SetPreviewMode(PreviewMode.Timeline); Assert.Equal(frozen, s.ReadView().TimelinePlayback!.ClockTicks);
        Assert.False(s.ReadView().TimelinePlayback!.DesiredPlaying);
    }

    [Fact]
    public void SavedGapVfrSourceRangeAndOpenDuringPlaybackUseCurrentDocument()
    {
        using var files = new Workspace(); using var s = EngineSession.Start(); var (a,b,track) = Prepare(s);
        var path = files.At("gap.cutcap"); s.SaveProjectAs(path); Until(s, v => v.Project!.Document!.State == "saved" && v.Pending == 0);
        var document = JsonNode.Parse(File.ReadAllText(path))!;
        document["project"]!["clips"]![1]!["start"] = 2_880_000; // 4 seconds: a real saved gap
        File.WriteAllText(path, document.ToJsonString());
        s.PlayTimeline(); Playing(s); s.OpenProject(path);
        Until(s, v => v.Project!.Document!.State == "opened" && v.Media.All(m => m.Availability == "ready") && v.Pending == 0);
        Assert.Equal(PlaybackState.Stopped, s.ReadView().TimelinePlayback!.State);
        s.SeekTimelineMilliseconds(2900); Until(s, v => v.TimelinePreview is { Busy: false, PositionMs: 2900 });
        Assert.Equal("no_frame", s.ReadView().TimelinePreview!.State); // still ceiling differs from playback interval
        s.PlayTimeline(); Playing(s);
        var gap = Until(s, v => v.TimelinePlayback!.ClockMs >= 3200);
        Assert.Null(gap.Frame); Assert.Equal("blank", gap.TimelinePreview!.State); Assert.Equal(PlaybackState.Playing, gap.TimelinePlayback!.State);
        var after = Until(s, v => v.TimelinePlayback!.ClockMs >= 4100 && v.Frame is not null);
        Assert.Equal(b, after.TimelinePreview!.MediaId); Assert.Equal(5000, after.Frame!.Actual!.Ticks);
        var clips = s.ReadTimeline().Clips;
        s.RemoveClip(clips[1].Id); Until(s, v => v.Project!.ClipCount == 1 && v.TimelinePlayback?.State == PlaybackState.Stopped);
        Assert.Null(s.ReadView().Frame);
        s.Undo(); Until(s, v => v.Project!.ClipCount == 2 && v.TimelinePreview?.State == "ready");
        Assert.Equal(b, s.ReadView().TimelinePreview!.MediaId);
        s.PlayTimeline(); Playing(s); s.AddTrack(TrackKind.Video);
        Until(s, v => v.TimelinePreview is { TrackName: "Video 2", State: "blank" }); Assert.Equal(PlaybackState.Stopped, s.ReadView().TimelinePlayback!.State);
        s.SelectTrack(track); Until(s, v => v.TimelinePreview is { TrackName: "Video 1", State: "ready" });
        s.PlayTimeline(); Playing(s);
        var close = Stopwatch.StartNew(); s.Dispose(); Assert.True(close.Elapsed < TimeSpan.FromSeconds(2));
        Assert.NotEmpty(a);
    }

    [Fact]
    public void VfrPlaybackSeekUsesCoveringFrameAndExclusiveShortEndpoint()
    {
        InteropTests.RequireFfmpeg(); using var files = new Workspace(); using var s = EngineSession.Start();
        Until(s,v=>v.EngineReady); s.Import(Fixture("colors-vfr.mkv"));
        var imported = Until(s,v=>v.Media.Count==1 && v.Frame is not null && !v.Import.Busy);
        s.InsertClip(imported.Media[0].Id,imported.Tracks[0].Id); Until(s,v=>v.Project!.ClipCount==1 && v.Pending==0);
        var path=files.At("short.cutcap"); s.SaveProjectAs(path); Until(s,v=>v.Project!.Document!.State=="saved" && v.Pending==0);
        var document=JsonNode.Parse(File.ReadAllText(path))!; document["project"]!["clips"]![0]!["duration"]=432_000; File.WriteAllText(path,document.ToJsonString());
        s.OpenProject(path); Until(s,v=>v.Project!.Document!.State=="opened" && v.Media.All(m=>m.Availability=="ready"));
        s.SetPreviewMode(PreviewMode.Timeline); Until(s,v=>v.TimelinePreview?.State=="ready");
        s.PlayTimeline(); Playing(s); s.PauseTimeline(); s.SeekTimelineMilliseconds(201);
        var sought=Until(s,v=>v.TimelinePlayback is {State:PlaybackState.Paused,ClockMs:201} && v.Frame is not null);
        Assert.Equal(200,sought.Frame!.Actual!.Ticks); var green=Copy(s); Assert.True(green[1]>200 && green[0]<50);
        s.PlayTimeline(); Until(s,v=>v.TimelinePlayback?.State==PlaybackState.Ended,v=> { if(v.Frame is not null) Assert.InRange(v.Frame.Actual!.Ticks,0,599); });
        Assert.Equal(600,s.ReadView().TimelinePlayback!.ClockMs); Assert.Equal(200,s.ReadView().Frame!.Actual!.Ticks);
    }

    [Fact]
    public void MissingRelinkAndSameSourceOffsetCutsPreserveClipsAndBoundFrames()
    {
        InteropTests.RequireFfmpeg(); using var files = new Workspace();
        var original=files.At("source 视频.mkv"); var moved=files.At("relinked 视频.mkv"); var path=files.At("offset-cuts.cutcap");
        File.Copy(Fixture("colors-cfr.mkv"),original);
        using(var s=EngineSession.Start())
        {
            Until(s,v=>v.EngineReady); s.Import(original); var v=Until(s,v=>v.Media.Count==1 && v.Frame is not null && !v.Import.Busy);
            s.InsertClip(v.Media[0].Id,v.Tracks[0].Id); Until(s,v=>v.Project!.ClipCount==1 && v.Pending==0);
            s.InsertClip(v.Media[0].Id,v.Tracks[0].Id); Until(s,v=>v.Project!.ClipCount==2 && v.Pending==0);
            s.SaveProjectAs(path); Until(s,v=>v.Project!.Document!.State=="saved" && v.Pending==0);
        }
        var doc=JsonNode.Parse(File.ReadAllText(path))!; var clips=doc["project"]!["clips"]!;
        clips[0]!["duration"]=288000; clips[0]!["source_in"]!["ticks"]=500;
        clips[1]!["start"]=288000; clips[1]!["duration"]=432000; clips[1]!["source_in"]!["ticks"]=1500;
        File.WriteAllText(path,doc.ToJsonString()); File.Move(original,moved);
        using var reopened=EngineSession.Start(); Until(reopened,v=>v.EngineReady); reopened.OpenProject(path);
        var missing=Until(reopened,v=>v.Project!.Document!.State=="opened" && v.Media is [{Availability:"missing"}]);
        Assert.Equal(2,missing.Project!.ClipCount); reopened.SetPreviewMode(PreviewMode.Timeline);
        Until(reopened,v=>v.TimelinePreview?.State=="unavailable"); reopened.PlayTimeline();
        var failed=Until(reopened,v=>v.TimelinePlayback?.State==PlaybackState.Failed);
        Assert.Null(failed.Frame); Assert.False(failed.TimelinePlayback!.DesiredPlaying); Assert.NotNull(failed.TimelinePlayback.Error);
        reopened.RelinkMedia(missing.Media[0].Id,moved); Until(reopened,v=>v.Media[0].Availability=="ready" && v.TimelinePreview?.State=="ready");
        reopened.PlayTimeline(); var crossed=Until(reopened,v=>v.TimelinePlayback!.ClockMs>=500,v=>
        {
            if(v.Frame?.Actual is not {} pts || v.TimelinePlayback?.State!=PlaybackState.Playing) return;
            Assert.InRange(pts.Ticks,v.TimelinePlayback.ClockMs<400?500:1500,v.TimelinePlayback.ClockMs<400?899:2099);
        });
        Assert.Equal(1UL,crossed.TimelinePlayback!.Handoffs);
        Until(reopened,v=>v.TimelinePlayback?.State==PlaybackState.Ended);
        reopened.SeekTimelineMilliseconds(100); var paused=Until(reopened,v=>v.TimelinePlayback is {State:PlaybackState.Paused,ClockMs:100} && v.Frame is not null);
        Assert.False(paused.TimelinePlayback!.DesiredPlaying); // seeking after Ended does not silently resume
    }
}

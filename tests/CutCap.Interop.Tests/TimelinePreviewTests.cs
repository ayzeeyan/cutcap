using System.Diagnostics;
using System.Text.Json;
using System.Text.Json.Nodes;
using Xunit;

namespace CutCap.Interop.Tests;

[Collection("engine")]
public sealed class TimelinePreviewTests
{
    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-timeline-preview-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }
    private static string Fixture(string name) => Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "../../../../../../tests/fixtures", name));
    private static SessionView Until(EngineSession session, Func<SessionView, bool> done)
    {
        var deadline = Stopwatch.StartNew();
        while (true)
        {
            session.Pump(); var view = session.ReadView();
            if (done(view)) return view;
            Assert.True(deadline.Elapsed < TimeSpan.FromSeconds(25), $"{view.Status}; {view.TimelinePreview}; {view.Source}");
            Thread.Sleep(5);
        }
    }
    private static SessionView Ready(EngineSession s) => Until(s, v => v.EngineReady && v.Pending == 0);
    private static SessionView Import(EngineSession s, string path, int count)
    {
        s.Import(path);
        return Until(s, v => v.Media.Count == count && !v.Import.Busy && v.Frame is not null && !v.Source.Busy);
    }
    private static SessionView Seek(EngineSession s, long milliseconds)
    {
        s.SeekTimelineMilliseconds(milliseconds);
        return Until(s, v => v.TimelinePreview is { Busy: false } p && p.PositionMs == milliseconds);
    }
    private static (string A, string B, string Track) TwoClips(EngineSession s, string second = "offset.mkv")
    {
        InteropTests.RequireFfmpeg(); Ready(s);
        var first = Import(s, Fixture("colors-cfr.mkv"), 1);
        var a = first.Media[0].Id; var track = first.Tracks[0].Id;
        s.InsertClip(a, track); Until(s, v => v.Project!.ClipCount == 1);
        var imported = Import(s, Fixture(second), 2);
        var b = imported.Media[1].Id;
        s.InsertClip(b, track); Until(s, v => v.Project!.ClipCount == 2 && v.Pending == 0);
        return (a,b,track);
    }
    private static byte[] Pixels(EngineSession session)
    {
        Assert.True(session.TryGetFrameInfo(out var info));
        var bytes = new byte[checked((int)(info.Stride * info.Height))];
        Assert.Equal(FrameCopyResult.Copied, session.CopyFrame(info.Generation, bytes, checked((int)info.Stride)));
        return bytes;
    }
    /// <summary>Independent demux/decode timestamp oracle; never nominal frame multiplication.</summary>
    private static long CeilingPts(string file, long requested)
    {
        var start = new ProcessStartInfo(Environment.GetEnvironmentVariable("CUTCAP_FFPROBE") ?? "ffprobe.exe")
        { UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true };
        foreach (var arg in new[] { "-v", "error", "-select_streams", "v:0", "-show_frames", "-show_entries", "frame=best_effort_timestamp", "-of", "json", file }) start.ArgumentList.Add(arg);
        using var child = Process.Start(start)!;
        var output = child.StandardOutput.ReadToEndAsync(); var error = child.StandardError.ReadToEndAsync();
        if (!child.WaitForExit(10_000)) { child.Kill(true); child.WaitForExit(); throw new TimeoutException("Oracle FFprobe timed out."); }
        Assert.Equal(0, child.ExitCode);
        var json = output.GetAwaiter().GetResult();
        Assert.True(json.Length < 1024 * 1024, error.GetAwaiter().GetResult());
        using var document = JsonDocument.Parse(json);
        return document.RootElement.GetProperty("frames").EnumerateArray()
            .Select(f => f.GetProperty("best_effort_timestamp").GetInt64()).First(p => p >= requested);
    }

    [Fact]
    public void RealFramesCutsGapsAndSourceModeHaveIndependentPositionsAndPixels()
    {
        using var files = new Workspace(); using var s = EngineSession.Start();
        var (a,b,track) = TwoClips(s);
        s.SaveProjectAs(files.At("timeline.cutcap")); Until(s, v => v.Project!.Document!.State == "saved" && v.Pending == 0);
        s.Seek(1250); Until(s, v => !v.Source.Busy && v.Source.PositionMs == 1250);
        var sourceFrame = s.ReadView().Frame!; var timelineDocument = s.ReadTimeline();
        var first = Seek(s, 500);
        Assert.Equal(PreviewMode.Timeline, first.PreviewMode);
        Assert.Equal(a, first.TimelinePreview!.MediaId); Assert.Equal(track, first.TimelinePreview.TrackId);
        Assert.Equal(CeilingPts(Fixture("colors-cfr.mkv"),500), first.Frame!.Actual!.Ticks);
        var red = Pixels(s); Assert.True(red[0] > 200 && red[1] < 60 && red[2] < 60);
        var cut = Seek(s, 3000);
        Assert.Equal(b, cut.TimelinePreview!.MediaId);
        Assert.Equal(5000, cut.Frame!.Actual!.Ticks);
        var second = Seek(s,4000);
        Assert.Equal(CeilingPts(Fixture("offset.mkv"),6000), second.Frame!.Actual!.Ticks);
        var green = Pixels(s); Assert.True(green[0] < 60 && green[1] > 90 && green[2] < 60);
        var blank = Seek(s,6000);
        Assert.Equal("blank", blank.TimelinePreview!.State); Assert.Null(blank.Frame);
        Assert.False(s.TryGetFrameInfo(out _));
        Assert.False(blank.Project!.Document!.Dirty);
        Assert.Equal(timelineDocument.Generation, s.ReadTimeline().Generation);
        Assert.Throws<EngineException>(() => s.Play());
        s.SetPreviewMode(PreviewMode.Source);
        var restored = s.ReadView();
        Assert.Equal(1250, restored.Source.PositionMs); Assert.Equal(sourceFrame.Actual, restored.Frame!.Actual);
        Assert.True(restored.Frame.Generation > sourceFrame.Generation);
        Assert.Equal(6000, restored.TimelinePreview!.PositionMs);
        Assert.Throws<EngineException>(() => s.SeekTimeline(-1));
        Assert.Equal(PreviewMode.Source, s.ReadView().PreviewMode);
    }

    [Fact]
    public void RemovalUndoTrackSelectionAndRapidSeeksUseTheLatestModel()
    {
        using var s = EngineSession.Start(); var (a,b,track) = TwoClips(s);
        var clips = s.ReadTimeline().Clips;
        Seek(s,500); s.RemoveClip(clips[0].Id);
        var removed = Until(s, v => v.Project!.ClipCount == 1 && v.TimelinePreview?.State == "blank");
        Assert.Null(removed.Frame);
        s.Undo(); var restored = Until(s, v => v.Project!.ClipCount == 2 && v.TimelinePreview?.State == "ready");
        Assert.Equal(a, restored.TimelinePreview!.MediaId);
        s.Redo(); Until(s, v => v.Project!.ClipCount == 1 && v.TimelinePreview?.State == "blank");
        s.Undo(); Until(s,v => v.Project!.ClipCount == 2 && v.TimelinePreview?.State == "ready");
        foreach (var target in new long[] { 400,1000,2900,3000,3500 }) { s.SeekTimelineMilliseconds(target); s.Pump(); }
        var latest = Until(s, v => v.TimelinePreview is { State: "ready", PositionMs:3500 });
        Assert.Equal(b,latest.TimelinePreview!.MediaId); Assert.Equal(5500,latest.Frame!.Actual!.Ticks);
        s.AddTrack(TrackKind.Video);
        var empty = Until(s, v => v.TimelinePreview is { TrackName:"Video 2", State:"blank" });
        Assert.Null(empty.Frame);
        s.SelectTrack(track);
        Assert.Equal(b,Until(s,v => v.TimelinePreview is { TrackName:"Video 1", State:"ready" }).TimelinePreview!.MediaId);
        s.SeekTimelineMilliseconds(long.MaxValue);
        var invalid = Until(s,v => v.TimelinePreview?.State == "failed");
        Assert.Equal(3500,invalid.TimelinePreview!.PositionMs);
    }

    [Fact]
    public void SavedVfrShortClipRejectsAnOracleFrameBeyondItsExclusiveEnd()
    {
        using var files = new Workspace(); using var s = EngineSession.Start();
        TwoClips(s,"colors-vfr.mkv");
        var path = files.At("short.cutcap"); s.SaveProjectAs(path);
        Until(s,v => v.Project!.Document!.State == "saved" && v.Pending == 0);
        var json = JsonNode.Parse(File.ReadAllText(path))!;
        var clips = json["project"]!["clips"]!.AsArray();
        // Keep just the VFR clip, at zero, but end it at 600 ms.
        var vfr = clips[1]!.DeepClone(); vfr["start"]=0; vfr["duration"]=432_000;
        clips.Clear(); clips.Add(vfr); File.WriteAllText(path,json.ToJsonString());
        s.OpenProject(path); Until(s,v => v.Project!.Document!.State == "opened" && v.Media.All(m => m.Availability=="ready"));
        Assert.Equal(700,CeilingPts(Fixture("colors-vfr.mkv"),201));
        var inside = Seek(s,201);
        Assert.Equal("no_frame",inside.TimelinePreview!.State); Assert.Null(inside.Frame);
        Assert.Equal("ready",Seek(s,200).TimelinePreview!.State);
        Assert.Equal(200,s.ReadView().Frame!.Actual!.Ticks);
        Assert.Equal("blank",Seek(s,600).TimelinePreview!.State);
        Assert.False(s.ReadView().Project!.Document!.Dirty);
    }

    [Fact]
    public void ReopenMissingRelinkAndModeSwitchDuringDecodeAreSafe()
    {
        InteropTests.RequireFfmpeg(); using var files = new Workspace();
        var original = files.At("original 视频.mkv"); var relocated = files.At("relocated 视频.mkv");
        File.Copy(Fixture("colors-cfr.mkv"),original);
        using var s=EngineSession.Start(); Ready(s); var imported=Import(s,original,1);
        var id=imported.Media[0].Id; s.InsertClip(id,imported.Tracks[0].Id); Until(s,v=>v.Project!.ClipCount==1);
        var path=files.At("project.cutcap"); s.SaveProjectAs(path); Until(s,v=>v.Project!.Document!.State=="saved"&&v.Pending==0);
        Seek(s,500); File.Move(original,relocated);
        s.OpenProject(path); var missing=Until(s,v=>v.Media[0].Availability=="missing"&&v.TimelinePreview?.State=="unavailable");
        Assert.Null(missing.Frame); Assert.Equal(1,missing.Project!.ClipCount);
        s.RelinkMedia(id,relocated); var linked=Until(s,v=>v.TimelinePreview?.State=="ready");
        Assert.Equal(id,linked.TimelinePreview!.MediaId); Assert.True(linked.Project!.Document!.Dirty);
        // Opening intentionally clears transient source selection. Select it
        // independently while the Timeline monitor remains active.
        s.SelectMedia(id); Until(s,v=>v.Media[0].Selected && v.Pending==0);
        s.SeekTimelineMilliseconds(1500); s.Pump();
        s.SetPreviewMode(PreviewMode.Source);
        Until(s,v=>v.PreviewMode==PreviewMode.Source && v.Source.Ready && !v.Source.Busy);
        var source=s.ReadView().Frame;
        Thread.Sleep(120); s.Pump(); Assert.Equal(source,s.ReadView().Frame);
        s.SetPreviewMode(PreviewMode.Timeline); Until(s,v=>v.TimelinePreview?.State=="ready");
        s.SeekTimelineMilliseconds(1000);
        var close=Stopwatch.StartNew(); s.Dispose(); Assert.True(close.Elapsed<TimeSpan.FromSeconds(2));
    }
}

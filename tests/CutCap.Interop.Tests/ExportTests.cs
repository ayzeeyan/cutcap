using System.Diagnostics;
using System.Text.Json.Nodes;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Phase 5A over the real ABI 11: argument handling always, and a real export
/// through FFmpeg when it is installed, checked by an independent FFprobe.
/// </summary>
[Collection("engine")]
public sealed class ExportTests
{
    private sealed class Workspace : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-export-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }

    private static string Run(string tool, params string[] arguments)
    {
        var info = new ProcessStartInfo(tool) { RedirectStandardError = true, RedirectStandardOutput = true, UseShellExecute = false };
        foreach (var a in arguments) info.ArgumentList.Add(a);
        using var process = Process.Start(info)!;
        var errors = process.StandardError.ReadToEndAsync();
        var output = process.StandardOutput.ReadToEnd();
        process.WaitForExit();
        Assert.True(process.ExitCode == 0, errors.Result);
        return output;
    }

    private static SessionView Until(EngineSession s, Func<SessionView, bool> done, int seconds = 60)
    {
        var watch = Stopwatch.StartNew();
        while (true)
        {
            s.Pump();
            var view = s.ReadView();
            if (done(view)) return view;
            Assert.True(watch.Elapsed < TimeSpan.FromSeconds(seconds), view.Export?.Job?.ToString() ?? view.Status.Text);
            Thread.Sleep(5);
        }
    }

    private static string Clip(Workspace files, string name, int seconds) =>
        Path.GetFullPath(new[] { files.At(name) }.Select(path =>
        {
            Run("ffmpeg", "-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", $"color=c=blue:s=320x180:r=30:d={seconds}",
                "-f", "lavfi", "-i", $"sine=frequency=440:sample_rate=48000:duration={seconds}",
                "-map", "0:v", "-map", "1:a", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", path);
            return path;
        }).Single());

    private static (SessionView View, string Track) Placed(EngineSession s, string path)
    {
        Until(s, x => x.EngineReady);
        s.Import(path);
        var v = Until(s, x => x.Media.Count == 1 && !x.Import.Busy && x.Media[0].Availability == "ready" && x.Pending == 0);
        s.PlaceClip(v.Media[0].Id, v.Tracks[0].Id, TimelinePosition.Milliseconds(0));
        v = Until(s, x => x.Pending == 0 && x.Export?.Summary.VideoTracks[0].Clips == 1);
        return (v, v.Tracks[0].Id);
    }

    [Fact]
    public void BadArgumentsAndStaleJobsAreRefusedByTheAbi()
    {
        using var s = EngineSession.Start();
        var v = Until(s, x => x.EngineReady);
        Assert.NotNull(v.Export);
        Assert.Throws<ArgumentException>(() => s.StartExport("", null, false, false));
        Assert.Throws<EngineException>(() => s.StartExport(@"C:\x.mp4", "not-a-uuid", false, false));
        Assert.Throws<EngineException>(() => s.CancelExport("00000000-0000-4000-8000-000000000001"));
        Assert.Equal("The timeline is empty, so there is nothing to export.", v.Export!.Summary.VideoTracks[0].Blocked);
    }

    [Fact]
    public void AnEmptyTimelineBecomesAFailedJobNotAFile()
    {
        using var files = new Workspace();
        using var s = EngineSession.Start();
        var v = Until(s, x => x.EngineReady);
        s.StartExport(files.At("x.mp4"), v.Tracks[0].Id, false, false);
        var failed = Until(s, x => x.Export?.Job?.Running == false);
        Assert.Equal("failed", failed.Export!.Job!.State);
        Assert.Contains("empty", failed.Export.Job.Error);
        Assert.False(File.Exists(files.At("x.mp4")));
    }

    [Fact]
    public void ARealExportProducesAPlayableFileAndLeavesTheProjectAlone()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Workspace();
        using var s = EngineSession.Start();
        var (placed, track) = Placed(s, Clip(files, "src.mp4", 2));
        Until(s, x => x.Export?.Available == true);
        var (dirty, revision, depth) = (placed.Project!.Document!.Dirty, placed.Project.Document.Revision, placed.History?.Depth);
        Assert.Equal(60ul, placed.Export!.Summary.VideoTracks[0].Frames);
        var output = files.At("结果 export.mp4");
        s.StartExport(output, track, false, false);
        var accepted = s.ReadView();
        Assert.True(accepted.Export!.Job!.Running);
        Assert.True(accepted.Export.Job.Progress < 0.5);
        var seen = new List<double>();
        var done = Until(s, x =>
        {
            if (x.Export?.Job is { Running: true } j) seen.Add(j.Progress);
            return x.Export?.Job?.Running == false;
        });
        Assert.Equal("completed", done.Export!.Job!.State);
        Assert.Equal(1.0, done.Export.Job.Progress);
        Assert.Equal(seen.OrderBy(p => p), seen);
        Assert.All(seen, p => Assert.True(p <= 0.98, $"{p} reported before finalization"));
        Assert.True(File.Exists(output));
        Assert.Equal(output, done.Export.Job.Result!.Path);
        Assert.Equal(60ul, done.Export.Job.Result.Frames);
        Assert.Equal(revision, done.Export.Job.Revision);
        Assert.Equal(dirty, done.Project!.Document!.Dirty);
        Assert.Equal(revision, done.Project.Document.Revision);
        Assert.Equal(depth, done.History?.Depth);
        var info = JsonNode.Parse(Run("ffprobe", "-v", "error", "-show_entries",
            "stream=codec_name,width,height,nb_frames,avg_frame_rate,pix_fmt,sample_rate,channels", "-of", "json", output))!["streams"]!.AsArray();
        Assert.Equal("h264", (string?)info[0]!["codec_name"]);
        Assert.Equal(1280, (int?)info[0]!["width"]);
        Assert.Equal("60", (string?)info[0]!["nb_frames"]);
        Assert.Equal("30/1", (string?)info[0]!["avg_frame_rate"]);
        Assert.Equal("aac", (string?)info[1]!["codec_name"]);
        Assert.Equal("48000", (string?)info[1]!["sample_rate"]);
        Assert.Empty(Directory.GetFiles(files.Root, "*.cutcap-*"));
    }

    [Fact]
    public void ACancelledExportRemovesItsFilesAndAcceptsANewOne()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Workspace();
        using var s = EngineSession.Start();
        var (_, track) = Placed(s, Clip(files, "src.mp4", 30));
        Until(s, x => x.Export?.Available == true);
        var output = files.At("out.mp4");
        s.StartExport(output, track, false, false);
        var running = Until(s, x => x.Export?.Job is { FramesEncoded: > 0 });
        var id = running.Export!.Job!.Id;
        var watch = Stopwatch.StartNew();
        s.CancelExport(id);
        Assert.True(watch.ElapsedMilliseconds < 200, "Cancel is an acceptance, not a wait");
        var cancelled = Until(s, x => x.Export?.Job?.Running == false);
        Assert.Equal("cancelled", cancelled.Export!.Job!.State);
        Assert.False(File.Exists(output));
        Assert.Empty(Directory.GetFiles(files.Root, "*.cutcap-*"));
        Assert.Throws<EngineException>(() => s.CancelExport(id));
    }

    [Fact]
    public void DisposingDuringAnExportEndsItPromptly()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Workspace();
        var s = EngineSession.Start();
        var (_, track) = Placed(s, Clip(files, "src.mp4", 30));
        Until(s, x => x.Export?.Available == true);
        s.StartExport(files.At("out.mp4"), track, false, false);
        Until(s, x => x.Export?.Job is { FramesEncoded: > 0 });
        var watch = Stopwatch.StartNew();
        s.Dispose();
        Assert.True(watch.Elapsed < TimeSpan.FromSeconds(5), watch.Elapsed.ToString());
        Assert.False(File.Exists(files.At("out.mp4")));
        Assert.Empty(Directory.GetFiles(files.Root, "*.cutcap-*"));
    }
}

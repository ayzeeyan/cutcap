using System.Diagnostics;
using System.Text.Json.Nodes;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Phase 3B clip editing across the real ABI: insertion, removal, selection,
/// Rust-owned undo/redo, the separately fetched timeline document, and
/// persistence of clips with real FFmpeg-probed sources.
/// </summary>
[Collection("engine")]
public sealed class TimelineEditingTests
{
    private sealed class Files : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-timeline-test-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }

    /// <summary>A view plus the timeline document, fetched only when its generation changes.</summary>
    private sealed class Reader(EngineSession session)
    {
        public TimelineView? Timeline { get; private set; }
        public int TimelineReads { get; private set; }

        public SessionView Until(Func<SessionView, TimelineView, bool> done)
        {
            var watch = Stopwatch.StartNew();
            while (true)
            {
                session.Pump();
                var view = session.ReadView();
                if (Timeline is null || Timeline.Generation != view.TimelineGeneration)
                {
                    Timeline = session.ReadTimeline();
                    TimelineReads++;
                }
                if (done(view, Timeline)) { return view; }
                Assert.True(watch.Elapsed < TimeSpan.FromSeconds(25),
                    $"{view.Status}; {view.Project?.Document}; clips {Timeline.Clips.Count}; {view.History}");
                Thread.Sleep(5);
            }
        }

        public SessionView Settled(Func<TimelineView, bool> done) => Until((v, t) => v.Pending == 0 && done(t));
    }

    private static string Video()
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName, "Cargo.toml"))) { root = root.Parent; }
        return Path.Combine(root?.FullName ?? throw new InvalidOperationException("Repo not found"), "tests", "fixtures", "colors-cfr.mkv");
    }

    private static SessionView Imported(EngineSession session, Reader reader, string path)
    {
        reader.Until((v, _) => v.EngineReady && v.Pending == 0);
        session.Import(path);
        return reader.Until((v, _) => v.Frame is not null && !v.Source.Busy && v.Media.Count == 1 && v.Pending == 0);
    }

    [Fact]
    public void ArgumentsAreValidatedAtTheBoundaryAndEngineRejectionsArriveAsStatus()
    {
        using var session = EngineSession.Start();
        var reader = new Reader(session);
        var ready = reader.Until((v, _) => v.EngineReady && v.Pending == 0);
        Assert.Empty(reader.Timeline!.Clips);
        Assert.False(ready.History!.CanUndo);
        var error = Assert.Throws<EngineException>(() => session.InsertClip("not-a-uuid", ready.Tracks[0].Id));
        Assert.Equal(NativeStatus.InvalidArgument, error.Status);
        Assert.Equal(NativeStatus.InvalidArgument, Assert.Throws<EngineException>(() => session.RemoveClip("")).Status);
        Assert.Equal(NativeStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => session.SelectClip("00000000-0000-0000-0000-000000000000")).Status);
        session.Undo();
        var rejected = reader.Until((v, _) => v.Status.Error && v.Pending == 0);
        Assert.Equal("Nothing to undo.", rejected.Status.Text);
        session.InsertClip(Guid.NewGuid().ToString(), ready.Tracks[0].Id);
        var unknown = reader.Until((v, _) => v.Status.Text.Contains("ready source") && v.Pending == 0);
        Assert.Equal(ready.Project!.Document!.Revision, unknown.Project!.Document!.Revision);
        Assert.Empty(reader.Timeline!.Clips);
    }

    [Fact]
    public void RealSourceInsertRemoveUndoRedoKeepsIdentityAndDirtyState()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Files();
        using var session = EngineSession.Start();
        var reader = new Reader(session);
        var imported = Imported(session, reader, Video());
        var media = imported.Media[0];
        var video = imported.Tracks.First(t => t.IsVideo);
        var audio = imported.Tracks.First(t => !t.IsVideo);
        session.SaveProjectAs(files.At("edit.cutcap"));
        reader.Until((v, _) => v.Project?.Document?.State == "saved" && v.Pending == 0);

        session.InsertClip(media.Id, video.Id);
        session.InsertClip(media.Id, video.Id);
        var two = reader.Settled(t => t.Clips.Count == 2);
        var (first, second) = (reader.Timeline!.Clips[0], reader.Timeline.Clips[1]);
        Assert.Equal(0, first.StartMs);
        Assert.InRange(Math.Abs(first.DurationMs - media.DurationMs!.Value), 0, 1);
        Assert.Equal(first.EndMs, second.StartMs);
        Assert.Equal(Path.GetFileName(Video()), first.Name);
        Assert.Equal((video.Id, 0, media.Id, "ready"), (first.TrackId, first.TrackIndex, first.MediaId, first.Availability));
        Assert.True(second.Selected);
        Assert.Equal("clip", two.Inspector.Kind);
        Assert.True(two.Project!.Document!.Dirty);
        Assert.True(two.History!.CanUndo);
        Assert.Equal($"add {first.Name}", two.History.Undo);

        // Selection is transient.
        session.SelectClip(first.Id);
        var selected = reader.Settled(t => t.Clips[0].Selected);
        Assert.Equal(2, selected.History!.Depth);

        // Remove keeps the media asset; undo restores the exact clip at its index.
        session.RemoveClip(first.Id);
        var removed = reader.Settled(t => t.Clips.Count == 1);
        Assert.Single(removed.Media);
        Assert.Equal(second.Id, reader.Timeline.Clips[0].Id);
        session.Undo();
        reader.Settled(t => t.Clips.Count == 2);
        Assert.Equal([first.Id, second.Id], reader.Timeline.Clips.Select(c => c.Id));
        Assert.Equal(first.StartText, reader.Timeline.Clips[0].StartText);

        // Back to the saved (empty) state is clean; redo returns to dirty.
        session.Undo();
        session.Undo();
        var clean = reader.Settled(t => t.Clips.Count == 0);
        Assert.False(clean.Project!.Document!.Dirty);
        Assert.True(clean.History!.CanRedo);
        session.Redo();
        var redone = reader.Settled(t => t.Clips.Count == 1);
        Assert.True(redone.Project!.Document!.Dirty);
        Assert.Equal(first.Id, reader.Timeline.Clips[0].Id);

        // A divergent edit discards redo; a wrong-kind insert is rejected atomically.
        session.InsertClip(media.Id, video.Id);
        var divergent = reader.Settled(t => t.Clips.Count == 2);
        Assert.False(divergent.History!.CanRedo);
        Assert.NotEqual(second.Id, reader.Timeline.Clips[1].Id);
        session.InsertClip(media.Id, audio.Id);
        var wrongKind = reader.Until((v, _) => v.Status.Error && v.Pending == 0);
        Assert.Contains("video track", wrongKind.Status.Text);
        Assert.Equal(divergent.Project!.Document!.Revision, wrongKind.Project!.Document!.Revision);
        Assert.Equal(2, reader.Timeline.Clips.Count);
    }

    [Fact]
    public void ClipsSurviveSaveReopenAndMissingMediaWithoutHistory()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Files();
        var source = files.At("片段 source.mkv");
        File.Copy(Video(), source);
        string[] ids;
        string projectPath = files.At("clips.cutcap");
        using (var session = EngineSession.Start())
        {
            var reader = new Reader(session);
            var imported = Imported(session, reader, source);
            for (var i = 0; i < 3; i++) { session.InsertClip(imported.Media[0].Id, imported.Tracks[0].Id); }
            reader.Settled(t => t.Clips.Count == 3);
            ids = [.. reader.Timeline!.Clips.Select(c => c.Id)];
            session.SaveProjectAs(projectPath);
            reader.Until((v, _) => v.Project?.Document?.State == "saved" && v.Pending == 0);
        }
        var json = JsonNode.Parse(File.ReadAllText(projectPath))!;
        Assert.Equal(3, json["project"]!["clips"]!.AsArray().Count);

        using (var session = EngineSession.Start())
        {
            var reader = new Reader(session);
            reader.Until((v, _) => v.EngineReady && v.Pending == 0);
            session.OpenProject(projectPath);
            var opened = reader.Until((v, t) => v.Project?.Document?.State == "opened" && v.Pending == 0
                && t.Clips.Count == 3 && t.Clips.All(c => c.Availability == "ready"));
            Assert.Equal(ids, reader.Timeline!.Clips.Select(c => c.Id));
            Assert.All(reader.Timeline.Clips, c => Assert.False(c.Selected));
            Assert.False(opened.History!.CanUndo);
            Assert.False(opened.Project!.Document!.Dirty);
        }

        File.Delete(source);
        using (var session = EngineSession.Start())
        {
            var reader = new Reader(session);
            reader.Until((v, _) => v.EngineReady && v.Pending == 0);
            session.OpenProject(projectPath);
            var missing = reader.Until((v, t) => v.Pending == 0 && t.Clips.Count == 3 && t.Clips.All(c => c.Availability == "missing"));
            Assert.Equal(ids, reader.Timeline!.Clips.Select(c => c.Id));
            session.InsertClip(missing.Media[0].Id, missing.Tracks[0].Id);
            var refused = reader.Until((v, _) => v.Status.Error && v.Pending == 0);
            Assert.False(refused.Project!.Document!.Dirty);
            Assert.Equal(3, reader.Timeline.Clips.Count);
        }
    }

    [Fact]
    public void PlaybackFramesDoNotResendTheTimeline()
    {
        InteropTests.RequireFfmpeg();
        using var session = EngineSession.Start();
        var reader = new Reader(session);
        var imported = Imported(session, reader, Video());
        session.InsertClip(imported.Media[0].Id, imported.Tracks[0].Id);
        var placed = reader.Settled(t => t.Clips.Count == 1);
        var reads = reader.TimelineReads;
        session.Play();
        var playing = reader.Until((v, _) => v.Playback is { Offered: > 3 });
        Assert.Equal(placed.TimelineGeneration, playing.TimelineGeneration);
        Assert.Equal(reads, reader.TimelineReads);
        // Editing during playback neither restarts nor seeks the source.
        var epoch = playing.Playback!.Epoch;
        session.Undo();
        var undone = reader.Settled(t => t.Clips.Count == 0);
        Assert.Equal(epoch, undone.Playback!.Epoch);
        session.Stop();
    }
}

using System.Diagnostics;
using System.Text.Json.Nodes;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>Real ABI 3 document workflows; every file is owned by a disposable fixture directory.</summary>
[Collection("engine")]
public sealed class PersistenceTests
{
    private sealed class Files : IDisposable
    {
        public string Root { get; } = Directory.CreateTempSubdirectory("cutcap-project-test-").FullName;
        public string At(string name) => Path.Combine(Root, name);
        public void Dispose() => Directory.Delete(Root, true);
    }
    private static SessionView Until(EngineSession session, Func<SessionView, bool> done)
    {
        var deadline = Stopwatch.StartNew();
        while (true)
        {
            session.Pump();
            var view = session.ReadView();
            if (done(view)) { return view; }
            Assert.True(deadline.Elapsed < TimeSpan.FromSeconds(25), $"{view.Project?.Document}; {view.Status}; {view.Import}; {view.Source}; {view.Frame}");
            Thread.Sleep(5);
        }
    }
    private static SessionView Ready(EngineSession session) => Until(session, v => v.EngineReady && v.Pending == 0);
    private static SessionView Saved(EngineSession session) => Until(session, v => v.Project?.Document?.State == "saved" && v.Pending == 0);
    private static SessionView Opened(EngineSession session) => Until(session, v => v.Project?.Document?.State == "opened" && v.Pending == 0);
    private static string Video()
    {
        var root = new DirectoryInfo(AppContext.BaseDirectory);
        while (root is not null && !File.Exists(Path.Combine(root.FullName, "Cargo.toml"))) { root = root.Parent; }
        return Path.Combine(root?.FullName ?? throw new InvalidOperationException("Repo not found"), "tests", "fixtures", "colors-cfr.mkv");
    }
    private static SessionView Imported(EngineSession session, string path)
    {
        Ready(session);
        session.Import(path);
        return Until(session, v => v.Frame is not null && !v.Source.Busy && v.Media.Count == 1);
    }

    [Fact]
    public void NewSaveOpenPreservesIdentityOrderingAndKindsWithoutPersistingSelection()
    {
        using var files = new Files();
        using var session = EngineSession.Start();
        Ready(session);
        session.AddTrack(TrackKind.Video);
        session.AddTrack(TrackKind.Audio);
        var original = Until(session, v => v.Tracks.Count == 4 && v.Pending == 0);
        session.SaveProjectAs(files.At("project.cutcap"));
        var saved = Saved(session);
        Assert.False(saved.Project!.Document!.Dirty);
        session.NewProject();
        Until(session, v => v.Project?.Id != saved.Project.Id && v.Pending == 0);
        session.OpenProject(files.At("project.cutcap"));
        var opened = Opened(session);
        Assert.Equal(original.Project!.Id, opened.Project!.Id);
        Assert.Equal(original.Tracks.Select(t => (t.Id, t.Name, t.Kind)), opened.Tracks.Select(t => (t.Id, t.Name, t.Kind)));
        Assert.All(opened.Tracks, track => Assert.False(track.Selected));
        session.SelectTrack(opened.Tracks[0].Id);
        var selected = Until(session, v => v.Tracks[0].Selected && v.Pending == 0);
        Assert.False(selected.Project!.Document!.Dirty);
        session.AddTrack(TrackKind.Audio);
        Assert.True(Until(session, v => v.Tracks.Count == 5).Project!.Document!.Dirty);
    }

    [Fact]
    public void RealMediaSurvivesSessionDisposalOpenRevalidationPreviewAndPlayback()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Files();
        var source = files.At("视频 with spaces.mkv");
        File.Copy(Video(), source);
        string id, projectId;
        using (var session = EngineSession.Start())
        {
            var imported = Imported(session, source);
            id = imported.Media[0].Id;
            projectId = imported.Project!.Id;
            session.SaveProjectAs(files.At("工程.cutcap"));
            Saved(session);
        }
        var json = JsonNode.Parse(File.ReadAllText(files.At("工程.cutcap")))!;
        Assert.False(Path.IsPathRooted(json["project"]!["media"]![0]!["path"]!.GetValue<string>()));
        using var reopened = EngineSession.Start();
        Ready(reopened);
        reopened.OpenProject(files.At("工程.cutcap"));
        var loaded = Until(reopened, v => v.Media.Count == 1 && v.Media[0].Availability == "ready");
        Assert.Equal(projectId, loaded.Project!.Id);
        Assert.Equal(id, loaded.Media[0].Id);
        Assert.False(loaded.Source.Ready);
        reopened.SelectMedia(id);
        Until(reopened, v => v.Frame is not null && v.Source.Ready);
        reopened.Play();
        Until(reopened, v => v.Playback?.State == PlaybackState.Playing && v.Playback.CopiedMs is null);
        Assert.True(reopened.TryGetFrameInfo(out var info));
        var buffer = new byte[checked((int)(info.Stride * info.Height))];
        Assert.Equal(FrameCopyResult.Copied, reopened.CopyFrame(info.Generation, buffer, checked((int)info.Stride)));
        reopened.Pause();
        Assert.Equal(PlaybackState.Paused, reopened.ReadView().Playback!.State);
        Assert.False(reopened.ReadView().Project!.Document!.Dirty);
    }

    [Fact]
    public void MissingSourceIsRetainedAndRelinkAcceptsOnlyTheOriginalContent()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Files();
        var source = files.At("source.mkv");
        var moved = files.At("moved.mkv");
        File.Copy(Video(), source);
        using var session = EngineSession.Start();
        var original = Imported(session, source);
        session.SaveProjectAs(files.At("test.cutcap"));
        Saved(session);
        File.Move(source, moved);
        session.OpenProject(files.At("test.cutcap"));
        var missing = Until(session, v => v.Media.Count == 1 && v.Media[0].Availability == "missing");
        Assert.Equal(original.Project!.Id, missing.Project!.Id);
        Assert.Equal(original.Media[0].Id, missing.Media[0].Id);
        session.SelectMedia(missing.Media[0].Id);
        Assert.False(Until(session, v => v.Media[0].Selected).Source.Ready);
        session.RelinkMedia(missing.Media[0].Id, moved);
        var relinked = Until(session, v => v.Media[0].Availability == "ready" && v.Source.Ready && v.Frame is not null);
        Assert.True(relinked.Project!.Document!.Dirty);
        session.SaveProject();
        Saved(session);
        // Appending changes the content while keeping this Matroska readable.
        using (var append = new FileStream(moved, FileMode.Append)) { append.Write([0, 0, 0, 0]); }
        session.OpenProject(files.At("test.cutcap"));
        var changed = Until(session, v => v.Media[0].Availability == "changed");
        Assert.False(changed.Project!.Document!.Dirty);
        Assert.Contains("changed", changed.Media[0].Detail);
        session.RelinkMedia(changed.Media[0].Id, moved);
        var refused = Until(session, v => v.Status.Error && !v.Import.Busy && v.Pending == 0);
        Assert.Equal("changed", refused.Media[0].Availability);
        Assert.False(refused.Project!.Document!.Dirty);
    }

    [Fact]
    public void RelinkingAReadySourceSwitchesTheLiveDecoderPathWithoutChangingIdentity()
    {
        InteropTests.RequireFfmpeg();
        using var files = new Files();
        var source = files.At("original.mkv");
        var copy = files.At("replacement path.mkv");
        File.Copy(Video(), source);
        File.Copy(source, copy);
        using var session = EngineSession.Start();
        var original = Imported(session, source);
        var id = original.Media[0].Id;
        session.RelinkMedia(id, copy);
        var linked = Until(session, v => v.Source.Title == "replacement path.mkv" && v.Frame is not null && v.Frame.Generation != original.Frame!.Generation);
        Assert.Equal(id, linked.Media[0].Id);
        File.Delete(source); // disposable original fixture, never user media
        session.Seek(700);
        // This fixture is 4 fps: the first real PTS >= 700 ms is 750 ms.
        var sought = Until(session, v => !v.Source.Busy && v.Frame?.Actual?.Ticks == 750);
        Assert.True(sought.Source.HasFrame);
        Assert.Equal("replacement path.mkv", sought.Source.Title);
    }

    [Fact]
    public void InvalidOpenLeavesCurrentDocumentAndDirtyStateIntact()
    {
        using var files = new Files();
        using var session = EngineSession.Start();
        var original = Ready(session);
        foreach (var content in new[] { "{ corrupt", "{\"format\":\"CutCap\",\"format_version\":999}" })
        {
            File.WriteAllText(files.At("invalid.cutcap"), content);
            session.OpenProject(files.At("invalid.cutcap"));
            var failed = Until(session, v => v.Project?.Document?.State == "failed" && v.Pending == 0);
            Assert.Equal(original.Project!.Id, failed.Project!.Id);
            Assert.Equal(original.Tracks, failed.Tracks);
            Assert.True(failed.Project.Document!.Dirty);
            Assert.True(failed.Status.Error);
        }
    }

    [Fact]
    public void FailedOverwriteRetainsValidFileAndDoesNotMarkNewEditsSaved()
    {
        using var files = new Files();
        using var session = EngineSession.Start();
        Ready(session);
        var path = files.At("locked.cutcap");
        session.SaveProjectAs(path);
        Saved(session);
        var previous = File.ReadAllBytes(path);
        session.AddTrack(TrackKind.Video);
        Until(session, v => v.Tracks.Count == 3);
        using var locked = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        session.SaveProject();
        var failed = Until(session, v => v.Project?.Document?.State == "failed" && v.Pending == 0);
        Assert.True(failed.Project!.Document!.Dirty);
        Assert.True(failed.Status.Error);
        Assert.Equal(previous, File.ReadAllBytes(path));
    }

    [Fact]
    public void SaveSnapshotsDoNotMarkLaterEditsAsSaved()
    {
        using var files = new Files();
        using var session = EngineSession.Start();
        var original = Ready(session);
        session.SaveProjectAs(files.At("snapshot.cutcap"));
        session.AddTrack(TrackKind.Audio);
        var after = Until(session, v => v.Project?.Document?.State == "saved" && v.Tracks.Count == 3 && v.Pending == 0);
        Assert.True(after.Project!.Document!.Dirty);
        Assert.Equal(original.Project!.Document!.Revision, after.Project.Document.SavedRevision);
        session.OpenProject(files.At("snapshot.cutcap"));
        Assert.Equal(2, Opened(session).Tracks.Count);
    }

    [Fact]
    public void LongUnicodeProjectPathRoundTripsThroughWideAbi()
    {
        using var files = new Files();
        var directory = files.Root;
        for (var i = 0; i < 4; i++) { directory = Path.Combine(directory, new string('x', 70) + " 视频"); }
        Directory.CreateDirectory(directory);
        var path = Path.Combine(directory, "工程.cutcap");
        Assert.True(path.Length > 260);
        using var session = EngineSession.Start();
        var original = Ready(session);
        session.SaveProjectAs(path);
        Saved(session);
        session.NewProject();
        Until(session, v => v.Project?.Id != original.Project!.Id);
        session.OpenProject(path);
        Assert.Equal(original.Project!.Id, Opened(session).Project!.Id);
    }
}

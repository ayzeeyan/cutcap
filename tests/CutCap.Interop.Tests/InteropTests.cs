using System.Diagnostics;
using Xunit;

namespace CutCap.Interop.Tests;

/// <summary>
/// Rust–C# boundary tests against the real cutcap_engine.dll. Media tests use
/// the generated fixtures and the external FFmpeg/FFprobe (skipped when absent).
/// </summary>
public sealed class InteropTests
{
    private static readonly TimeSpan Timeout = TimeSpan.FromSeconds(20);

    private static string RepoRoot()
    {
        var directory = new DirectoryInfo(AppContext.BaseDirectory);
        while (directory is not null && !File.Exists(Path.Combine(directory.FullName, "Cargo.toml")))
        {
            directory = directory.Parent;
        }
        return directory?.FullName ?? throw new InvalidOperationException("Repository root not found.");
    }

    private static string Fixture(string name) => Path.Combine(RepoRoot(), "tests", "fixtures", name);

    private static bool HasTool(string name, string variable)
    {
        if (Environment.GetEnvironmentVariable(variable) is { Length: > 0 } configured)
        {
            return File.Exists(configured);
        }
        var path = Environment.GetEnvironmentVariable("PATH") ?? "";
        return path.Split(Path.PathSeparator).Any(folder => folder.Length > 0 && File.Exists(Path.Combine(folder, name)));
    }

    internal static void RequireFfmpeg()
    {
        Assert.SkipUnless(HasTool("ffprobe.exe", "CUTCAP_FFPROBE") && HasTool("ffmpeg.exe", "CUTCAP_FFMPEG"),
            "Requires the external development FFmpeg/FFprobe on PATH (never bundled).");
    }

    private static SessionView PumpUntil(EngineSession session, Func<SessionView, bool> done)
    {
        var watch = Stopwatch.StartNew();
        while (true)
        {
            session.Pump();
            var view = session.ReadView();
            if (done(view))
            {
                return view;
            }
            Assert.True(watch.Elapsed < Timeout, $"Timed out. Status: {view.Status.Text}; source: {view.Source.Status} {view.Source.Detail}; import: {view.Import.Message}");
            Thread.Sleep(10);
        }
    }

    private static SessionView Ready(EngineSession session) => PumpUntil(session, view => view.EngineReady);

    private static SessionView Import(EngineSession session, string path)
    {
        Ready(session);
        session.Import(path);
        return PumpUntil(session, view => view.Import.Error
            || (view.Media.Count > 0 && view.Frame is not null && !view.Source.Busy));
    }

    [Fact]
    public void LibraryAbiMatchesTheFrontend()
    {
        Assert.Equal(NativeMethods.AbiVersion, EngineSession.LibraryAbiVersion);
    }

    [Fact]
    public void AbiMismatchIsRejectedWithAMessage()
    {
        var status = NativeMethods.SessionCreate(NativeMethods.AbiVersion + 1, out var handle);
        Assert.Equal(NativeStatus.AbiMismatch, status);
        Assert.Equal(0ul, handle);
        Assert.Contains("ABI", EngineSession.LastError());
    }

    [Fact]
    public void InvalidHandlesAreRejectedNotDereferenced()
    {
        using var forged = new SessionSafeHandle(0xDEAD_BEEF_0000_0001, ownsHandle: false);
        Assert.Equal(NativeStatus.InvalidHandle, NativeMethods.NewProject(forged));
        Assert.Equal(NativeStatus.InvalidHandle, NativeMethods.Pump(forged, out _));
        Assert.Contains("Unknown or closed", EngineSession.LastError());
        Assert.Equal(NativeStatus.InvalidHandle, NativeMethods.SessionDestroy(0));
    }

    [Fact]
    public void DoubleDisposeIsHarmlessAndUseAfterDisposeThrows()
    {
        var session = EngineSession.Start();
        Ready(session);
        session.Dispose();
        session.Dispose();
        Assert.True(session.IsDisposed);
        Assert.Throws<ObjectDisposedException>(() => session.Pump());
        Assert.Throws<ObjectDisposedException>(() => session.AddTrack(TrackKind.Audio));
    }

    [Fact]
    public void RawDoubleDestroyReturnsInvalidHandle()
    {
        Assert.Equal(NativeStatus.Ok, NativeMethods.SessionCreate(NativeMethods.AbiVersion, out var value));
        Assert.Equal(NativeStatus.Ok, NativeMethods.SessionDestroy(value));
        Assert.Equal(NativeStatus.InvalidHandle, NativeMethods.SessionDestroy(value));
    }

    [Fact]
    public void NativeErrorsPropagateStatusAndMessage()
    {
        using var session = EngineSession.Start();
        Ready(session);
        var error = Assert.Throws<EngineException>(() => session.SeekText("1.5"));
        Assert.Equal(NativeStatus.InvalidArgument, error.Status);
        Assert.Contains("Enter a source time", error.Message);
        Assert.Equal(NativeStatus.InvalidArgument, Assert.Throws<EngineException>(() => session.SelectTrack("not-a-uuid")).Status);
        Assert.Equal(NativeStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => session.SelectMedia("00000000-0000-0000-0000-000000000000")).Status);
        Assert.Equal(NativeStatus.InvalidArgument, Assert.Throws<EngineException>(() => session.AddTrack((TrackKind)9)).Status);
        // The engine's own rejection arrives as view state.
        session.SelectMedia(Guid.NewGuid().ToString());
        var view = PumpUntil(session, v => v.Status.Text.Contains("no longer", StringComparison.Ordinal));
        Assert.True(view.Status.Error);
    }

    [Fact]
    public void ProjectAndTrackStateComeFromTheEngine()
    {
        using var session = EngineSession.Start();
        var first = Ready(session);
        Assert.NotNull(first.Project);
        Assert.False(first.Project.Saved);
        Assert.Equal(["V1", "A1"], first.Tracks.Select(t => t.Badge));
        session.AddTrack(TrackKind.Audio);
        var added = PumpUntil(session, v => v.Tracks.Count == 3);
        Assert.Equal("A2", added.Tracks[2].Badge);
        Assert.True(added.Tracks[2].Selected);
        session.SelectTrack(added.Tracks[0].Id);
        var selected = PumpUntil(session, v => v.Tracks[0].Selected);
        Assert.Equal("track", selected.Inspector.Kind);
        session.NewProject();
        var next = PumpUntil(session, v => v.EngineReady && v.Project?.Id != first.Project.Id);
        Assert.Equal(2, next.Tracks.Count);
    }

    [Fact]
    public void RealMetadataAndFirstFrameCrossTheBoundary()
    {
        RequireFfmpeg();
        using var session = EngineSession.Start();
        var view = Import(session, Fixture("colors-cfr.mkv"));
        Assert.False(view.Import.Error, view.Import.Message);
        var media = Assert.Single(view.Media);
        Assert.Equal("colors-cfr.mkv", media.Name);
        Assert.Equal("320 × 180", media.Resolution);
        Assert.StartsWith("FFV1", media.Format, StringComparison.Ordinal);
        Assert.False(media.HasAudio);
        Assert.True(media.Selected);
        Assert.Equal("source", view.Inspector.Kind);
        Assert.Contains(view.Inspector.Sections, s => s.Title == "Video" && s.Rows.Any(r => r.Value == "320 × 180"));
        Assert.Contains("PTS 0 @", view.Source.Caption, StringComparison.Ordinal);

        Assert.True(session.TryGetFrameInfo(out var info));
        Assert.Equal(view.Frame!.Generation, info.Generation);
        Assert.Equal(info.Width * 4, info.Stride);
        var pixels = new byte[(int)(info.Stride * info.Height)];
        Assert.Equal(FrameCopyResult.Copied, session.CopyFrame(info.Generation, pixels, (int)info.Stride));
        // The CFR fixture starts on a red frame; alpha is opaque.
        Assert.True(pixels[0] > 200 && pixels[1] < 60 && pixels[2] < 60, $"RGB {pixels[0]},{pixels[1]},{pixels[2]}");
        Assert.Equal(255, pixels[3]);
    }

    [Fact]
    public void UnicodePathsRoundTripAsUtf16()
    {
        RequireFfmpeg();
        var folder = Path.Combine(Path.GetTempPath(), $"cutcap 编辑 Видео {Guid.NewGuid():N}");
        Directory.CreateDirectory(folder);
        try
        {
            var path = Path.Combine(folder, "clip 🎬 ünï.mkv");
            File.Copy(Fixture("colors-cfr.mkv"), path);
            using var session = EngineSession.Start();
            var view = Import(session, path);
            Assert.False(view.Import.Error, view.Import.Message);
            Assert.Equal("clip 🎬 ünï.mkv", Assert.Single(view.Media).Name);
            Assert.Contains(view.Inspector.Sections.SelectMany(s => s.Rows), r => r.Label == "Folder" && r.Value == folder);
        }
        finally
        {
            Directory.Delete(folder, recursive: true);
        }
    }

    [Fact]
    public void MalformedFrameDestinationsAreRejectedAndGenerationsAreChecked()
    {
        RequireFfmpeg();
        using var session = EngineSession.Start();
        Import(session, Fixture("colors-cfr.mkv"));
        Assert.True(session.TryGetFrameInfo(out var info));
        var row = (int)info.Stride;
        var tooSmall = new byte[row * (int)info.Height - 1];
        Assert.Equal(NativeStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => session.CopyFrame(info.Generation, tooSmall, row)).Status);
        var full = new byte[row * (int)info.Height];
        Assert.Equal(NativeStatus.InvalidArgument,
            Assert.Throws<EngineException>(() => session.CopyFrame(info.Generation, full, row - 4)).Status);
        Assert.Equal(FrameCopyResult.Stale, session.CopyFrame(info.Generation + 1, full, row));
        Assert.Throws<ArgumentOutOfRangeException>(() => session.CopyFrame(info.Generation, 0, -1, row));
        // Padded rows, as a locked bitmap may have: padding stays untouched.
        var stride = row + 32;
        var padded = new byte[stride * (int)info.Height];
        Array.Fill(padded, (byte)0xAA);
        Assert.Equal(FrameCopyResult.Copied, session.CopyFrame(info.Generation, padded, stride));
        Assert.All(padded.AsSpan(row, 32).ToArray(), value => Assert.Equal(0xAA, value));
        Assert.Equal(255, padded[stride + 3]);
    }

    [Fact]
    public void RapidSeeksPresentOnlyTheLatestAndOldGenerationsGoStale()
    {
        RequireFfmpeg();
        using var session = EngineSession.Start();
        var first = Import(session, Fixture("colors-cfr.mkv"));
        var oldGeneration = first.Frame!.Generation;
        foreach (var ms in new[] { 2600, 1200, 2000, 1000 })
        {
            session.Seek(ms);
            session.Pump();
        }
        var view = PumpUntil(session, v => !v.Source.Busy && v.Frame?.Generation != oldGeneration);
        Assert.Equal(1000, view.Source.PositionMs);
        Assert.Contains("PTS 1000 @ 1/1000", view.Source.Caption, StringComparison.Ordinal);
        Assert.Equal(first.Metrics.FramesPresented + 1, view.Metrics.FramesPresented);
        Assert.Equal(FrameCopyResult.Stale, session.CopyFrame(oldGeneration, new byte[320 * 180 * 4], 320 * 4));
    }

    [Fact]
    public void VariableFrameRateSeeksUseRealTimestamps()
    {
        RequireFfmpeg();
        using var session = EngineSession.Start();
        Import(session, Fixture("colors-vfr.mkv"));
        session.SeekText("201");
        var view = PumpUntil(session, v => !v.Source.Busy && v.Source.PositionMs == 201);
        Assert.Contains("PTS 700 @ 1/1000", view.Source.Caption, StringComparison.Ordinal);
    }

    [Theory]
    [InlineData("corrupt.mp4", "damaged or unsupported")]
    [InlineData("does-not-exist.mp4", "")]
    public void BrokenOrMissingFilesFailWithoutRegistering(string name, string expected)
    {
        RequireFfmpeg();
        using var session = EngineSession.Start();
        var view = Import(session, Fixture(name));
        Assert.True(view.Import.Error);
        Assert.Empty(view.Media);
        Assert.Contains(expected, view.Import.Message, StringComparison.Ordinal);
    }

    [Fact]
    public void DisposeDuringActiveWorkIsBoundedAndReapsChildren()
    {
        RequireFfmpeg();
        var before = Process.GetProcessesByName("ffmpeg").Length + Process.GetProcessesByName("ffprobe").Length;
        var session = EngineSession.Start();
        Import(session, Fixture("audio-video.mp4"));
        session.Seek(2500);
        session.Import(Fixture("colors-vfr.mkv"));
        var watch = Stopwatch.StartNew();
        session.Dispose();
        Assert.True(watch.Elapsed < TimeSpan.FromSeconds(5), $"Dispose took {watch.Elapsed}");
        Thread.Sleep(300);
        var after = Process.GetProcessesByName("ffmpeg").Length + Process.GetProcessesByName("ffprobe").Length;
        Assert.True(after <= before, $"FFmpeg children before {before}, after {after}");
    }
}

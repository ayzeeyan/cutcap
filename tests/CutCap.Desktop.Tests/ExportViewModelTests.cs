using CutCap.Desktop.Services;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using Xunit;

namespace CutCap.Desktop.Tests;

/// <summary>Phase 5A dialog rules, without a running engine: everything comes from the view.</summary>
public sealed class ExportViewModelTests
{
    private static ExportTrackChoice Track(string id, int clips, string? blocked = null, ulong frames = 90, string name = "") =>
        new(id, name.Length > 0 ? name : $"Video {id}", clips, 3000, "00:03.000", frames, blocked);

    private static ExportSummary Summary(params ExportTrackChoice[] tracks) =>
        new("CutCap Standard MP4", "MP4 · 1280×720", tracks, tracks.FirstOrDefault(t => t.Clips > 0)?.Id ?? tracks.FirstOrDefault()?.Id, 2, 2, 7);

    private static ExportJobView Job(string state, double progress = 0, bool running = true, ExportResultView? result = null, string? error = null) =>
        new("job1", state, running, progress, "", @"C:\o\out.mp4", "out.mp4", "Cut", 7, "Video 1", 90, 40, 30, 1000, 1000, 2500, "00:02.500", 1.5, error, result);

    private static SessionView View(ExportView? export, bool ready = true) => new(
        1, ready, new ProjectView("p", "My Cut", false, 1), new StatusView("", false), 0, [], [], new ImportView(false, false, ""),
        new InspectorView("project", "", "", []), new SourceView(false, "", 0, 0, false, false, false, "", "", ""),
        null, new PreviewMetrics(null, null, null, 0), Export: export);

    private static ExportView Ready(params ExportTrackChoice[] tracks) => new(true, null, Summary(tracks), null);

    private static ExportViewModel Model(ExportView export)
    {
        var model = new ExportViewModel(new EngineHost());
        model.Apply(View(export));
        return model;
    }

    [Fact]
    public void StartNeedsAnAvailableEncoderAFileAndAnExportableTrack()
    {
        var model = Model(Ready(Track("1", 2)));
        Assert.False(model.StartCommand.CanExecute(null));
        Assert.Equal("Choose where to save the MP4.", model.OutputHint);
        model.OutputPath = @"C:\o\out.mp4";
        Assert.True(model.StartCommand.CanExecute(null));
        Assert.Equal("out.mp4", model.OutputName);
        model.Apply(View(new ExportView(false, "Export needs: this FFmpeg build has no libx264 H.264 encoder.", Summary(Track("1", 2)), null)));
        Assert.False(model.StartCommand.CanExecute(null));
        Assert.True(model.ShowUnavailable);
        Assert.Contains("libx264", model.UnavailableText);
    }

    [Fact]
    public void ABlockedTrackExplainsWhyAndCannotStart()
    {
        var model = Model(Ready(Track("1", 1, "“a.mp4” is missing or changed. Relink it to export.")));
        model.OutputPath = @"C:\o\out.mp4";
        Assert.False(model.StartCommand.CanExecute(null));
        Assert.True(model.ShowBlocked);
        Assert.Contains("Relink", model.BlockedText);
    }

    [Fact]
    public void OmittedOccupiedTracksMustBeAcknowledged()
    {
        var model = Model(Ready(Track("1", 2), Track("2", 1)));
        model.OutputPath = @"C:\o\out.mp4";
        Assert.Equal("1", model.SelectedTrack?.Id);
        Assert.True(model.NeedsAcknowledgment);
        Assert.Contains("Video 2", model.OmissionText);
        Assert.False(model.StartCommand.CanExecute(null));
        model.OmissionAcknowledged = true;
        Assert.True(model.StartCommand.CanExecute(null));
        // Choosing the other track names the other omission.
        model.SelectedTrack = model.Tracks[1];
        Assert.Contains("Video 1", model.OmissionText);
    }

    [Fact]
    public void AnEmptyVideoTrackSaysThePictureIsBlack()
    {
        var model = Model(Ready(Track("1", 0)));
        Assert.True(model.ShowPictureNote);
        Assert.Contains("black", model.PictureNote);
        Assert.False(model.NeedsAcknowledgment);
    }

    [Fact]
    public void TheChoiceOfTrackSurvivesEngineUpdates()
    {
        var model = Model(Ready(Track("1", 1), Track("2", 1)));
        model.SelectedTrack = model.Tracks[1];
        var item = model.Tracks[1];
        model.Apply(View(Ready(Track("1", 3), Track("2", 5))));
        Assert.Same(item, model.SelectedTrack);
        Assert.Equal("Video 2 · 5 clips", model.SelectedTrack!.Label);
    }

    [Fact]
    public void TheFiguresShownAreTheEngines()
    {
        var model = Model(Ready(Track("1", 2, frames: 1234)));
        Assert.Equal("00:03.000", model.DurationText);
        Assert.Equal("1,234 frames at 30 fps", model.FramesText);
        Assert.Equal("2 audio tracks", model.AudioText);
        Assert.Equal("Project revision 7", model.RevisionText);
        Assert.Equal("MP4 · 1280×720", model.PresetText);
    }

    [Fact]
    public void MutedTracksAreCountedHonestly()
    {
        var export = new ExportView(true, null, Summary(Track("1", 1)) with { AudioTracks = 1, AudioTracksWithClips = 3 }, null);
        Assert.Contains("1 of 3", Model(export).AudioText);
    }

    [Fact]
    public void ProgressShowsOnlyWhatTheEngineReports()
    {
        var model = Model(Ready(Track("1", 2)) with { Job = Job("encoding", 0.5) });
        Assert.Equal(50, model.ProgressPercent);
        Assert.Equal("50%", model.ProgressText);
        Assert.Equal("Encoding", model.StateText);
        Assert.Contains("30 of 90 frames", model.TimingText);
        Assert.Contains("1.5× real time", model.TimingText);
        Assert.Equal("Exporting 50%", model.ButtonText);
        Assert.True(model.Running);
        Assert.False(model.StartCommand.CanExecute(null));
        Assert.True(model.CancelCommand.CanExecute(null));
        Assert.False(model.ChooseOutputCommand.CanExecute(null));
        // Not 100% until the engine says the file is finished.
        model.Apply(View(Ready(Track("1", 2)) with { Job = Job("finalizing", 0.98) }));
        Assert.Equal("98%", model.ProgressText);
    }

    [Fact]
    public void CancellingIsStateTheEngineReports()
    {
        var model = Model(Ready(Track("1", 2)) with { Job = Job("cancelling", 0.2) });
        Assert.Equal("Stopping the export", model.StateText);
        Assert.False(model.CancelCommand.CanExecute(null));
        model.Apply(View(Ready(Track("1", 2)) with { Job = Job("cancelled", 0.2, running: false) }));
        Assert.Equal("Export cancelled", model.StateText);
        Assert.False(model.Running);
        Assert.False(model.OpenResultCommand.CanExecute(null));
    }

    [Fact]
    public void AFinishedExportCanBeOpenedAndItsCaveatsAreStated()
    {
        var result = new ExportResultView(@"C:\o\out.mp4", 5 * 1024 * 1024, 90, 3000, 12, 1.2, 40, 0, 3, 1, 90, 0, 1, 1, 10, 10, 1, 1, 1, 1, 1);
        var model = Model(Ready(Track("1", 2)) with { Job = Job("completed", 1, running: false, result: result) });
        Assert.Equal("100%", model.ProgressText);
        Assert.Contains("5.0 MB", model.ResultText);
        Assert.Contains("clipped 12 samples", model.ResultText);
        Assert.Contains("3 frames had no source picture", model.ResultText);
        Assert.Contains("40 ms", model.ResultText);
        Assert.True(model.OpenResultCommand.CanExecute(null));
        Assert.Equal("Export", model.ButtonText);
    }

    [Fact]
    public void AFailureShowsItsReason()
    {
        var model = Model(Ready(Track("1", 2)) with { Job = Job("failed", 0.3, running: false, error: "Encoding failed: the disk is full.") });
        Assert.True(model.ShowError);
        Assert.Contains("disk is full", model.ErrorText);
        Assert.Equal("Export failed", model.StateText);
        Assert.False(model.OpenResultCommand.CanExecute(null));
    }

    [Fact]
    public void TheExportButtonFollowsAvailabilityAndExplainsItself()
    {
        var model = new ExportViewModel(new EngineHost());
        model.Apply(View(new ExportView(null, null, Summary(Track("1", 1)), null)));
        Assert.False(model.CanOpen);
        Assert.Contains("Checking", model.ButtonTip);
        model.Apply(View(Ready(Track("1", 1))));
        Assert.True(model.CanOpen);
        model.Apply(View(new ExportView(false, "FFmpeg could not be started.", Summary(Track("1", 1)), null)));
        Assert.False(model.CanOpen);
        Assert.Contains("FFmpeg", model.ButtonTip);
        model.Apply(View(Ready(Track("1", 1)), ready: false));
        Assert.False(model.CanOpen);
    }

    [Fact]
    public void SameFiguresDoNotReapplyTheTrackList()
    {
        var model = Model(Ready(Track("1", 2)));
        var changes = 0;
        model.Tracks.CollectionChanged += (_, _) => changes++;
        model.Apply(View(Ready(Track("1", 2))));
        Assert.Equal(0, changes);
    }
}

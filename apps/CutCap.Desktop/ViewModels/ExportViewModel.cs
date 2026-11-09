using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using CutCap.Desktop.Services;
using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>One video track the export can render, as the engine described it.</summary>
public sealed partial class ExportTrackItem : ObservableObject
{
    public ExportTrackItem(ExportTrackChoice choice) => Choice = choice;

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(Label))]
    public partial ExportTrackChoice Choice { get; set; }

    public string Id => Choice.Id;
    public string Name => Choice.Name;
    public string Label => Choice.Clips switch
    {
        0 => $"{Choice.Name} · no clips",
        1 => $"{Choice.Name} · 1 clip",
        var n => $"{Choice.Name} · {n} clips",
    };
    public override string ToString() => Label;
}

/// <summary>
/// The export dialog's presentation state. What is exported, whether it can
/// be, and how far it has got all come from the engine's view; this only holds
/// what the person has chosen so far.
/// </summary>
public sealed partial class ExportViewModel : ObservableObject
{
    private readonly EngineHost _engine;
    private ExportView? _export;
    /// <summary>Permission to replace, granted by the picker's own prompt, for one start.</summary>
    private bool _allowOverwrite;
    private bool _engineReady;

    public ExportViewModel(EngineHost engine) => _engine = engine;

    /// <summary>Set by the window; opens the save picker with a suggested name.</summary>
    public Func<string?, Task<string?>>? PickOutput { get; set; }
    /// <summary>Set by the shell; reveals or opens a finished file.</summary>
    public Action<string>? OpenFile { get; set; }

    public ObservableCollection<ExportTrackItem> Tracks { get; } = [];

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(StartCommand))]
    [NotifyPropertyChangedFor(nameof(OutputName), nameof(OutputFolder), nameof(OutputHint))]
    public partial string OutputPath { get; set; } = "";

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(StartCommand))]
    public partial ExportTrackItem? SelectedTrack { get; set; }

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(StartCommand))]
    public partial bool OmissionAcknowledged { get; set; }

    [ObservableProperty]
    public partial bool Available { get; set; }

    [ObservableProperty]
    public partial string UnavailableText { get; set; } = "Checking this FFmpeg's encoders…";

    [ObservableProperty]
    public partial string PresetName { get; set; } = "";

    [ObservableProperty]
    public partial string PresetText { get; set; } = "";

    [ObservableProperty]
    public partial string DurationText { get; set; } = "";

    [ObservableProperty]
    public partial string FramesText { get; set; } = "";

    [ObservableProperty]
    public partial string AudioText { get; set; } = "";

    [ObservableProperty]
    public partial string RevisionText { get; set; } = "";

    [ObservableProperty]
    public partial string OmissionText { get; set; } = "";

    [ObservableProperty]
    public partial string PictureNote { get; set; } = "";

    [ObservableProperty]
    public partial string BlockedText { get; set; } = "";

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(StartCommand), nameof(CancelCommand))]
    [NotifyPropertyChangedFor(nameof(ShowProgress), nameof(CanEditChoices))]
    public partial ExportJobView? Job { get; set; }

    [ObservableProperty]
    public partial double ProgressPercent { get; set; }

    [ObservableProperty]
    public partial string ProgressText { get; set; } = "";

    [ObservableProperty]
    public partial string StateText { get; set; } = "";

    [ObservableProperty]
    public partial string TimingText { get; set; } = "";

    [ObservableProperty]
    public partial string ResultText { get; set; } = "";

    [ObservableProperty]
    public partial string ErrorText { get; set; } = "";

    [ObservableProperty]
    [NotifyCanExecuteChangedFor(nameof(OpenResultCommand))]
    public partial string? FinishedPath { get; set; }

    public bool Running => Job?.Running == true;
    public bool ShowProgress => Job is not null;
    public bool CanEditChoices => !Running;
    public bool NeedsAcknowledgment => OmissionText.Length > 0;
    public bool ShowResult => ResultText.Length > 0;
    public bool ShowError => ErrorText.Length > 0;
    public bool ShowBlocked => BlockedText.Length > 0;
    public bool ShowPictureNote => PictureNote.Length > 0;
    public bool ShowUnavailable => !Available;
    public string OutputName => Path.GetFileName(OutputPath);
    public string OutputFolder => Path.GetDirectoryName(OutputPath) ?? "";
    public string OutputHint => OutputPath.Length == 0 ? "Choose where to save the MP4." : OutputPath;

    /// <summary>What the main Export button says: its action, or how far the job is.</summary>
    public string ButtonText => Running ? $"Exporting {(int)Math.Floor(ProgressPercent)}%" : "Export";
    public bool CanOpen => _engineReady && (Available || Job is not null);
    public string ButtonTip => Running ? "Show the export in progress"
        : Available ? "Export the timeline to an MP4 file"
        : UnavailableText;

    partial void OnAvailableChanged(bool value)
    {
        OnPropertyChanged(nameof(ShowUnavailable));
        NotifyButton();
        StartCommand.NotifyCanExecuteChanged();
    }

    partial void OnUnavailableTextChanged(string value) => OnPropertyChanged(nameof(ButtonTip));

    partial void OnSelectedTrackChanged(ExportTrackItem? value) => DescribeSelection();

    partial void OnJobChanged(ExportJobView? value)
    {
        OnPropertyChanged(nameof(Running));
        NotifyButton();
    }

    partial void OnOmissionTextChanged(string value)
    {
        OnPropertyChanged(nameof(NeedsAcknowledgment));
        if (value.Length == 0) { OmissionAcknowledged = false; }
        StartCommand.NotifyCanExecuteChanged();
    }

    partial void OnBlockedTextChanged(string value) => OnPropertyChanged(nameof(ShowBlocked));
    partial void OnPictureNoteChanged(string value) => OnPropertyChanged(nameof(ShowPictureNote));
    partial void OnResultTextChanged(string value) => OnPropertyChanged(nameof(ShowResult));
    partial void OnErrorTextChanged(string value) => OnPropertyChanged(nameof(ShowError));

    private void NotifyButton()
    {
        OnPropertyChanged(nameof(ButtonText));
        OnPropertyChanged(nameof(ButtonTip));
        OnPropertyChanged(nameof(CanOpen));
        StateChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>The shell's Export command depends on this.</summary>
    public event EventHandler? StateChanged;

    public void Apply(SessionView view)
    {
        _engineReady = view.EngineReady;
        var export = view.Export;
        if (export is null || export == _export)
        {
            NotifyButton();
            return;
        }
        var previous = _export;
        _export = export;
        Available = export.Available == true;
        UnavailableText = export.Available is null
            ? "Checking this FFmpeg's encoders…"
            : export.UnavailableReason ?? "";
        ApplySummary(export.Summary, previous?.Summary);
        ApplyJob(export.Job);
        NotifyButton();
    }

    private void ApplySummary(ExportSummary summary, ExportSummary? previous)
    {
        PresetName = summary.PresetName;
        PresetText = summary.PresetText;
        RevisionText = $"Project revision {summary.Revision}";
        AudioText = summary.AudioTracksWithClips switch
        {
            0 => "No audio clips: the file has a silent audio track",
            var n when n == summary.AudioTracks => n == 1 ? "1 audio track" : $"{n} audio tracks",
            var n => $"{summary.AudioTracks} of {n} audio tracks are heard (mute and solo apply)",
        };
        if (summary == previous) { return; }
        Sync.Items(Tracks, summary.VideoTracks, item => item.Id, choice => choice.Id,
            choice => new ExportTrackItem(choice), (item, choice) => item.Choice = choice);
        SelectedTrack = Tracks.FirstOrDefault(t => t.Id == SelectedTrack?.Id)
            ?? Tracks.FirstOrDefault(t => t.Id == summary.DefaultTrack)
            ?? Tracks.FirstOrDefault();
        DescribeSelection();
    }

    private void DescribeSelection()
    {
        var choice = SelectedTrack?.Choice;
        DurationText = choice?.DurationText ?? "";
        FramesText = choice is null ? "" : $"{choice.Frames:N0} frames at 30 fps";
        BlockedText = choice?.Blocked ?? (Tracks.Count == 0 ? "This project has no video track." : "");
        var others = Tracks.Where(t => t != SelectedTrack && t.Choice.Clips > 0).Select(t => t.Name).ToArray();
        OmissionText = others.Length == 0 ? "" : others.Length == 1
            ? $"{others[0]} also holds clips and will not be included. Tracks are not composited."
            : $"{string.Join(", ", others)} also hold clips and will not be included. Tracks are not composited.";
        PictureNote = choice is { Clips: 0, Blocked: null }
            ? "This track has no clips, so the picture is black for the whole file."
            : "";
        StartCommand.NotifyCanExecuteChanged();
    }

    private void ApplyJob(ExportJobView? job)
    {
        Job = job;
        if (job is null) { return; }
        ProgressPercent = job.Progress * 100;
        ProgressText = $"{(int)Math.Floor(ProgressPercent)}%";
        StateText = job.State switch
        {
            "validating" => "Checking the project and sources",
            "preparing" => "Mixing audio",
            "decoding" => "Starting the first decoder",
            "encoding" => "Encoding",
            "finalizing" => "Finishing the file",
            "cancelling" => "Stopping the export",
            "completed" => "Export finished",
            "cancelled" => "Export cancelled",
            "failed" => "Export failed",
            var other => other,
        };
        var speed = job.Speed is { } s ? $" · {s:0.0}× real time" : "";
        TimingText = job.State is "encoding" or "decoding" or "finalizing"
            ? $"{job.FramesEncoded:N0} of {job.FramesTotal:N0} frames · {job.ElapsedText}{speed}"
            : $"{job.ElapsedText}{speed}";
        ErrorText = job.Error ?? "";
        FinishedPath = job.Result?.Path;
        ResultText = job.Result is { } result ? Describe(job, result) : "";
    }

    private static string Describe(ExportJobView job, ExportResultView result)
    {
        var size = result.Bytes >= 1024 * 1024
            ? $"{result.Bytes / (1024.0 * 1024.0):0.0} MB"
            : $"{result.Bytes / 1024.0:0} KB";
        var notes = new List<string> { $"{job.FileName} · {size} · {result.Frames:N0} frames" };
        if (result.ClippedSamples > 0) { notes.Add($"The audio mix clipped {result.ClippedSamples:N0} samples."); }
        if (result.NoPictureFrames > 0) { notes.Add($"{result.NoPictureFrames:N0} frames had no source picture and are black."); }
        if (result.SourceSilenceMs > 0) { notes.Add($"{result.SourceSilenceMs:N0} ms of clips had no source audio."); }
        return string.Join("\n", notes);
    }

    [RelayCommand(CanExecute = nameof(CanEdit))]
    private async Task ChooseOutputAsync()
    {
        if (PickOutput is null) { return; }
        var picked = await PickOutput(OutputPath.Length > 0 ? OutputName : SuggestedName());
        if (picked is null) { return; }
        OutputPath = picked;
        // The picker has already asked whether an existing file may be replaced.
        _allowOverwrite = true;
    }

    private bool CanEdit() => !Running;

    private string SuggestedName()
    {
        var project = _engine.View?.Project?.Name;
        return $"{(string.IsNullOrWhiteSpace(project) ? "Export" : project)}.mp4";
    }

    private bool CanStart() => Available && !Running && OutputPath.Length > 0 && SelectedTrack is { Choice.Blocked: null }
        && (!NeedsAcknowledgment || OmissionAcknowledged);

    [RelayCommand(CanExecute = nameof(CanStart))]
    private void Start()
    {
        var (path, track, replace, acknowledged) = (OutputPath, SelectedTrack?.Id, _allowOverwrite, OmissionAcknowledged);
        // One permission, one start: a repeat must be chosen again.
        _allowOverwrite = false;
        _engine.Run(session => session.StartExport(path, track, replace, acknowledged));
    }

    private bool CanCancel() => Running && Job?.State != "cancelling";

    [RelayCommand(CanExecute = nameof(CanCancel))]
    private void Cancel()
    {
        if (Job is { } job) { _engine.Run(session => session.CancelExport(job.Id)); }
    }

    private bool CanOpenResult() => FinishedPath is not null;

    [RelayCommand(CanExecute = nameof(CanOpenResult))]
    private void OpenResult()
    {
        if (FinishedPath is { } path) { OpenFile?.Invoke(path); }
    }
}

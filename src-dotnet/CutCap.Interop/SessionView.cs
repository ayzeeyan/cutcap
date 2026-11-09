using System.Text.Json.Serialization;

namespace CutCap.Interop;

// Mirrors crates/cutcap-ffi/src/view.rs. Field names are snake_case on the wire.
// Times and rational values arrive already formatted by Rust; the frontend
// renders them and never re-derives source time from floating point.

public sealed record SessionView(
    ulong Revision,
    bool EngineReady,
    ProjectView? Project,
    StatusView Status,
    uint Pending,
    IReadOnlyList<TrackView> Tracks,
    IReadOnlyList<MediaView> Media,
    ImportView Import,
    InspectorView Inspector,
    SourceView Source,
    FrameView? Frame,
    PreviewMetrics Metrics,
    PlaybackView? Playback = null,
    ulong TimelineGeneration = 0,
    HistoryStatus? History = null,
    PreviewMode PreviewMode = PreviewMode.Source,
    TimelinePreviewView? TimelinePreview = null,
    TimelinePlaybackView? TimelinePlayback = null,
    EditingView? Editing = null,
    ThumbnailStats? Thumbnails = null,
    AudioView? Audio = null,
    WaveformStats? Waveforms = null,
    ExportView? Export = null);

/// <summary>Phase 5A (ABI 11): what Export can do and how the current job is going.</summary>
public sealed record ExportView(bool? Available, string? UnavailableReason, ExportSummary Summary, ExportJobView? Job);

public sealed record ExportSummary(
    string PresetName, string PresetText, IReadOnlyList<ExportTrackChoice> VideoTracks, string? DefaultTrack,
    int AudioTracks, int AudioTracksWithClips, ulong Revision)
{
    public bool Equals(ExportSummary? other) =>
        other is not null && PresetName == other.PresetName && PresetText == other.PresetText
        && DefaultTrack == other.DefaultTrack && AudioTracks == other.AudioTracks
        && AudioTracksWithClips == other.AudioTracksWithClips && Revision == other.Revision
        && VideoTracks.SequenceEqual(other.VideoTracks);
    public override int GetHashCode() => HashCode.Combine(PresetName, DefaultTrack, AudioTracks, Revision, VideoTracks.Count);
}

public sealed record ExportTrackChoice(
    string Id, string Name, int Clips, long DurationMs, string DurationText, ulong Frames, string? Blocked);

public sealed record ExportJobView(
    string Id, string State, bool Running, double Progress, string Detail, string OutputPath, string FileName,
    string ProjectName, ulong Revision, string? TrackName, ulong FramesTotal, ulong FramesSubmitted, ulong FramesEncoded,
    ulong SamplesTotal, ulong SamplesMixed, ulong ElapsedMs, string ElapsedText, double? Speed, string? Error,
    ExportResultView? Result);

public sealed record ExportResultView(
    string Path, ulong Bytes, ulong Frames, ulong DurationMs, ulong ClippedSamples, double Peak, ulong SourceSilenceMs,
    ulong GapFrames, ulong NoPictureFrames, ulong HeadFrames, ulong DecodedFrames, ulong ReusedFrames,
    ulong VideoDecoderStarts, ulong ClipChanges, ulong CutWaitMaxUs, ulong CutWaitTotalUs, ulong AudioDecoderStarts,
    int AudioDecodersPeak, ulong MixUs, ulong EncodeUs, ulong TotalUs);

/// <summary>
/// Phase 4B audio output and master-clock state (ABI 9). <c>Clock</c> is
/// "audio" when the output device clocks timeline playback, else "system".
/// </summary>
public sealed record AudioView(
    string State, string Clock, AudioOutputStatus Output, ulong Epoch, int Decoders, ulong DecoderStarts,
    double SourceSilenceMs, ulong SilentClips, string? Detail, MixView? Mix = null);

/// <summary>Mixer diagnostics for the current epoch (ABI 10); never project data.</summary>
public sealed record MixView(
    int ActiveTracks, int MixedTracks, int MixedTracksPeak, int DecodersPeak, ulong ClippedSamples, float Peak,
    ulong OverLimit, ulong Blocks, ulong BlockUsLast, ulong BlockUsMax, ulong BlockUsTotal, double MixedMs);

public sealed record AudioOutputStatus(
    string State, string Device, uint Rate, double StreamLatencyMs, double LatencyCompensationMs, double BufferedMs, ulong Underruns,
    double PaddedMs, double PlayedMs, string? Error, ulong PeriodUsLast = 0, ulong PeriodUsMax = 0, ulong Periods = 0);

/// <summary>Waveform worker diagnostics (ABI 9); never project data.</summary>
public sealed record WaveformStats(
    int Pending, int InFlight, int Waiting, ulong Analysed, ulong Failed,
    ulong Batches, ulong CancelledBatches, ulong? LastBatchMs, uint? LastProcessId, bool Throttled);

/// <summary>
/// One waveform chunk the frontend wants: chunk <c>Chunk</c> of audio stream
/// <c>Stream</c> of a verified source (<c>AudioKey</c> from the clip row).
/// </summary>
public sealed record WaveformWant(ulong Token, string MediaId, string AudioKey, uint Stream, long Chunk);

/// <summary>Filmstrip worker diagnostics (ABI 8); never project data.</summary>
public sealed record ThumbnailStats(
    int Pending, int InFlight, int Waiting, ulong Decoded, ulong NoFrame, ulong Failed,
    ulong Batches, ulong CancelledBatches, ulong? LastBatchMs, uint? LastProcessId, bool Throttled);

/// <summary>
/// One filmstrip sample the frontend wants. <c>SampleUs</c> is a point on the
/// source's microsecond axis chosen by the layout; Rust maps it to the stream
/// clock (ceiling), validates it and picks the actual frame.
/// </summary>
public sealed record ThumbnailWant(ulong Token, string MediaId, string SourceKey, long SampleUs, int Height);

/// <summary>Editing affordances that depend on Rust-owned time (ABI 7).</summary>
public sealed record EditingView(bool SplitEnabled, string SplitDetail, string? Notice = null,
    bool CanUnlink = false, string? LinkCandidate = null);

/// <summary>Project-tick clock and copied frame identity; no clip semantics or pixels in managed state.</summary>
public sealed record TimelinePlaybackView(
    PlaybackState State, bool DesiredPlaying, string? Epoch, long RequestedTicks, string RequestedText, long ClockTicks, long ClockMs, string ClockText,
    long? OfferedTicks, long? CopiedTicks, SourceTimestamp? OfferedPts, SourceTimestamp? CopiedPts, string? CopiedClipId,
    ulong Decoded, ulong Offered, ulong Copied, ulong Dropped, ulong BufferStalls, ulong Handoffs, ulong DecoderStarts,
    int Buffered, uint? ProcessId, uint? PreparedProcessId, string? Error, ulong? RequestToReadyUs,
    string ClockSource = "system", ulong VideoLate = 0, double? AvOffsetMs = null, double AvOffsetMaxMs = 0, ulong AvSamples = 0);

[JsonConverter(typeof(JsonStringEnumConverter<PreviewMode>))]
public enum PreviewMode { Source, Timeline }

/// <summary>Transient Rust evaluation/preview, independent of source transport and the clip-list document.</summary>
public sealed record TimelinePreviewView(
    long PositionTicks, RationalTimebase Timebase, long PositionMs, string PositionText, string PositionInput,
    long SeekMaxMs, long ExtentMs, long ExtentTicks, string ExtentText,
    string? TrackId, string TrackName, string Title, string State, string Detail, bool Busy, bool HasFrame,
    string? ClipId, string? MediaId, string SourceName, long? ClipStartTicks, long? ClipEndTicks,
    SourceTimestamp? Requested, SourceTimestamp? Actual, string Caption, string? EvaluationGeneration,
    ulong? EvaluationUs, ulong? DecodeUs, ulong? RequestToReadyUs, uint? ProcessId, ulong FramesCopied);

/// <summary>Rust-owned clip-edit history; labels describe the next undo/redo.</summary>
public sealed record HistoryStatus(bool CanUndo, bool CanRedo, string? Undo, string? Redo, int Depth,
    bool UndoTrack = false, bool RedoTrack = false);

/// <summary>The clip list, fetched separately when <see cref="SessionView.TimelineGeneration"/> changes.</summary>
public sealed record TimelineView(ulong Generation, IReadOnlyList<ClipView> Clips);

/// <summary>
/// One clip. <c>StartMs</c>/<c>DurationMs</c> are floored from exact project
/// ticks for drawing only; the text fields are the authoritative readouts.
/// </summary>
public sealed record ClipView(
    string Id, string TrackId, int TrackIndex, string MediaId, string Name,
    long StartMs, long DurationMs, string StartText, string DurationText, string EndText,
    string Availability, bool Selected,
    long StartTicks = 0, long EndTicks = 0, long MinStartTicks = 0, long MinStartMs = 0,
    long? MaxEndTicks = null, long? MaxEndMs = null,
    [property: JsonPropertyName("end_ms")] long? ExactEndMs = null,
    string SourceKey = "", long SourceInTicks = 0, long SourceEndTicks = 0,
    long SourceInUs = 0, long SourceEndUs = 0, double ThumbAspect = 0,
    string Stream = "video", uint? AudioStream = null, string? LinkId = null,
    string AudioKey = "", uint AudioRate = 0, long SourceInSamples = 0, long SourceEndSamples = 0)
{
    [JsonIgnore]
    public bool IsAudio => Stream == "audio";
    /// <summary>Floor of the exact end; older/test rows fall back to start + duration.</summary>
    [JsonIgnore]
    public long EndMs => ExactEndMs ?? StartMs + DurationMs;
    public bool SourceReady => Availability == "ready";
}

public sealed record ProjectView(string Id, string Name, bool Saved, int ClipCount, DocumentStatus? Document = null);

public sealed record DocumentStatus(string? Path, ulong Revision, ulong? SavedRevision, bool Dirty,
    string? OperationId, string State, string? Error)
{
    public bool Busy => State is "saving" or "opening";
}

public sealed record StatusView(string Text, bool Error);

public sealed record TrackView(string Id, string Name, string Kind, string Badge, bool Selected, string LaneLabel = "",
    int GainCdb = 0, string GainText = "0.0 dB", bool Muted = false, bool Soloed = false, bool Audible = true)
{
    public bool IsVideo => Kind == "video";
}

public sealed record MediaView(
    string Id,
    string Name,
    string Resolution,
    string Format,
    string Duration,
    long? DurationMs,
    bool Estimated,
    bool HasAudio,
    bool Selected,
    string Availability = "ready", string Detail = "", string Path = "",
    bool AudioOnly = false, IReadOnlyList<AudioStreamView>? AudioStreams = null);

/// <summary>One decodable audio stream of a source, for explicit placement.</summary>
public sealed record AudioStreamView(uint Index, string Label, bool Primary);

public sealed record ImportView(bool Busy, bool Error, string Message);

public sealed record InspectorView(string Kind, string Title, string Detail, IReadOnlyList<SectionView> Sections, string Badge = "");

public sealed record SectionView(string Title, IReadOnlyList<RowView> Rows);

public sealed record RowView(string Label, string Value);

public sealed record SourceView(
    bool Ready,
    string Title,
    long PositionMs,
    long DurationMs,
    bool DurationEstimated,
    bool Busy,
    bool HasFrame,
    string Status,
    string Detail,
    string Caption);

public sealed record FrameView(ulong Generation, uint Width, uint Height,
    SourceTimestamp? Actual = null, string? Epoch = null);

public sealed record RationalTimebase(uint Numerator, uint Denominator);
public sealed record SourceTimestamp(long Ticks, RationalTimebase Timebase);

[JsonConverter(typeof(JsonStringEnumConverter<PlaybackState>))]
public enum PlaybackState { Stopped, Starting, Playing, Paused, Seeking, Buffering, Ended, Failed }

/// <summary>Source-only transport. Copied time acknowledges pixel delivery, not physical scanout.</summary>
public sealed record PlaybackView(
    PlaybackState State, bool DesiredPlaying, string? Epoch, long RequestedMs,
    ulong ClockMs, ulong? OfferedMs, ulong? CopiedMs,
    SourceTimestamp? OfferedPts, SourceTimestamp? CopiedPts,
    ulong Decoded, ulong Offered, ulong Copied, ulong Dropped, ulong BufferStalls,
    int Buffered, uint? ProcessId, string? Error,
    string RequestedText, string CopiedText, string ClockText, ulong? RequestToReadyUs);

public sealed record PreviewMetrics(ulong? ProbeUs, ulong? DecodeUs, ulong? RequestToReadyUs, ulong FramesPresented);

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    UnmappedMemberHandling = JsonUnmappedMemberHandling.Skip)]
[JsonSerializable(typeof(SessionView))]
[JsonSerializable(typeof(TimelineView))]
[JsonSerializable(typeof(IReadOnlyList<ThumbnailWant>))]
[JsonSerializable(typeof(IReadOnlyList<WaveformWant>))]
internal sealed partial class ViewJsonContext : JsonSerializerContext;

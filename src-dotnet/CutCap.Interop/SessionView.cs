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
    HistoryStatus? History = null);

/// <summary>Rust-owned clip-edit history; labels describe the next undo/redo.</summary>
public sealed record HistoryStatus(bool CanUndo, bool CanRedo, string? Undo, string? Redo, int Depth);

/// <summary>The clip list, fetched separately when <see cref="SessionView.TimelineGeneration"/> changes.</summary>
public sealed record TimelineView(ulong Generation, IReadOnlyList<ClipView> Clips);

/// <summary>
/// One clip. <c>StartMs</c>/<c>DurationMs</c> are floored from exact project
/// ticks for drawing only; the text fields are the authoritative readouts.
/// </summary>
public sealed record ClipView(
    string Id, string TrackId, int TrackIndex, string MediaId, string Name,
    long StartMs, long DurationMs, string StartText, string DurationText, string EndText,
    string Availability, bool Selected)
{
    public long EndMs => StartMs + DurationMs;
    public bool SourceReady => Availability == "ready";
}

public sealed record ProjectView(string Id, string Name, bool Saved, int ClipCount, DocumentStatus? Document = null);

public sealed record DocumentStatus(string? Path, ulong Revision, ulong? SavedRevision, bool Dirty,
    string? OperationId, string State, string? Error)
{
    public bool Busy => State is "saving" or "opening";
}

public sealed record StatusView(string Text, bool Error);

public sealed record TrackView(string Id, string Name, string Kind, string Badge, bool Selected, string LaneLabel = "")
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
    string Availability = "ready", string Detail = "", string Path = "");

public sealed record ImportView(bool Busy, bool Error, string Message);

public sealed record InspectorView(string Kind, string Title, string Detail, IReadOnlyList<SectionView> Sections);

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
internal sealed partial class ViewJsonContext : JsonSerializerContext;

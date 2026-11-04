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
    PreviewMetrics Metrics);

public sealed record ProjectView(string Id, string Name, bool Saved, int ClipCount);

public sealed record StatusView(string Text, bool Error);

public sealed record TrackView(string Id, string Name, string Kind, string Badge, bool Selected)
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
    bool Selected);

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

public sealed record FrameView(ulong Generation, uint Width, uint Height);

public sealed record PreviewMetrics(ulong? ProbeUs, ulong? DecodeUs, ulong? RequestToReadyUs, ulong FramesPresented);

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.SnakeCaseLower,
    UnmappedMemberHandling = JsonUnmappedMemberHandling.Skip)]
[JsonSerializable(typeof(SessionView))]
internal sealed partial class ViewJsonContext : JsonSerializerContext;

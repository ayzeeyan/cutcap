using Avalonia;
using CutCap.Interop;

namespace CutCap.Desktop.Controls;

public enum ClipZone { Body, StartHandle, EndHandle }

public enum GestureKind { Move, TrimStart, TrimEnd, Place }

/// <summary>
/// What a drag would do if released now: a frontend-only preview. Nothing in
/// the project changes until the engine accepts the committed edit; the
/// commit carries either an exact tick boundary (snapped or clamped) or whole
/// milliseconds that Rust floors to ticks.
/// </summary>
public sealed record GesturePreview(
    GestureKind Kind, string? ClipId, string? MediaId, int TrackIndex, string? TrackId,
    double StartMs, double EndMs, bool Valid, string Reason, SnapTarget? Snap,
    TimelinePosition Position, ClipEdge Anchor, bool Changed);

/// <summary>
/// Pure gesture math shared by the lanes control and its tests. Pointer
/// positions are display milliseconds (doubles) for the preview only.
/// </summary>
public static class TimelineGestures
{
    /// <summary>Pointer travel (px) before a press on a clip body becomes a move.</summary>
    public const double DragThreshold = 4;

    /// <summary>Trim handles: up to 8 px inside each edge, a quarter of the clip at most, never below 3 px.</summary>
    public static double HandleWidth(double clipWidth) => Math.Clamp(clipWidth / 4, 3, 8);

    public static ClipZone Zone(Rect clip, double x)
    {
        var handle = HandleWidth(clip.Width);
        if (x <= clip.Left + handle) return ClipZone.StartHandle;
        if (x >= clip.Right - handle) return ClipZone.EndHandle;
        return ClipZone.Body;
    }

    private static string Text(double ms) => TimeSpan.FromMilliseconds(Math.Max(0, ms)).ToString(@"mm\:ss\.fff");

    private static (bool Ok, string? TrackId, string Reason) Lane(IReadOnlyList<LaneInfo> lanes, int track, bool audio = false) =>
        track < 0 || track >= lanes.Count ? (false, null, audio ? "Drop on an audio track" : "Drop on a video track")
        : audio && lanes[track].IsVideo ? (false, lanes[track].Id, "Audio clips go on an audio track")
        : !audio && !lanes[track].IsVideo ? (false, lanes[track].Id, "Video clips go on a video track")
        : (true, lanes[track].Id, "");

    public static GesturePreview Move(
        TimelineGeometry geometry, IReadOnlyList<LaneInfo> lanes, ClipView clip, double pointerMs, double grabMs,
        int track, double thresholdMs, TimelineSnapper snapper, SnapTarget? playhead, bool snap)
    {
        var duration = clip.EndMs - clip.StartMs;
        var start = Math.Max(0, pointerMs - grabMs);
        var position = TimelinePosition.Milliseconds((long)Math.Round(start));
        var anchor = ClipEdge.Start;
        SnapTarget? snapped = null;
        if (snap)
        {
            var head = snapper.Snap(start, thresholdMs, clip.Id, playhead);
            var tail = snapper.Snap(start + duration, thresholdMs, clip.Id, playhead);
            if (head is { } h && (tail is null || Math.Abs(h.Ms - start) <= Math.Abs(tail.Value.Ms - start - duration)))
            {
                (start, position, snapped) = (h.Ms, TimelinePosition.Ticks(h.Ticks), h);
            }
            else if (tail is { } t && t.Ms - duration >= 0)
            {
                (start, position, anchor, snapped) = (t.Ms - duration, TimelinePosition.Ticks(t.Ticks), ClipEdge.End, t);
            }
        }
        else
        {
            start = Math.Round(start);
        }
        var end = start + duration;
        var (ok, trackId, reason) = Lane(lanes, track, clip.IsAudio);
        if (ok && geometry.Overlaps(track, start, end, clip.Id))
        {
            (ok, reason) = (false, "Clips cannot overlap");
        }
        var changed = track != clip.TrackIndex
            || (snapped is { } s ? (anchor == ClipEdge.Start ? s.Ticks != clip.StartTicks : s.Ticks != clip.EndTicks) : Math.Abs(start - clip.StartMs) >= 1);
        return new GesturePreview(GestureKind.Move, clip.Id, null, track, trackId, start, end, ok,
            ok ? $"Move to {Text(start)}" : reason, snapped, position, anchor, changed);
    }

    /// <summary>Trim one edge, clamped to the source range and the neighbouring clips.</summary>
    public static GesturePreview Trim(
        TimelineGeometry geometry, ClipView clip, ClipEdge edge, double pointerMs, double thresholdMs,
        TimelineSnapper snapper, SnapTarget? playhead, bool snap)
    {
        SnapTarget? snapped = snap ? snapper.Snap(pointerMs, thresholdMs, clip.Id, playhead) : null;
        var value = snapped?.Ms ?? Math.Round(pointerMs);
        TimelinePosition position = snapped is { } s ? TimelinePosition.Ticks(s.Ticks) : TimelinePosition.Milliseconds((long)value);
        string limit = "";
        if (edge == ClipEdge.Start)
        {
            // Earliest: the source's first frame (on its tick grid) or the previous clip's end.
            var previous = geometry.Previous(clip.TrackIndex, clip.StartMs, clip.Id);
            var (lowMs, lowTicks, lowWhy) = previous is { } p && p.EndTicks > clip.MinStartTicks
                ? ((double)p.EndMs, p.EndTicks, "previous clip")
                : ((double)clip.MinStartMs, clip.MinStartTicks, "start of the source");
            var high = clip.EndMs - 1.0;
            if (value <= lowMs) { (value, position, snapped, limit) = (lowMs, TimelinePosition.Ticks(lowTicks), null, lowWhy); }
            else if (value >= high) { (value, position, snapped, limit) = (high, TimelinePosition.Milliseconds((long)high), null, "minimum length"); }
            var changed = position.IsTicks ? position.Value != clip.StartTicks : Math.Abs(value - clip.StartMs) >= 1;
            return new GesturePreview(GestureKind.TrimStart, clip.Id, null, clip.TrackIndex, clip.TrackId, value, clip.EndMs,
                true, limit.Length > 0 ? $"Start {Text(value)} (at the {limit})" : $"Start {Text(value)}",
                snapped, position, ClipEdge.Start, changed);
        }
        else
        {
            var next = geometry.Next(clip.TrackIndex, clip.EndMs, clip.Id);
            var sourceEnd = clip.MaxEndTicks is { } maxTicks ? ((double)(clip.MaxEndMs ?? clip.EndMs), maxTicks) : (clip.EndMs, clip.EndTicks);
            var (highMs, highTicks, highWhy) = next is { } n && n.StartTicks < sourceEnd.Item2
                ? ((double)n.StartMs, n.StartTicks, "next clip")
                : (sourceEnd.Item1, sourceEnd.Item2, "end of the source");
            var low = clip.StartMs + 1.0;
            if (value >= highMs) { (value, position, snapped, limit) = (highMs, TimelinePosition.Ticks(highTicks), null, highWhy); }
            else if (value <= low) { (value, position, snapped, limit) = (low, TimelinePosition.Milliseconds((long)low), null, "minimum length"); }
            var changed = position.IsTicks ? position.Value != clip.EndTicks : Math.Abs(value - clip.EndMs) >= 1;
            return new GesturePreview(GestureKind.TrimEnd, clip.Id, null, clip.TrackIndex, clip.TrackId, clip.StartMs, value,
                true, limit.Length > 0 ? $"End {Text(value)} (at the {limit})" : $"End {Text(value)}",
                snapped, position, ClipEdge.End, changed);
        }
    }

    /// <summary>Library drop: the whole source starting at the pointer (start edge snaps).</summary>
    public static GesturePreview Place(
        TimelineGeometry geometry, IReadOnlyList<LaneInfo> lanes, MediaDrag media, double pointerMs, int track,
        double thresholdMs, TimelineSnapper snapper, SnapTarget? playhead, bool snap)
    {
        var start = Math.Max(0, pointerMs);
        SnapTarget? snapped = snap ? snapper.Snap(start, thresholdMs, null, playhead) : null;
        start = snapped?.Ms ?? Math.Round(start);
        var position = snapped is { } s ? TimelinePosition.Ticks(s.Ticks) : TimelinePosition.Milliseconds((long)start);
        var end = start + Math.Max(1, media.DurationMs ?? 0);
        var (ok, trackId, reason) = Lane(lanes, track);
        if (ok && !media.Ready) (ok, reason) = (false, $"{media.Name} is unavailable");
        else if (ok && media.DurationMs is not > 0) (ok, reason) = (false, $"{media.Name} has no known duration");
        else if (ok && geometry.Overlaps(track, start, end, null)) (ok, reason) = (false, "Clips cannot overlap");
        return new GesturePreview(GestureKind.Place, null, media.Id, track, trackId, start, end, ok,
            ok ? $"Place {media.Name} at {Text(start)}" : reason, snapped, position, ClipEdge.Start, true);
    }
}

/// <summary>The in-process payload of a library drag (no file paths or pixels).</summary>
public sealed record MediaDrag(string Id, string Name, long? DurationMs, bool Ready);

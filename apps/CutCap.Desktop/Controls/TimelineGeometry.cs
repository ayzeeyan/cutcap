using Avalonia;
using CutCap.Interop;

namespace CutCap.Desktop.Controls;

/// <summary>
/// Pure clip layout for the lanes: per-track clip lists sorted by start, so the
/// visible clips of a viewport are found by binary search and drawing cost
/// follows what is on screen rather than the clip count. Positions use the
/// engine's display milliseconds (floored from exact ticks); nothing here
/// feeds time back into the project.
/// </summary>
public sealed class TimelineGeometry
{
    public const double LaneInset = 5;
    /// <summary>Clips shorter than this many pixels are still drawn and clickable.</summary>
    public const double MinClipWidth = 3;

    private readonly ClipView[][] _byTrack;

    public static readonly TimelineGeometry Empty = new([], 0);

    public TimelineGeometry(IReadOnlyList<ClipView> clips, int trackCount)
    {
        var lanes = new List<ClipView>[Math.Max(0, trackCount)];
        for (var i = 0; i < lanes.Length; i++) { lanes[i] = []; }
        long end = 0;
        foreach (var clip in clips)
        {
            end = Math.Max(end, clip.EndMs);
            if (clip.TrackIndex >= 0 && clip.TrackIndex < lanes.Length)
            {
                lanes[clip.TrackIndex].Add(clip);
            }
        }
        _byTrack = [.. lanes.Select(lane => lane.OrderBy(c => c.StartMs).ToArray())];
        EndMs = end;
        Count = clips.Count;
    }

    /// <summary>The latest clip end on any track, in display milliseconds.</summary>
    public long EndMs { get; }
    public int Count { get; }
    public int TrackCount => _byTrack.Length;

    public IReadOnlyList<ClipView> Lane(int track) => track >= 0 && track < _byTrack.Length ? _byTrack[track] : [];

    public static Rect ClipRect(TimelineScale scale, ClipView clip, double trackHeight)
    {
        var left = scale.TimeToX(clip.StartMs);
        var width = Math.Max(MinClipWidth, scale.TimeToX(clip.EndMs) - left);
        return new Rect(left, clip.TrackIndex * trackHeight + LaneInset, width, trackHeight - 2 * LaneInset);
    }

    /// <summary>Clips of one track that intersect content x in [left, right], in start order.</summary>
    public IEnumerable<ClipView> Visible(int track, TimelineScale scale, double left, double right)
    {
        var lane = Lane(track);
        // Clips on one track never overlap (engine invariant), so ends are sorted too.
        var from = LowerBound(lane, scale.XToTime(left) - MinClipWidth / scale.PixelsPerSecond * 1000);
        for (var i = from; i < lane.Count; i++)
        {
            var clip = lane[i];
            if (scale.TimeToX(clip.StartMs) > right) { yield break; }
            yield return clip;
        }
    }

    /// <summary>The clip under a content-space point, if any.</summary>
    public ClipView? HitTest(TimelineScale scale, Point point, double trackHeight)
    {
        var track = (int)Math.Floor(point.Y / trackHeight);
        foreach (var clip in Visible(track, scale, point.X, point.X))
        {
            if (ClipRect(scale, clip, trackHeight).Contains(point)) { return clip; }
        }
        return null;
    }

    /// <summary>
    /// Whether [startMs, endMs) on a track intersects any clip other than
    /// <paramref name="except"/>. Binary search: cost follows the overlap, not the track.
    /// </summary>
    public bool Overlaps(int track, double startMs, double endMs, string? except)
    {
        var lane = Lane(track);
        for (var i = LowerBound(lane, startMs); i < lane.Count && lane[i].StartMs < endMs; i++)
        {
            if (lane[i].Id != except && lane[i].EndMs > startMs) return true;
        }
        return false;
    }

    /// <summary>The nearest clip on the track ending at or before <paramref name="ms"/> (excluding one clip).</summary>
    public ClipView? Previous(int track, double ms, string? except)
    {
        var lane = Lane(track);
        var i = LowerBound(lane, ms) - 1;
        while (i >= 0 && (lane[i].Id == except || lane[i].EndMs > ms)) i--;
        return i >= 0 ? lane[i] : null;
    }

    /// <summary>The nearest clip on the track starting at or after <paramref name="ms"/> (excluding one clip).</summary>
    public ClipView? Next(int track, double ms, string? except)
    {
        var lane = Lane(track);
        for (var i = LowerBound(lane, ms); i < lane.Count; i++)
        {
            if (lane[i].Id != except && lane[i].StartMs >= ms) return lane[i];
        }
        return null;
    }

    /// <summary>First index whose end is after <paramref name="ms"/>.</summary>
    private static int LowerBound(IReadOnlyList<ClipView> lane, double ms)
    {
        int low = 0, high = lane.Count;
        while (low < high)
        {
            var mid = (low + high) / 2;
            if (lane[mid].EndMs <= ms) { low = mid + 1; } else { high = mid; }
        }
        return low;
    }
}

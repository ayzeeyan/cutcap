namespace CutCap.Desktop.Controls;

/// <summary>A snap target: an exact project-tick boundary plus its display millisecond.</summary>
public readonly record struct SnapTarget(double Ms, long Ticks, string? ClipId);

/// <summary>
/// Snapping is a user-interface assistance policy, not a second time model:
/// it only chooses one of the engine's exact boundaries (timeline zero, the
/// playhead, clip starts and ends) when the pointer is within a pixel
/// threshold, and the edit is then committed with that exact tick value.
/// Clip edges are kept sorted once per clip-list change, so each pointer move
/// is a binary search, not a scan.
/// </summary>
public sealed class TimelineSnapper
{
    /// <summary>Distance in device-independent pixels within which an edge snaps.</summary>
    public const double ThresholdPixels = 8;

    private readonly SnapTarget[] _edges;

    public TimelineSnapper(IEnumerable<Interop.ClipView> clips)
    {
        _edges = clips
            .SelectMany(c => new[] { new SnapTarget(c.StartMs, c.StartTicks, c.Id), new SnapTarget(c.EndMs, c.EndTicks, c.Id) })
            .OrderBy(t => t.Ms)
            .ToArray();
    }

    public static readonly TimelineSnapper Empty = new([]);

    public int Count => _edges.Length;

    /// <summary>
    /// The nearest target to <paramref name="ms"/> within <paramref name="thresholdMs"/>,
    /// ignoring the edges of <paramref name="exceptClip"/> (the clip being edited).
    /// Zero and the playhead are always candidates.
    /// </summary>
    public SnapTarget? Snap(double ms, double thresholdMs, string? exceptClip, SnapTarget? playhead)
    {
        SnapTarget? best = null;
        void Consider(SnapTarget candidate)
        {
            var distance = Math.Abs(candidate.Ms - ms);
            if (distance <= thresholdMs && (best is null || distance < Math.Abs(best.Value.Ms - ms)))
            {
                best = candidate;
            }
        }
        Consider(new SnapTarget(0, 0, null));
        if (playhead is { } head) Consider(head);
        // Walk outward from the insertion point while still inside the threshold.
        var index = LowerBound(ms - thresholdMs);
        for (var i = index; i < _edges.Length && _edges[i].Ms <= ms + thresholdMs; i++)
        {
            if (_edges[i].ClipId != exceptClip) Consider(_edges[i]);
        }
        return best;
    }

    private int LowerBound(double ms)
    {
        int low = 0, high = _edges.Length;
        while (low < high)
        {
            var mid = (low + high) / 2;
            if (_edges[mid].Ms < ms) low = mid + 1; else high = mid;
        }
        return low;
    }
}

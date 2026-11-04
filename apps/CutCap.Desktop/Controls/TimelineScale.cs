namespace CutCap.Desktop.Controls;

/// <summary>
/// View-only mapping between timeline milliseconds and device-independent
/// pixels. Authoritative timeline time stays in Rust ticks; this never feeds
/// back into the project. Pure, so it is unit-tested without rendering.
/// </summary>
public readonly record struct TimelineScale(double PixelsPerSecond, double Origin)
{
    public const double BasePixelsPerSecond = 20;
    public const int MinZoom = 50;
    public const int MaxZoom = 200;
    public const int ZoomStep = 25;
    /// <summary>The ruler always shows at least this range, independent of project length.</summary>
    public const double MinimumViewSeconds = 60;

    private static readonly int[] LabelSteps = [1, 2, 3, 5, 10, 15, 30, 60, 120, 300, 600];

    public static TimelineScale ForZoom(int zoomPercent, double origin = 12) =>
        new(BasePixelsPerSecond * Math.Clamp(zoomPercent, MinZoom, MaxZoom) / 100.0, origin);

    public double TimeToX(double milliseconds) => Origin + milliseconds / 1000.0 * PixelsPerSecond;

    public double XToTime(double x) => Math.Max(0, (x - Origin) / PixelsPerSecond * 1000.0);

    /// <summary>Content width covering the minimum view range or the viewport, whichever is wider.</summary>
    public double ContentWidth(double viewportWidth) =>
        Math.Max(viewportWidth, TimeToX(MinimumViewSeconds * 1000) + 32);

    /// <summary>Smallest label interval (seconds) that keeps labels at least <paramref name="minimumGap"/> px apart.</summary>
    public int LabelStepSeconds(double minimumGap = 56)
    {
        foreach (var step in LabelSteps)
        {
            if (step * PixelsPerSecond >= minimumGap)
            {
                return step;
            }
        }
        return LabelSteps[^1];
    }

    /// <summary>Seconds whose ticks fall inside [left, right] of content space.</summary>
    public (int First, int Last) VisibleSeconds(double left, double right)
    {
        var first = (int)Math.Max(0, Math.Floor((left - Origin) / PixelsPerSecond));
        var last = (int)Math.Ceiling((right - Origin) / PixelsPerSecond);
        return (first, Math.Max(first, last));
    }

    public static string FormatSeconds(int total) => $"{total / 60:00}:{total % 60:00}";
}

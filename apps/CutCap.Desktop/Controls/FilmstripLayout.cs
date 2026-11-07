using CutCap.Interop;

namespace CutCap.Desktop.Controls;

/// <summary>Cache identity of one decoded thumbnail: verified source, sample point, pixel height.</summary>
public readonly record struct ThumbKey(string Source, long SampleUs, int Height);

/// <summary>One filmstrip cell: display x/width (DIPs, content space) and its source sample.</summary>
public readonly record struct FilmCell(double X, double Width, long SampleUs, bool Visible);

/// <summary>
/// Pure filmstrip layout (Phase 4A). Cells sit on a fixed source-time grid:
/// one cell spans <c>cellWidth / pixelsPerSecond</c> of source time and its
/// sample is the cell's left edge, so for a given zoom the same source always
/// uses the same samples. Scrolling, moving a clip or splitting it therefore
/// reuses decoded thumbnails, and a split's parts share them. The first,
/// partial cell of a clip samples the clip's own first source tick, so no cell
/// ever samples before the clip's in-point. Positions are display-only
/// doubles; Rust maps each sample to the stream clock and picks the frame.
/// </summary>
public static class FilmstripLayout
{
    /// <summary>Clips narrower than this (DIPs) draw a solid fallback instead of images.</summary>
    public const double MinStripWidth = 12;

    /// <summary>The cell width that shows a thumbnail undistorted at the strip height.</summary>
    public static double CellWidth(double stripHeight, double aspect) =>
        aspect > 0 && stripHeight > 0 ? stripHeight * aspect : 0;

    /// <summary>Grid step in source microseconds for one cell at this zoom.</summary>
    public static long StepUs(double cellWidth, double pixelsPerSecond) =>
        Math.Max(1, (long)Math.Round(cellWidth / pixelsPerSecond * 1_000_000));

    private static long FloorDiv(long a, long b) => a / b - (a % b != 0 && (a < 0) != (b < 0) ? 1 : 0);
    private static long CeilDiv(long a, long b) => -FloorDiv(-a, b);

    /// <summary>
    /// Appends the cells of <paramref name="clip"/> that intersect
    /// [<paramref name="left"/>, <paramref name="right"/>] (content x) to
    /// <paramref name="cells"/>; <c>Visible</c> marks those inside the inner
    /// window [<paramref name="visibleLeft"/>, <paramref name="visibleRight"/>].
    /// No allocation beyond the caller's list.
    /// </summary>
    public static void Cells(ClipView clip, double clipX, double clipRight, double pixelsPerSecond, long stepUs,
        double left, double right, double visibleLeft, double visibleRight, List<FilmCell> cells)
    {
        long inUs = clip.SourceInUs, endUs = clip.SourceEndUs;
        if (endUs <= inUs || stepUs <= 0 || pixelsPerSecond <= 0) return;
        var from = Math.Max(left, clipX);
        var to = Math.Min(right, clipRight);
        if (to <= from) return;
        double X(long us) => clipX + (us - inUs) / 1_000_000.0 * pixelsPerSecond;
        void Add(long start, long end)
        {
            var x = X(start);
            var width = Math.Min(X(end), clipRight) - x;
            if (width <= 0 || x >= to || x + width <= from) return;
            cells.Add(new FilmCell(x, width, start, x < visibleRight && x + width > visibleLeft));
        }
        var firstGrid = CeilDiv(inUs, stepUs) * stepUs;
        if (firstGrid > inUs) Add(inUs, Math.Min(firstGrid, endUs));
        var fromUs = inUs + (long)((from - clipX) / pixelsPerSecond * 1_000_000);
        var k = Math.Max(CeilDiv(inUs, stepUs), FloorDiv(fromUs, stepUs));
        for (var start = k * stepUs; start < endUs && X(start) < to; start += stepUs)
        {
            Add(start, Math.Min(start + stepUs, endUs));
        }
    }

    /// <summary>Whether a decoded frame belongs to this clip: inside its exact source interval.</summary>
    public static bool Belongs(ClipView clip, long actualTicks) =>
        actualTicks >= clip.SourceInTicks && actualTicks < clip.SourceEndTicks;
}

using System.Diagnostics;

namespace CutCap.Desktop.Services;

/// <summary>
/// Opt-in filmstrip timing (CUTCAP_TRACE_THUMBNAILS=1), written to stdout as one
/// line per event with wall-clock time and milliseconds since first use. Used by the native performance
/// scenario to measure time to first thumbnail, viewport fill, cache reuse and
/// draw cost; off by default, with no cost beyond one static check when off.
/// </summary>
public static class ThumbnailTrace
{
    public static readonly bool Enabled = Environment.GetEnvironmentVariable("CUTCAP_TRACE_THUMBNAILS") == "1";
    private static readonly long Started = Stopwatch.GetTimestamp();

    private static double Now => Stopwatch.GetElapsedTime(Started).TotalMilliseconds;

    public static void Line(FormattableString text)
    {
        if (Enabled) Console.WriteLine(FormattableString.Invariant($"thumb at={DateTime.Now:HH:mm:ss.fff} t={Now:F1} ") + FormattableString.Invariant(text));
    }
}

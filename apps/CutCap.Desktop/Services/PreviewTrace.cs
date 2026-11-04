using System.Diagnostics;

namespace CutCap.Desktop.Services;

/// <summary>
/// Opt-in preview timing (CUTCAP_TRACE_PREVIEW=1), written to stderr next to the
/// engine's own tracing. Measures UI-side latency to the moment the frame is in
/// the bitmap, comparable with the Phase 1 Slint "preview presented" log.
/// Physical paint and scan-out are not included.
/// </summary>
internal static class PreviewTrace
{
    public static readonly bool Enabled = Environment.GetEnvironmentVariable("CUTCAP_TRACE_PREVIEW") == "1";
    private static long _request;
    private static long _import;

    public static void MarkRequest()
    {
        if (Enabled)
        {
            _request = Stopwatch.GetTimestamp();
        }
    }

    public static void MarkImport()
    {
        if (Enabled)
        {
            _import = Stopwatch.GetTimestamp();
            _request = _import;
        }
    }

    public static void Presented(ulong generation, double copyMicroseconds)
    {
        if (!Enabled || _request == 0)
        {
            return;
        }
        var now = Stopwatch.GetTimestamp();
        var fromImport = _import != 0 ? $" import_to_present_us={Stopwatch.GetElapsedTime(_import, now).TotalMicroseconds:F0}" : "";
        Console.Error.WriteLine(
            $"ui-present generation={generation} request_to_present_us={Stopwatch.GetElapsedTime(_request, now).TotalMicroseconds:F0} copy_us={copyMicroseconds:F0}{fromImport}");
        _request = 0;
        _import = 0;
    }
}

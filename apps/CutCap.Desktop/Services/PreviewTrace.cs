using System.Diagnostics;
using CutCap.Interop;

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

    private static readonly bool PlaybackEnabled = Environment.GetEnvironmentVariable("CUTCAP_TRACE_PLAYBACK") == "1";
    private static string? _lastTransport;

    /// <summary>Opt-in (CUTCAP_TRACE_PLAYBACK=1): a transport command the UI issued.</summary>
    public static void Command(string name, long? milliseconds = null)
    {
        if (PlaybackEnabled)
        {
            Console.Error.WriteLine($"ui-command {name}{(milliseconds is { } ms ? $" ms={ms}" : "")} t={Stopwatch.GetTimestamp()}");
        }
    }

    /// <summary>Opt-in: engine playback state transitions as the UI received them.</summary>
    public static void Transport(PlaybackView? playback)
    {
        if (!PlaybackEnabled || playback is null)
        {
            return;
        }
        var line = $"{playback.State} desired={playback.DesiredPlaying} req={playback.RequestedMs} epoch={playback.Epoch}";
        if (line != _lastTransport)
        {
            _lastTransport = line;
            Console.Error.WriteLine($"ui-state {line} copied={playback.CopiedMs} t={Stopwatch.GetTimestamp()}");
        }
    }
}
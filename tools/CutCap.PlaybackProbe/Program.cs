using System.Diagnostics;
using System.Text.Json;
using CutCap.Interop;

// Diagnostic consumer of the real DLL. It does not create another editor or
// simulate video time: Rust schedules real timestamped frames; this only polls
// and acknowledges bytes. Physical rendering is deliberately not claimed.

internal static class Program
{
    [STAThread]
    public static int Main(string[] args)
    {
        if (args.FirstOrDefault() == "--timeline") return TimelineProbe.Run(args.Skip(1).ToArray());
        if (args.FirstOrDefault() == "--timeline-min")
        {
            Environment.SetEnvironmentVariable("CUTCAP_PROBE_MIN_LAYOUT", "1");
            return TimelineProbe.Run(args.Skip(1).ToArray());
        }
        if (args.FirstOrDefault() == "--ui") return UiProbe.Run(args.Skip(1).ToArray());
        if (args.FirstOrDefault() == "--ui-min")
        {
            Environment.SetEnvironmentVariable("CUTCAP_PROBE_MIN_LAYOUT", "1");
            return UiProbe.Run(args.Skip(1).ToArray());
        }
        Offscreen(args);
        return 0;
    }
    private static void Offscreen(string[] args)
    {
        if (args.Length != 2) throw new ArgumentException("Usage: PlaybackProbe <generated-sources-directory> <report.json>");
        var reports = new List<object>();
        foreach (var name in new[] { "720p", "1080p", "4k" })
            for (var run = 1; run <= 3; run++)
            {
                using var session = EngineSession.Start();
                SessionView current = session.ReadView();
                void Wait(Func<SessionView, bool> done)
                {
                    var wait = Stopwatch.StartNew();
                    do { session.Pump(); current = session.ReadView(); if (done(current)) return; Thread.Sleep(5); }
                    while (wait.Elapsed < TimeSpan.FromSeconds(30));
                    throw new TimeoutException(current.Source.Detail);
                }
                Wait(v => v.EngineReady); session.Import(Path.GetFullPath(Path.Combine(args[0], name + ".mp4")));
                Wait(v => v.Import.Error || v.Frame is not null && !v.Source.Busy);
                if (current.Import.Error) throw new InvalidOperationException(current.Import.Message);
                if (!session.TryGetFrameInfo(out var first)) throw new InvalidOperationException("No initial frame");
                var pixels = new byte[checked((int)(first.Stride * first.Height))];
                session.CopyFrame(first.Generation, pixels, (int)first.Stride);
                using var host = Process.GetCurrentProcess();
                var hostStartCpu = host.TotalProcessorTime.TotalMilliseconds;
                Process? child = null; double childCpu = 0; long peakWorking = 0, peakPrivate = 0;
                var deliveries = new List<object>(); var gaps = new List<double>();
                var watch = Stopwatch.StartNew(); double previous = 0, maxPumpMs = 0, maxLoopGap = 0, lastTick = 0;
                ulong generation = first.Generation;
                session.Play();
                while (watch.Elapsed < TimeSpan.FromSeconds(45))
                {
                    var tick = watch.Elapsed.TotalMilliseconds; maxLoopGap = Math.Max(maxLoopGap, tick - lastTick); lastTick = tick;
                    var pump = Stopwatch.StartNew(); session.Pump(); current = session.ReadView(); maxPumpMs = Math.Max(maxPumpMs, pump.Elapsed.TotalMilliseconds);
                    var playback = current.Playback ?? throw new InvalidOperationException("Missing ABI 2 playback view");
                    if (playback.State == PlaybackState.Failed) throw new InvalidOperationException(playback.Error);
                    if (current.Frame is { } frame && frame.Generation != generation && session.TryGetFrameInfo(out var info))
                    {
                        var copy = Stopwatch.StartNew();
                        if (session.CopyFrame(info.Generation, pixels, (int)info.Stride) == FrameCopyResult.Copied)
                        {
                            generation = info.Generation; var at = watch.Elapsed.TotalMilliseconds;
                            if (deliveries.Count > 0) gaps.Add(at - previous); previous = at;
                            deliveries.Add(new { wallMs = at, generation, pts = frame.Actual, copyUs = copy.Elapsed.TotalMicroseconds });
                        }
                    }
                    if (child is null && playback.ProcessId is > 0)
                    {
                        try { child = Process.GetProcessById((int)playback.ProcessId.Value); } catch (ArgumentException) { }
                    }
                    host.Refresh(); long working = host.WorkingSet64, commit = host.PrivateMemorySize64;
                    if (child is not null)
                    {
                        try
                        {
                            child.Refresh(); childCpu = child.TotalProcessorTime.TotalMilliseconds;
                            if (!child.HasExited) { working += child.WorkingSet64; commit += child.PrivateMemorySize64; }
                        }
                        catch (InvalidOperationException) { }
                    }
                    peakWorking = Math.Max(peakWorking, working); peakPrivate = Math.Max(peakPrivate, commit);
                    if (playback.State == PlaybackState.Ended) break;
                    Thread.Sleep(16);
                }
                var seconds = watch.Elapsed.TotalSeconds;
                if (current.Playback!.State != PlaybackState.Ended) throw new TimeoutException("Source did not finish");
                var cpuMs = host.TotalProcessorTime.TotalMilliseconds - hostStartCpu + childCpu;
                child?.Dispose();
                var shutdown = Stopwatch.StartNew(); session.Dispose();
                reports.Add(new
                {
                    name,
                    run,
                    method = "real DLL; 16ms offscreen consumer; copied FPS is not physical presented FPS",
                    seconds,
                    delivered = deliveries.Count,
                    copiedFps = deliveries.Count / seconds,
                    current.Playback.Decoded,
                    current.Playback.Dropped,
                    current.Playback.BufferStalls,
                    maxPumpMs,
                    maxLoopGapMs = maxLoopGap,
                    cpuOneCorePercent = cpuMs / (seconds * 1000) * 100,
                    cpuMachinePercent = cpuMs / (seconds * 1000 * Environment.ProcessorCount) * 100,
                    logicalProcessors = Environment.ProcessorCount,
                    peakTotalWorkingSetBytes = peakWorking,
                    peakTotalPrivateBytes = peakPrivate,
                    memoryMethod = "sampled host plus decoder child every consumer tick; captures may miss peaks",
                    shutdownMs = shutdown.Elapsed.TotalMilliseconds,
                    deliveries,
                    gapsMs = gaps
                });
                Console.WriteLine($"{name} run {run}: {deliveries.Count} copied in {seconds:F3}s");
            }
        File.WriteAllText(args[1], JsonSerializer.Serialize(reports, new JsonSerializerOptions { WriteIndented = true }));
    }
}

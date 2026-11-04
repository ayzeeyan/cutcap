using System.Diagnostics;
using System.Text.Json;
using Avalonia;
using Avalonia.Controls.ApplicationLifetimes;
using Avalonia.Media;
using Avalonia.Threading;
using Avalonia.VisualTree;
using CutCap.Desktop;
using CutCap.Desktop.ViewModels;
using CutCap.Interop;
using VideoFrame = CutCap.Desktop.Controls.FrameView;

/// <summary>Drives the actual editor without replacing its views, host pump or clock.</summary>
internal sealed class UiProbe
{
    private readonly string _directory;
    private readonly List<object> _results = [];
    private readonly Queue<(string Name, int Run)> _plan = new(
        from name in new[] { "720p", "1080p", "4k" } from run in Enumerable.Range(1, 3) select (name, run));
    private readonly DispatcherTimer _timer;
    private readonly Stopwatch _lifetime = Stopwatch.StartNew();
    private readonly Stopwatch _phase = new();
    private readonly Process _host = Process.GetCurrentProcess();
    private Process? _child;
    private (string Name, int Run) _item;
    private VideoFrame? _frame;
    private EngineSession? _session;
    private string _stage = "project";
    private readonly List<object> _renders = [];
    private readonly List<double> _gaps = [];
    private ulong _generation;
    private long _previousRender;
    private double _lastTick, _maxTickGap, _hostCpu, _childCpu;
    private long _peakWorking, _peakPrivate;
    private string? _error;
    private string? _previousProject;
    private List<object> _seekSamples = [];
    private readonly Queue<long> _seekPlan = new();
    private long _seekTarget, _seekStarted;
    private string? _seekOldEpoch;
    private bool _seekDone;

    private UiProbe(string directory)
    {
        _directory = directory;
        _timer = new DispatcherTimer(TimeSpan.FromMilliseconds(16), DispatcherPriority.Background, (_, _) => Tick());
        _phase.Start();
    }
    public static int Run(string[] args)
    {
        if (args.Length != 2) throw new ArgumentException("--ui <generated-source-directory> <report.json>");
        Environment.SetEnvironmentVariable("CUTCAP_TRACE_PLAYBACK", "1");
        var probe = new UiProbe(Path.GetFullPath(args[0]));
        AppBuilder.Configure<App>().UsePlatformDetect().WithInterFont()
            .With(new FontManagerOptions { DefaultFamilyName = "fonts:Inter#Inter" })
            .AfterSetup(_ => probe._timer.Start()).StartWithClassicDesktopLifetime([]);
        probe._timer.Stop();
        if (probe._frame is not null) probe._frame.FrameRendered -= probe.Rendered;
        probe._child?.Dispose();
        if (probe._error is null && probe._results.Count != 9) probe._error = "Window closed before all benchmark episodes completed.";
        File.WriteAllText(args[1], JsonSerializer.Serialize(new
        {
            method = "actual CutCap editor, original 16ms EngineHost pump; distinct FrameView.DrawImage generations, not physical scanout; generated playback controlled through public interop methods, visual Play remains frontend-owned",
            date = DateTimeOffset.UtcNow,
            error = probe._error,
            runs = probe._results
        }, new JsonSerializerOptions { WriteIndented = true }));
        probe._host.Dispose();
        return probe._error is null ? 0 : 1;
    }
    private void Rendered(ulong generation, long ticks)
    {
        if (_stage is not ("playing" or "seeking") || generation == _generation) return;
        var view = _session?.LastReadView;
        if (view?.Frame?.Generation != generation || view.Playback?.Epoch is null
            || view.Frame.Epoch != view.Playback.Epoch) return;
        if (_stage == "seeking")
        {
            if (!_seekDone && view.Playback.RequestedMs == _seekTarget && view.Playback.Epoch != _seekOldEpoch)
            {
                _seekSamples.Add(new { requestedMs = _seekTarget, renderLatencyMs = Stopwatch.GetElapsedTime(_seekStarted, ticks).TotalMilliseconds, actual = view.Frame.Actual, readyUs = view.Playback.RequestToReadyUs });
                _seekDone = true; _generation = generation;
            }
            return;
        }
        var at = Stopwatch.GetElapsedTime(_started, ticks).TotalMilliseconds;
        if (_previousRender != 0) _gaps.Add(Stopwatch.GetElapsedTime(_previousRender, ticks).TotalMilliseconds);
        _generation = generation; _previousRender = ticks;
        _renders.Add(new { generation, wallMs = at, pts = view.Frame.Actual, copyUs = _frame!.LastCopyMicroseconds });
    }
    private long _started;
    private void Tick()
    {
        try
        {
            var desktop = (IClassicDesktopStyleApplicationLifetime)Application.Current!.ApplicationLifetime!;
            if (desktop.MainWindow?.DataContext is not MainWindowViewModel model || !model.EngineReady) return;
            _session = model.Player.Session ?? throw new InvalidOperationException("No session");
            var view = _session.LastReadView;
            if (view is null) return;
            var current = _lifetime.Elapsed.TotalMilliseconds;
            if (_stage == "playing") _maxTickGap = Math.Max(_maxTickGap, current - _lastTick);
            _lastTick = current;
            _frame ??= desktop.MainWindow.GetVisualDescendants().OfType<VideoFrame>().Single();
            if (_stage == "project")
            {
                if (_plan.Count == 0) { Finish(desktop); return; }
                _item = _plan.Dequeue(); _previousProject = view.Project?.Id; _session.NewProject(); _stage = "empty"; _phase.Restart(); return;
            }
            if (_stage == "empty" && view.EngineReady && view.Project?.Id != _previousProject && view.Media.Count == 0 && view.Frame is null)
            {
                _session.Import(Path.Combine(_directory, _item.Name + ".mp4")); _stage = "import"; _phase.Restart(); return;
            }
            if (_stage == "import")
            {
                if (view.Import.Error) throw new InvalidOperationException(view.Import.Message);
                if (view.Frame is not null && !view.Source.Busy && _frame.HasFrame)
                {
                    _renders.Clear(); _gaps.Clear(); _generation = 0; _previousRender = 0; _maxTickGap = 0;
                    _seekSamples = [];
                    _hostCpu = _host.TotalProcessorTime.TotalMilliseconds; _childCpu = 0; _peakWorking = 0; _peakPrivate = 0;
                    _frame.FrameRendered += Rendered; _stage = "playing"; _started = Stopwatch.GetTimestamp(); _phase.Restart(); _session.Play();
                }
            }
            else if (_stage == "playing")
            {
                var playback = view.Playback ?? throw new InvalidOperationException("Missing ABI 2 view");
                if (playback.State == PlaybackState.Failed) throw new InvalidOperationException(playback.Error);
                if (_child is null && playback.ProcessId is > 0)
                { try { _child = Process.GetProcessById((int)playback.ProcessId.Value); } catch (ArgumentException) { } }
                _host.Refresh(); long working = _host.WorkingSet64, commit = _host.PrivateMemorySize64;
                if (_child is not null)
                {
                    try { _child.Refresh(); _childCpu = _child.TotalProcessorTime.TotalMilliseconds; if (!_child.HasExited) { working += _child.WorkingSet64; commit += _child.PrivateMemorySize64; } }
                    catch (InvalidOperationException) { }
                }
                _peakWorking = Math.Max(_peakWorking, working); _peakPrivate = Math.Max(_peakPrivate, commit);
                if (playback.State == PlaybackState.Ended && view.Frame?.Generation == _generation)
                {
                    var elapsed = Stopwatch.GetElapsedTime(_started).TotalSeconds;
                    var cpu = _host.TotalProcessorTime.TotalMilliseconds - _hostCpu + _childCpu;
                    _results.Add(new
                    {
                        source = _item.Name,
                        run = _item.Run,
                        seconds = elapsed,
                        rendered = _renders.Count,
                        distinctRenderFps = _renders.Count / elapsed,
                        playback.Decoded,
                        playback.Offered,
                        playback.Copied,
                        playback.Dropped,
                        playback.BufferStalls,
                        maxDispatcherGapMs = _maxTickGap,
                        peakHostAndChildWorkingSetBytes = _peakWorking,
                        peakHostAndChildPrivateBytes = _peakPrivate,
                        cpuOneCorePercent = cpu / (elapsed * 1000) * 100,
                        cpuMachinePercent = cpu / (elapsed * 1000 * Environment.ProcessorCount) * 100,
                        logicalProcessors = Environment.ProcessorCount,
                        scale = desktop.MainWindow.RenderScaling,
                        clientWidth = desktop.MainWindow.ClientSize.Width,
                        clientHeight = desktop.MainWindow.ClientSize.Height,
                        frames = _renders.ToArray(),
                        gapsMs = _gaps.ToArray(),
                        seeks = _seekSamples
                    });
                    Console.WriteLine($"UI {_item.Name} {_item.Run}: {_renders.Count} distinct renders / {elapsed:F3}s");
                    _child?.Dispose(); _child = null;
                    foreach (var target in new long[] { 1250, 2750, 4800 }) _seekPlan.Enqueue(target);
                    _stage = "seeking"; _seekDone = true; _phase.Restart();
                }
            }
            else if (_stage == "seeking" && _seekDone)
            {
                if (_seekPlan.TryDequeue(out _seekTarget))
                {
                    _seekOldEpoch = view.Playback?.Epoch; _seekDone = false; _seekStarted = Stopwatch.GetTimestamp();
                    _session.Seek(_seekTarget); _phase.Restart();
                }
                else
                {
                    // Keep an independent list in each report; source playback samples
                    // are immutable, seek samples are completed after its EOS.
                    _frame.FrameRendered -= Rendered; _stage = "project"; _phase.Restart();
                }
            }
            if (_phase.Elapsed > TimeSpan.FromSeconds(40)) throw new TimeoutException($"UI {_stage} stalled: {view.Source.Detail}");
        }
        catch (Exception error)
        {
            _error = error.ToString(); Console.Error.WriteLine(_error);
            if (Application.Current?.ApplicationLifetime is IClassicDesktopStyleApplicationLifetime desktop) Finish(desktop);
        }
    }
    private void Finish(IClassicDesktopStyleApplicationLifetime desktop)
    {
        _timer.Stop(); if (_frame is not null) _frame.FrameRendered -= Rendered; _child?.Dispose(); desktop.Shutdown();
    }
}

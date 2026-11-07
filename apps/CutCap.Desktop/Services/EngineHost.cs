using Avalonia;
using Avalonia.Controls;
using Avalonia.Media.Imaging;
using Avalonia.Platform;
using Avalonia.Threading;
using CutCap.Interop;

namespace CutCap.Desktop.Services;

/// <summary>
/// UI-thread owner of the native engine session. A dispatcher timer pumps the
/// engine (bounded, non-blocking) and publishes the coalesced view; frames are
/// announced by generation and pulled by the view layer, never pushed through
/// view-model properties. No native callbacks exist, so nothing native can
/// outlive a collected delegate or race UI shutdown.
/// </summary>
public sealed class EngineHost : IDisposable
{
    private static readonly TimeSpan PumpInterval = TimeSpan.FromMilliseconds(16);
    private readonly DispatcherTimer _timer;
    private EngineSession? _session;
    private ulong _frameGeneration;
    // Render-loop pumping during playback (see AttachRenderLoop). Diagnostic A/B:
    // CUTCAP_PUMP_MODE=timer keeps the timer-only behaviour of Phase 2.
    private static readonly bool RenderLoopPump = Environment.GetEnvironmentVariable("CUTCAP_PUMP_MODE") != "timer";
    private TopLevel? _renderLoop;
    private bool _frameRequested;

    public EngineHost()
    {
        _timer = new DispatcherTimer(PumpInterval, DispatcherPriority.Input, (_, _) => Tick());
    }

    /// <summary>The latest engine view; null until the first pump or if the engine failed to start.</summary>
    public SessionView? View { get; private set; }

    /// <summary>Set when the native engine could not be loaded or started.</summary>
    public string? StartupError { get; private set; }

    public event EventHandler<SessionView>? ViewChanged;

    /// <summary>A command was rejected before reaching the engine queue (busy, invalid input).</summary>
    public event EventHandler<string>? CommandRejected;

    /// <summary>A newer decoded frame is available (or the frame was cleared: null).</summary>
    public event EventHandler<FrameView?>? FrameChanged;

    public EngineSession? Session => _session;

    /// <summary>Filmstrip thumbnails (Phase 4A): bounded, UI-thread owned, never project data.</summary>
    public ThumbnailCache Thumbnails { get; } = ThumbnailCache.FromEnvironment();

    /// <summary>New thumbnails arrived; the lanes redraw and may request more.</summary>
    public event EventHandler? ThumbnailsChanged;

    /// <summary>Most thumbnails copied per pump, so a burst never stalls the UI thread.</summary>
    public const int MaxThumbnailsPerTick = 16;

    private string? _thumbnailProject;
    private long _playbackTraced;
    private IReadOnlyList<ThumbnailWant> _lastDemand = [];

    /// <summary>Sends the current filmstrip demand (skipped when unchanged).</summary>
    public void RequestThumbnails(IReadOnlyList<ThumbnailWant> wants)
    {
        if (_session is null || _session.IsDisposed) return;
        if (wants.Count == _lastDemand.Count && wants.Select(w => w.Token).SequenceEqual(_lastDemand.Select(w => w.Token))) return;
        _lastDemand = wants;
        ThumbnailTrace.Line($"demand n={wants.Count}");
        Run(session => session.RequestThumbnails(wants));
    }

    /// <summary>Copies waiting thumbnails into bitmaps (one allocation per decoded image).</summary>
    private void DrainThumbnails(EngineSession session)
    {
        var changed = false;
        var ready = 0;
        var started = ThumbnailTrace.Enabled ? System.Diagnostics.Stopwatch.GetTimestamp() : 0;
        for (var i = 0; i < MaxThumbnailsPerTick && session.TryNextThumbnail(out var info); i++)
        {
            if (info.Status != 0)
            {
                changed |= Thumbnails.Complete(info.Token, info.Status == 1 ? ThumbnailState.NoFrame : ThumbnailState.Failed, null, 0, 0);
                continue;
            }
            var bitmap = new WriteableBitmap(new PixelSize((int)info.Width, (int)info.Height), new Vector(96, 96),
                PixelFormats.Rgba8888, AlphaFormat.Opaque);
            bool copied;
            using (var target = bitmap.Lock())
            {
                copied = session.CopyThumbnail(info.Token, target.Address, target.RowBytes * target.Size.Height, target.RowBytes);
            }
            if (!copied)
            {
                bitmap.Dispose();
                continue;
            }
            changed |= Thumbnails.Complete(info.Token, ThumbnailState.Ready, bitmap, info.ActualTicks,
                (int)(info.Width * info.Height * 4));
            ready++;
        }
        if (ThumbnailTrace.Enabled && ready > 0)
        {
            ThumbnailTrace.Line($"copied n={ready} copy_us={System.Diagnostics.Stopwatch.GetElapsedTime(started).TotalMicroseconds:F0} resident_kib={Thumbnails.ResidentBytes / 1024} entries={Thumbnails.Count} evictions={Thumbnails.Evictions} hits={Thumbnails.Hits} misses={Thumbnails.Misses} pid={View?.Thumbnails?.LastProcessId} batch_ms={View?.Thumbnails?.LastBatchMs} batches={View?.Thumbnails?.Batches} cancelled={View?.Thumbnails?.CancelledBatches} managed_mib={GC.GetTotalMemory(false) / 1048576} ws_mib={Environment.WorkingSet / 1048576}");
        }
        if (changed) ThumbnailsChanged?.Invoke(this, EventArgs.Empty);
    }

    /// <summary>
    /// The latest clip list. Read only when the view's timeline generation
    /// changes (edits, opens), never per playback frame.
    /// </summary>
    public TimelineView? Timeline { get; private set; }

    /// <summary>True while the engine is in a playback epoch that may offer new frames.</summary>
    public bool PlaybackActive { get; private set; }

    /// <summary>
    /// Also pump once per compositor frame while playback is active. The 16 ms
    /// timer alone is quantized by the Windows timer (~15.6 ms ticks), so 33 ms
    /// frames were offered at alternating ~16/~47 ms intervals. Animation-frame
    /// callbacks run on the UI thread just before a render, so an offered frame is
    /// copied and drawn in the same compositor frame. No busy loop: callbacks stop
    /// when playback is inactive and the timer remains the idle path.
    /// </summary>
    public void AttachRenderLoop(TopLevel topLevel)
    {
        _renderLoop = topLevel;
        RequestFrame();
    }

    private void RequestFrame()
    {
        if (!RenderLoopPump || _frameRequested || !PlaybackActive || _renderLoop is null || _session is null || _session.IsDisposed)
        {
            return;
        }
        _frameRequested = true;
        _renderLoop.RequestAnimationFrame(_ =>
        {
            _frameRequested = false;
            Tick();
        });
    }

    public void Start()
    {
        try
        {
            _session = EngineSession.Start();
            _timer.Start();
        }
        catch (Exception error) when (error is EngineException or DllNotFoundException or EntryPointNotFoundException or BadImageFormatException)
        {
            StartupError = error is EngineException
                ? error.Message
                : "CutCap could not load its engine (cutcap_engine.dll). Reinstall or rebuild CutCap.";
            CommandRejected?.Invoke(this, StartupError);
        }
    }

    /// <summary>Runs one engine command; rejections become a status message, never an exception in the UI.</summary>
    public bool Run(Action<EngineSession> command)
    {
        if (_session is null || _session.IsDisposed)
        {
            return false;
        }
        try
        {
            command(_session);
            return true;
        }
        catch (EngineException error)
        {
            CommandRejected?.Invoke(this, error.Message);
            return false;
        }
    }

    private void Tick()
    {
        if (_session is null || _session.IsDisposed)
        {
            return;
        }
        try
        {
            var viewChanged = _session.Pump();
            DrainThumbnails(_session);
            if (!viewChanged)
            {
                RequestFrame();
                return;
            }
            var view = _session.ReadView();
            if (Timeline is null || Timeline.Generation != view.TimelineGeneration)
            {
                Timeline = _session.ReadTimeline();
            }
            View = view;
            if (view.Project?.Id is { } project && project != _thumbnailProject)
            {
                // Results requested for the previous project are void.
                _thumbnailProject = project;
                Thumbnails.ClearPending();
                _lastDemand = [];
            }
            var activeState = view.PreviewMode == PreviewMode.Timeline ? view.TimelinePlayback?.State : view.Playback?.State;
            PlaybackActive = activeState is PlaybackState.Starting or PlaybackState.Playing
                or PlaybackState.Seeking or PlaybackState.Buffering;
            PreviewTrace.Transport(view.Playback);
            if (ThumbnailTrace.Enabled && view.TimelinePlayback is { } timeline && PlaybackActive
                && Environment.TickCount64 - _playbackTraced >= 1000)
            {
                _playbackTraced = Environment.TickCount64;
                ThumbnailTrace.Line($"playback copied={timeline.Copied} dropped={timeline.Dropped} stalls={timeline.BufferStalls} state={timeline.State} thumbs_pending={view.Thumbnails?.Pending} throttled={view.Thumbnails?.Throttled}");
            }
            ViewChanged?.Invoke(this, view);
            var generation = view.Frame?.Generation ?? 0;
            if (generation != _frameGeneration)
            {
                _frameGeneration = generation;
                FrameChanged?.Invoke(this, view.Frame);
            }
            RequestFrame();
        }
        catch (EngineException error)
        {
            _timer.Stop();
            CommandRejected?.Invoke(this, $"The engine stopped responding: {error.Message} Restart CutCap.");
        }
    }

    /// <summary>Stops pumping, then cancels media work and joins the native workers.</summary>
    public void Dispose()
    {
        _timer.Stop();
        Thumbnails.Dispose();
        PlaybackActive = false;
        _renderLoop = null;
        _session?.Dispose();
    }
}

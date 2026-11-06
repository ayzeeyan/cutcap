using Avalonia.Controls;
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
            if (!_session.Pump())
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
            var activeState = view.PreviewMode == PreviewMode.Timeline ? view.TimelinePlayback?.State : view.Playback?.State;
            PlaybackActive = activeState is PlaybackState.Starting or PlaybackState.Playing
                or PlaybackState.Seeking or PlaybackState.Buffering;
            PreviewTrace.Transport(view.Playback);
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
        PlaybackActive = false;
        _renderLoop = null;
        _session?.Dispose();
    }
}

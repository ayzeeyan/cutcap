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
                return;
            }
            var view = _session.ReadView();
            View = view;
            ViewChanged?.Invoke(this, view);
            var generation = view.Frame?.Generation ?? 0;
            if (generation != _frameGeneration)
            {
                _frameGeneration = generation;
                FrameChanged?.Invoke(this, view.Frame);
            }
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
        _session?.Dispose();
    }
}

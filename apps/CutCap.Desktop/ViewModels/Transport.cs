using CutCap.Interop;

namespace CutCap.Desktop.ViewModels;

/// <summary>
/// What the source transport shows, derived only from the engine's view (ABI 2).
/// There is no frontend clock: the readout is the last copied frame's PTS while
/// a playback epoch has delivered one, otherwise the requested source position.
/// A command being accepted never changes this; only the next view does.
/// </summary>
public readonly record struct Transport(
    PlaybackState State,
    bool CanPlay,
    bool CanStop,
    bool ShowPause,
    string PlayName,
    string PlayHint,
    string? Chip,
    bool ChipBusy,
    long DisplayMs,
    bool DisplayIsFrame,
    long SliderMs,
    bool ShowIsolatedBusy,
    string? Error)
{
    public static Transport Map(SourceView source, PlaybackView? playback, bool engineReady)
    {
        var state = playback?.State ?? PlaybackState.Stopped;
        var desired = playback?.DesiredPlaying ?? false;
        var inEpoch = state is not PlaybackState.Stopped;
        var canPlay = engineReady && source.Ready;
        // Pause is offered while the engine intends to play, including while it is
        // still starting, seeking or buffering; Play otherwise (Ended restarts at zero).
        var showPause = desired && state is PlaybackState.Starting or PlaybackState.Playing
            or PlaybackState.Seeking or PlaybackState.Buffering;
        var (chip, busy) = state switch
        {
            PlaybackState.Starting => ("Starting…", true),
            PlaybackState.Seeking => ("Seeking…", true),
            PlaybackState.Buffering => ("Buffering…", true),
            PlaybackState.Ended => ("End of source", false),
            PlaybackState.Failed => ("Playback failed", false),
            _ => ((string?)null, false),
        };
        var copied = inEpoch ? playback?.CopiedMs : null;
        var requested = inEpoch ? playback!.RequestedMs : source.PositionMs;
        var display = copied is { } ms ? (long)ms : requested;
        // While a seek or start is outstanding the thumb holds the request, so the
        // control does not snap back to the previous frame before the new one arrives.
        var slider = state is PlaybackState.Seeking or PlaybackState.Starting ? requested : display;
        return new Transport(
            state,
            canPlay,
            CanStop: canPlay && inEpoch,
            showPause,
            PlayName: showPause ? "Pause" : "Play",
            PlayHint: !canPlay ? "Select a source video to play it."
                : showPause ? "Pause source playback (Space)"
                : state == PlaybackState.Ended ? "Play again from the start (Space)"
                : "Play the source from the shown position (Space)",
            chip,
            busy,
            display,
            DisplayIsFrame: copied is not null,
            slider,
            ShowIsolatedBusy: !inEpoch && source.Busy,
            Error: state == PlaybackState.Failed ? playback?.Error ?? "The decoder stopped." : null);
    }
}

/// <summary>
/// Turns slider movement into source seeks. Keyboard steps seek at once; a
/// pointer drag issues at most one seek per <see cref="IntervalMs"/> (latest
/// position wins) and a final seek on release, never repeating a target within
/// one gesture. The engine's own latest-request cancellation handles the rest.
/// </summary>
public sealed class ScrubController(Func<long> nowMs, Action<long> seek)
{
    public const long IntervalMs = 120;
    private long? _issued;
    private long? _pending;
    private long? _issuedAt;

    public bool IsScrubbing { get; private set; }

    public void Begin()
    {
        IsScrubbing = true;
        _issued = null;
        _pending = null;
    }

    public void Move(long target)
    {
        if (!IsScrubbing)
        {
            Issue(target);
            return;
        }
        _pending = target;
        if (_issuedAt is not { } last || nowMs() - last >= IntervalMs)
        {
            Flush();
        }
    }

    public void End()
    {
        if (!IsScrubbing)
        {
            return;
        }
        IsScrubbing = false;
        Flush();
    }

    /// <summary>Source replaced or project reset: forget the gesture without seeking.</summary>
    public void Cancel()
    {
        IsScrubbing = false;
        _issued = null;
        _pending = null;
    }

    private void Flush()
    {
        if (_pending is { } target)
        {
            _pending = null;
            if (target != _issued)
            {
                Issue(target);
            }
        }
    }

    private void Issue(long target)
    {
        _issued = target;
        _issuedAt = nowMs();
        seek(target);
    }
}

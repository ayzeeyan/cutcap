using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

[assembly: InternalsVisibleTo("CutCap.Interop.Tests")]

namespace CutCap.Interop;

public enum TrackKind : uint
{
    Video = 0,
    Audio = 1,
}

public enum FrameCopyResult
{
    Copied,
    /// <summary>A newer frame replaced the requested generation; read the info again.</summary>
    Stale,
    NoFrame,
}

public sealed class EngineException(NativeStatus status, string message) : Exception(message)
{
    public NativeStatus Status { get; } = status;
}

/// <summary>
/// Managed owner of one Rust engine session. Call it from one thread (the UI
/// thread): the native last-error slot is per thread. Every call is
/// non-blocking except <see cref="Dispose"/>, which cancels media work and
/// joins the native workers.
/// </summary>
public sealed class EngineSession : IDisposable
{
    /// <summary>Upper bound on the view document; the engine caps media at 128 assets.</summary>
    public const int MaxViewBytes = 8 * 1024 * 1024;

    private readonly SessionSafeHandle _handle;
    private byte[] _viewBuffer = new byte[64 * 1024];

    private EngineSession(SessionSafeHandle handle) => _handle = handle;

    /// <summary>ABI version reported by the loaded cutcap_engine.dll.</summary>
    public static uint LibraryAbiVersion => NativeMethods.AbiVersionOfLibrary();

    public bool IsDisposed => _handle.IsClosed;

    /// <summary>Last immutable view read on the owning thread; diagnostics need no extra ABI polling.</summary>
    public SessionView? LastReadView { get; private set; }

    /// <summary>Loads the engine, verifies the ABI and starts a session with an empty project.</summary>
    public static EngineSession Start()
    {
        var library = LibraryAbiVersion;
        if (library != NativeMethods.AbiVersion)
        {
            throw new EngineException(NativeStatus.AbiMismatch,
                $"cutcap_engine.dll speaks ABI {library}; this build of CutCap expects {NativeMethods.AbiVersion}.");
        }
        var status = NativeMethods.SessionCreate(NativeMethods.AbiVersion, out var value);
        if (status != NativeStatus.Ok)
        {
            throw new EngineException(status, LastError());
        }
        return new EngineSession(new SessionSafeHandle(value, ownsHandle: true));
    }

    public void NewProject() => Check(NativeMethods.NewProject(Handle));

    public void AddTrack(TrackKind kind) => Check(NativeMethods.AddTrack(Handle, (uint)kind));

    public unsafe void SelectTrack(string id)
    {
        var bytes = Encoding.UTF8.GetBytes(id);
        fixed (byte* pointer = bytes)
        {
            Check(NativeMethods.SelectTrack(Handle, pointer, (nuint)bytes.Length));
        }
    }

    public void ClearSelection() => Check(NativeMethods.ClearSelection(Handle));

    public unsafe void SelectMedia(string id)
    {
        var bytes = Encoding.UTF8.GetBytes(id);
        fixed (byte* pointer = bytes)
        {
            Check(NativeMethods.SelectMedia(Handle, pointer, (nuint)bytes.Length));
        }
    }

    /// <summary>Starts a probe of a local file. The path crosses as UTF-16, so any Windows path round-trips.</summary>
    public unsafe void Import(string path)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        fixed (char* pointer = path)
        {
            Check(NativeMethods.Import(Handle, pointer, (nuint)path.Length));
        }
    }

    public void CancelImport() => Check(NativeMethods.CancelImport(Handle));

    public void Seek(long milliseconds) => Check(NativeMethods.Seek(Handle, milliseconds));

    /// <summary>Parses go-to text with the engine's tested grammar (1500, 1.5s, 00:01.500).</summary>
    public unsafe void SeekText(string text)
    {
        var bytes = Encoding.UTF8.GetBytes(text);
        fixed (byte* pointer = bytes)
        {
            Check(NativeMethods.SeekText(Handle, pointer, (nuint)bytes.Length));
        }
    }

    public void CancelPreview() => Check(NativeMethods.CancelPreview(Handle));

    /// <summary>Accepts source playback/resume; inspect Playback state for readiness.</summary>
    public void Play() => Check(NativeMethods.Play(Handle));
    public void Resume() => Play();
    public void Pause() => Check(NativeMethods.Pause(Handle));
    public void Stop() => Check(NativeMethods.Stop(Handle));

    /// <summary>Drains bounded worker results. True when the view changed since it was last read.</summary>
    public bool Pump()
    {
        Check(NativeMethods.Pump(Handle, out var changed));
        return changed != 0;
    }

    /// <summary>Reads the current view. Serialized once per change on the native side.</summary>
    public unsafe SessionView ReadView()
    {
        while (true)
        {
            NativeStatus status;
            nuint length;
            fixed (byte* pointer = _viewBuffer)
            {
                status = NativeMethods.View(Handle, pointer, (nuint)_viewBuffer.Length, out length);
            }
            if (status == NativeStatus.BufferTooSmall)
            {
                if (length > MaxViewBytes)
                {
                    throw new EngineException(NativeStatus.Failed, "The engine view exceeded its size limit.");
                }
                _viewBuffer = new byte[(int)length];
                continue;
            }
            Check(status);
            if (length > (nuint)_viewBuffer.Length)
            {
                throw new EngineException(NativeStatus.Failed, "The engine reported an invalid view length.");
            }
            LastReadView = JsonSerializer.Deserialize(_viewBuffer.AsSpan(0, (int)length), ViewJsonContext.Default.SessionView)
                ?? throw new EngineException(NativeStatus.Failed, "The engine returned an empty view.");
            return LastReadView;
        }
    }

    public bool TryGetFrameInfo(out NativeFrameInfo info)
    {
        var status = NativeMethods.FrameInfo(Handle, out info);
        if (status == NativeStatus.NoFrame)
        {
            return false;
        }
        Check(status);
        if (info.PixelFormat != NativeMethods.PixelRgba8Opaque || info.Width == 0 || info.Height == 0
            || info.Stride < info.Width * 4)
        {
            throw new EngineException(NativeStatus.Failed, "The engine described an unsupported frame.");
        }
        return true;
    }

    /// <summary>
    /// Copies frame <paramref name="generation"/> into caller memory with
    /// <paramref name="stride"/> bytes per row (for example a locked bitmap).
    /// The engine validates the destination size before writing.
    /// </summary>
    public unsafe FrameCopyResult CopyFrame(ulong generation, nint destination, int length, int stride)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(length);
        ArgumentOutOfRangeException.ThrowIfNegative(stride);
        var status = NativeMethods.CopyFrame(Handle, generation, (byte*)destination, (nuint)length, (nuint)stride);
        switch (status)
        {
            case NativeStatus.StaleFrame:
                return FrameCopyResult.Stale;
            case NativeStatus.NoFrame:
                return FrameCopyResult.NoFrame;
            default:
                Check(status);
                return FrameCopyResult.Copied;
        }
    }

    /// <summary>Copies into a managed buffer; used by tests and diagnostics.</summary>
    public unsafe FrameCopyResult CopyFrame(ulong generation, Span<byte> destination, int stride)
    {
        fixed (byte* pointer = destination)
        {
            return CopyFrame(generation, (nint)pointer, destination.Length, stride);
        }
    }

    /// <summary>Cancels probe/decode work, stops FFmpeg children and joins native workers.</summary>
    public void Dispose() => _handle.Dispose();

    private SessionSafeHandle Handle
    {
        get
        {
            ObjectDisposedException.ThrowIf(_handle.IsClosed, this);
            return _handle;
        }
    }

    private static void Check(NativeStatus status)
    {
        if (status != NativeStatus.Ok)
        {
            throw new EngineException(status, LastError());
        }
    }

    internal static unsafe string LastError()
    {
        Span<byte> buffer = stackalloc byte[1024];
        nuint length;
        NativeStatus status;
        fixed (byte* pointer = buffer)
        {
            status = NativeMethods.LastError(pointer, (nuint)buffer.Length, out length);
        }
        if (status == NativeStatus.BufferTooSmall && length <= 64 * 1024)
        {
            var large = new byte[(int)length];
            fixed (byte* pointer = large)
            {
                status = NativeMethods.LastError(pointer, (nuint)large.Length, out length);
            }
            return status == NativeStatus.Ok ? Encoding.UTF8.GetString(large, 0, (int)length) : "Unknown engine error.";
        }
        if (status != NativeStatus.Ok || length > (nuint)buffer.Length)
        {
            return "Unknown engine error.";
        }
        var message = Encoding.UTF8.GetString(buffer[..(int)length]);
        return message.Length == 0 ? "Unknown engine error." : message;
    }
}

using System.Runtime.InteropServices;

namespace CutCap.Interop;

/// <summary>
/// Owns one native session handle. The handle is an opaque registry key, not a
/// pointer, so a stale value is rejected natively instead of dereferenced.
/// SafeHandle reference counting keeps the session alive for the duration of
/// every P/Invoke that uses it, so Dispose cannot race an in-flight call.
/// </summary>
public sealed class SessionSafeHandle : SafeHandle
{
    public SessionSafeHandle()
        : base(IntPtr.Zero, ownsHandle: true)
    {
    }

    internal SessionSafeHandle(ulong value, bool ownsHandle)
        : base(IntPtr.Zero, ownsHandle)
    {
        SetHandle(unchecked((nint)value));
    }

    public override bool IsInvalid => handle == IntPtr.Zero;

    /// <summary>Status of the native destroy call, for diagnostics after release.</summary>
    public NativeStatus ReleaseStatus { get; private set; } = NativeStatus.Ok;

    internal ulong Value => unchecked((ulong)handle);

    protected override bool ReleaseHandle()
    {
        // Cancels probe/decode work and joins the native workers (bounded).
        ReleaseStatus = NativeMethods.SessionDestroy(Value);
        return ReleaseStatus == NativeStatus.Ok;
    }
}

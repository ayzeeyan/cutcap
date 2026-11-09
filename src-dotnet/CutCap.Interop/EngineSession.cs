using System.Runtime.CompilerServices;
using System.Text;
using System.Text.Json;

[assembly: InternalsVisibleTo("CutCap.Interop.Tests")]

namespace CutCap.Interop;

/// <summary>Which clip boundary a move anchor or trim names (ABI 7).</summary>
public enum ClipEdge : uint
{
    Start = 0,
    End = 1,
}

/// <summary>
/// A timeline position for an edit: exact project ticks (from the view, for
/// snapped boundaries) or whole milliseconds, which Rust floors to ticks.
/// The frontend never converts between them.
/// </summary>
public readonly record struct TimelinePosition(long Value, bool IsTicks)
{
    public static TimelinePosition Ticks(long ticks) => new(ticks, true);
    public static TimelinePosition Milliseconds(long milliseconds) => new(milliseconds, false);
    internal uint Unit => IsTicks ? 0u : 1u;
}

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

    /// <summary>Upper bound on the timeline document (about 340 bytes per clip, 100,000 clips).</summary>
    public const int MaxTimelineBytes = 64 * 1024 * 1024;

    private readonly SessionSafeHandle _handle;
    private byte[] _viewBuffer = new byte[64 * 1024];
    private byte[] _timelineBuffer = new byte[16 * 1024];

    private EngineSession(SessionSafeHandle handle) => _handle = handle;

    /// <summary>ABI version reported by the loaded cutcap_engine.dll.</summary>
    public static uint LibraryAbiVersion => NativeMethods.AbiVersionOfLibrary();

    public bool IsDisposed => _handle.IsClosed;

    /// <summary>Last immutable view read on the owning thread; diagnostics need no extra ABI polling.</summary>
    public SessionView? LastReadView { get; private set; }
    /// <summary>Diagnostic byte counts; pixels are copied separately.</summary>
    public int LastViewBytes { get; private set; }
    public ulong TimelineReadCount { get; private set; }
    public int LastTimelineBytes { get; private set; }

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

    public void SetPreviewMode(PreviewMode mode) => Check(NativeMethods.SetPreviewMode(Handle, (uint)mode));
    /// <summary>Exact project ticks; evaluated in Rust. Also selects Timeline mode.</summary>
    public void SeekTimeline(long ticks) => Check(NativeMethods.SeekTimeline(Handle, ticks));
    public void SeekTimelineMilliseconds(long milliseconds) => Check(NativeMethods.SeekTimelineMilliseconds(Handle, milliseconds));
    public unsafe void SeekTimelineText(string text)
    {
        var bytes = Encoding.UTF8.GetBytes(text);
        fixed (byte* pointer = bytes) { Check(NativeMethods.SeekTimelineText(Handle, pointer, (nuint)bytes.Length)); }
    }
    public void CancelTimelinePreview() => Check(NativeMethods.CancelTimelinePreview(Handle));

    /// <summary>
    /// Appends a ready video source to the end of a video track. Acceptance
    /// only: the engine validates and reports rejections in the view status.
    /// </summary>
    /// <summary>
    /// Replaces the filmstrip thumbnail demand (visible samples first, at most
    /// 128). Samples no longer wanted are dropped or cancelled in Rust.
    /// </summary>
    public unsafe void RequestThumbnails(IReadOnlyList<ThumbnailWant> wants)
    {
        var json = JsonSerializer.SerializeToUtf8Bytes(wants, ViewJsonContext.Default.IReadOnlyListThumbnailWant);
        fixed (byte* pointer = json)
        {
            Check(NativeMethods.RequestThumbnails(Handle, pointer, (nuint)json.Length));
        }
    }

    /// <summary>Announces the next completed thumbnail; false when none is waiting.</summary>
    public bool TryNextThumbnail(out NativeThumbnailInfo info)
    {
        var status = NativeMethods.NextThumbnail(Handle, out info);
        if (status == NativeStatus.NoFrame)
        {
            return false;
        }
        Check(status);
        if (info.Status == 0 && (info.Width == 0 || info.Height == 0 || info.Stride < info.Width * 4))
        {
            throw new EngineException(NativeStatus.Failed, "The engine described an invalid thumbnail.");
        }
        return true;
    }

    /// <summary>Copies the announced thumbnail into caller memory (for example a locked bitmap).</summary>
    public unsafe bool CopyThumbnail(ulong token, nint destination, int length, int stride)
    {
        ArgumentOutOfRangeException.ThrowIfNegative(length);
        ArgumentOutOfRangeException.ThrowIfNegative(stride);
        var status = NativeMethods.CopyThumbnail(Handle, token, (byte*)destination, (nuint)length, (nuint)stride);
        if (status is NativeStatus.StaleFrame or NativeStatus.NoFrame)
        {
            return false;
        }
        Check(status);
        return true;
    }

    public unsafe bool CopyThumbnail(ulong token, Span<byte> destination, int stride)
    {
        fixed (byte* pointer = destination)
        {
            return CopyThumbnail(token, (nint)pointer, destination.Length, stride);
        }
    }

    /// <summary>
    /// Replaces the waveform demand (visible chunks first, at most 128). Chunks
    /// no longer wanted are dropped or cancelled in Rust (ABI 9).
    /// </summary>
    public unsafe void RequestWaveforms(IReadOnlyList<WaveformWant> wants)
    {
        var json = JsonSerializer.SerializeToUtf8Bytes(wants, ViewJsonContext.Default.IReadOnlyListWaveformWant);
        fixed (byte* pointer = json)
        {
            Check(NativeMethods.RequestWaveforms(Handle, pointer, (nuint)json.Length));
        }
    }

    /// <summary>Announces the next completed waveform chunk; false when none is waiting.</summary>
    public bool TryNextWaveform(out NativeWaveformInfo info)
    {
        var status = NativeMethods.NextWaveform(Handle, out info);
        if (status == NativeStatus.NoFrame)
        {
            return false;
        }
        Check(status);
        if (info.Status == 0 && (info.Buckets == 0 || info.Buckets > 1 << 20 || info.Rate == 0 || info.BucketFrames == 0))
        {
            throw new EngineException(NativeStatus.Failed, "The engine described an invalid waveform.");
        }
        return true;
    }

    /// <summary>Copies the announced chunk as interleaved (min, max) pairs; false when stale or failed.</summary>
    public unsafe bool CopyWaveform(ulong token, Span<float> destination)
    {
        fixed (float* pointer = destination)
        {
            var status = NativeMethods.CopyWaveform(Handle, token, pointer, (nuint)destination.Length);
            if (status is NativeStatus.StaleFrame or NativeStatus.NoFrame)
            {
                return false;
            }
            Check(status);
            return true;
        }
    }

    /// <summary>Places the whole Ready source at a position (drag-and-drop). Acceptance only.</summary>
    public unsafe void PlaceClip(string mediaId, string trackId, TimelinePosition position)
    {
        var media = Ids(mediaId);
        var track = Ids(trackId);
        fixed (byte* m = media)
        fixed (byte* t = track)
        {
            Check(NativeMethods.PlaceClip(Handle, m, (nuint)media.Length, t, (nuint)track.Length, position.Value, position.Unit));
        }
    }

    /// <summary>Moves a clip so its <paramref name="anchor"/> edge lands at <paramref name="position"/>.</summary>
    public unsafe void MoveClip(string clipId, string trackId, TimelinePosition position, ClipEdge anchor)
    {
        var clip = Ids(clipId);
        var track = Ids(trackId);
        fixed (byte* c = clip)
        fixed (byte* t = track)
        {
            Check(NativeMethods.MoveClip(Handle, c, (nuint)clip.Length, t, (nuint)track.Length,
                position.Value, position.Unit, (uint)anchor));
        }
    }

    /// <summary>Moves one clip boundary within its source range.</summary>
    public unsafe void TrimClip(string clipId, ClipEdge edge, TimelinePosition position)
    {
        var clip = Ids(clipId);
        fixed (byte* c = clip)
        {
            Check(NativeMethods.TrimClip(Handle, c, (nuint)clip.Length, (uint)edge, position.Value, position.Unit));
        }
    }

    public unsafe void SplitClip(string clipId, TimelinePosition position)
    {
        var clip = Ids(clipId);
        fixed (byte* c = clip)
        {
            Check(NativeMethods.SplitClip(Handle, c, (nuint)clip.Length, position.Value, position.Unit));
        }
    }

    /// <summary>Splits at the Rust-owned timeline playhead; stops timeline playback like any structural edit.</summary>
    public unsafe void SplitClipAtPlayhead(string clipId)
    {
        var clip = Ids(clipId);
        fixed (byte* c = clip)
        {
            Check(NativeMethods.SplitClipAtPlayhead(Handle, c, (nuint)clip.Length));
        }
    }

    private static byte[] Ids(string id)
    {
        ArgumentException.ThrowIfNullOrEmpty(id);
        return Encoding.UTF8.GetBytes(id);
    }

    public unsafe void InsertClip(string mediaId, string trackId)
    {
        ArgumentException.ThrowIfNullOrEmpty(mediaId);
        ArgumentException.ThrowIfNullOrEmpty(trackId);
        var media = Encoding.UTF8.GetBytes(mediaId);
        var track = Encoding.UTF8.GetBytes(trackId);
        fixed (byte* mediaPointer = media)
        fixed (byte* trackPointer = track)
        {
            Check(NativeMethods.InsertClip(Handle, mediaPointer, (nuint)media.Length, trackPointer, (nuint)track.Length));
        }
    }

    public unsafe void RemoveClip(string id)
    {
        var bytes = Encoding.UTF8.GetBytes(id);
        fixed (byte* pointer = bytes) { Check(NativeMethods.RemoveClip(Handle, pointer, (nuint)bytes.Length)); }
    }

    /// <summary>
    /// Places one chosen audio stream as an independent clip. Without a track,
    /// Rust uses the first audio track free for the whole interval.
    /// </summary>
    public unsafe void PlaceAudioStream(string mediaId, uint stream, string? trackId, TimelinePosition position)
    {
        var media = Ids(mediaId);
        var track = trackId is null ? [] : Ids(trackId);
        fixed (byte* m = media)
        fixed (byte* t = track)
        {
            Check(NativeMethods.PlaceAudioStream(Handle, m, (nuint)media.Length, stream, t, (nuint)track.Length, position.Value, position.Unit));
        }
    }

    /// <summary>Removes the link of a linked pair (one undo step, no timing change).</summary>
    public unsafe void UnlinkClip(string clipId)
    {
        var clip = Ids(clipId);
        fixed (byte* c = clip) { Check(NativeMethods.UnlinkClip(Handle, c, (nuint)clip.Length)); }
    }

    /// <summary>Links two compatible unlinked clips; Rust validates them.</summary>
    public unsafe void LinkClips(string clipId, string partnerId)
    {
        var clip = Ids(clipId);
        var partner = Ids(partnerId);
        fixed (byte* c = clip)
        fixed (byte* p = partner)
        {
            Check(NativeMethods.LinkClips(Handle, c, (nuint)clip.Length, p, (nuint)partner.Length));
        }
    }

    /// <summary>One undo step; timeline playback continues.</summary>
    public unsafe void SetTrackMute(string trackId, bool muted)
    {
        var track = Ids(trackId);
        fixed (byte* t = track) { Check(NativeMethods.SetTrackMute(Handle, t, (nuint)track.Length, muted ? 1u : 0u)); }
    }

    /// <summary>
    /// Phase 5A: accepts an export of the project as it is now. Acceptance is
    /// not completion; <c>Export.Job</c> in the view reports the rest.
    /// </summary>
    public unsafe void StartExport(string path, string? trackId, bool allowOverwrite, bool acknowledgeOmitted)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        var track = trackId is null ? [] : Ids(trackId);
        var flags = (allowOverwrite ? 1u : 0u) | (acknowledgeOmitted ? 2u : 0u);
        fixed (char* p = path)
        fixed (byte* t = track)
        {
            Check(NativeMethods.StartExport(Handle, p, (nuint)path.Length, t, (nuint)track.Length, flags));
        }
    }

    /// <summary>Asks the named export to stop. Never waits for it.</summary>
    public unsafe void CancelExport(string jobId)
    {
        var job = Ids(jobId);
        fixed (byte* j = job) { Check(NativeMethods.CancelExport(Handle, j, (nuint)job.Length)); }
    }

    /// <summary>One undo step; timeline playback continues.</summary>
    public unsafe void SetTrackSolo(string trackId, bool soloed)
    {
        var track = Ids(trackId);
        fixed (byte* t = track) { Check(NativeMethods.SetTrackSolo(Handle, t, (nuint)track.Length, soloed ? 1u : 0u)); }
    }

    /// <summary>A volume drag in hundredths of a dB: heard live, never in history.</summary>
    public unsafe void PreviewTrackGain(string trackId, int cdb)
    {
        var track = Ids(trackId);
        fixed (byte* t = track) { Check(NativeMethods.PreviewTrackGain(Handle, t, (nuint)track.Length, cdb)); }
    }

    /// <summary>Ends a volume drag with one undoable change.</summary>
    public unsafe void CommitTrackGain(string trackId, int cdb)
    {
        var track = Ids(trackId);
        fixed (byte* t = track) { Check(NativeMethods.CommitTrackGain(Handle, t, (nuint)track.Length, cdb)); }
    }

    /// <summary>Abandons a volume drag; the stored value applies again.</summary>
    public void CancelTrackGain() => Check(NativeMethods.CancelTrackGain(Handle));

    /// <summary>Transient selection; never dirties the document or enters history.</summary>
    public unsafe void SelectClip(string id)
    {
        var bytes = Encoding.UTF8.GetBytes(id);
        fixed (byte* pointer = bytes) { Check(NativeMethods.SelectClip(Handle, pointer, (nuint)bytes.Length)); }
    }

    /// <summary>Rust-owned clip-edit history. "Nothing to undo" arrives as a view status.</summary>
    public void Undo() => Check(NativeMethods.Undo(Handle));
    public void Redo() => Check(NativeMethods.Redo(Handle));

    /// <summary>Acceptance only. Observe Project.Document for completion and dirty state.</summary>
    public void SaveProject() => Check(NativeMethods.SaveProject(Handle));

    public unsafe void SaveProjectAs(string path)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        fixed (char* pointer = path) { Check(NativeMethods.SaveProjectAs(Handle, pointer, (nuint)path.Length)); }
    }

    public unsafe void OpenProject(string path)
    {
        ArgumentException.ThrowIfNullOrEmpty(path);
        fixed (char* pointer = path) { Check(NativeMethods.OpenProject(Handle, pointer, (nuint)path.Length)); }
    }

    public unsafe void RelinkMedia(string id, string path)
    {
        ArgumentException.ThrowIfNullOrEmpty(id);
        ArgumentException.ThrowIfNullOrEmpty(path);
        var bytes = Encoding.UTF8.GetBytes(id);
        fixed (byte* idPointer = bytes)
        fixed (char* pathPointer = path)
        { Check(NativeMethods.RelinkMedia(Handle, idPointer, (nuint)bytes.Length, pathPointer, (nuint)path.Length)); }
    }

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
    public void PlayTimeline() => Check(NativeMethods.PlayTimeline(Handle));
    public void PauseTimeline() => Check(NativeMethods.PauseTimeline(Handle));
    public void ResumeTimeline() => PlayTimeline();
    public void StopTimeline() => Check(NativeMethods.StopTimeline(Handle));
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
            LastViewBytes = (int)length;
            LastReadView = JsonSerializer.Deserialize(_viewBuffer.AsSpan(0, (int)length), ViewJsonContext.Default.SessionView)
                ?? throw new EngineException(NativeStatus.Failed, "The engine returned an empty view.");
            return LastReadView;
        }
    }

    /// <summary>
    /// Reads the clip list. It is a separate document from the view so playback
    /// frames never re-send it: call this only when
    /// <see cref="SessionView.TimelineGeneration"/> differs from the last
    /// <see cref="TimelineView.Generation"/> read.
    /// </summary>
    public unsafe TimelineView ReadTimeline()
    {
        while (true)
        {
            NativeStatus status;
            nuint length;
            fixed (byte* pointer = _timelineBuffer)
            {
                status = NativeMethods.Timeline(Handle, pointer, (nuint)_timelineBuffer.Length, out length);
            }
            if (status == NativeStatus.BufferTooSmall)
            {
                if (length > MaxTimelineBytes)
                {
                    throw new EngineException(NativeStatus.Failed, "The timeline exceeded its size limit.");
                }
                _timelineBuffer = new byte[(int)length];
                continue;
            }
            Check(status);
            if (length > (nuint)_timelineBuffer.Length)
            {
                throw new EngineException(NativeStatus.Failed, "The engine reported an invalid timeline length.");
            }
            LastTimelineBytes = (int)length;
            TimelineReadCount++;
            return JsonSerializer.Deserialize(_timelineBuffer.AsSpan(0, (int)length), ViewJsonContext.Default.TimelineView)
                ?? throw new EngineException(NativeStatus.Failed, "The engine returned an empty timeline.");
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

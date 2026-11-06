using System.Runtime.InteropServices;

namespace CutCap.Interop;

/// <summary>Status codes returned by every <c>cutcap_*</c> export (see docs/NATIVE_ABI.md).</summary>
public enum NativeStatus
{
    Ok = 0,
    InvalidArgument = 1,
    InvalidHandle = 2,
    AbiMismatch = 3,
    BufferTooSmall = 4,
    NoFrame = 5,
    StaleFrame = 6,
    Busy = 7,
    Failed = 8,
    Panic = 9,
}

/// <summary>Mirror of Rust <c>CutCapFrameInfo</c>: 24 bytes, 8-byte aligned.</summary>
[StructLayout(LayoutKind.Sequential)]
public struct NativeFrameInfo
{
    public ulong Generation;
    public uint Width;
    public uint Height;
    public uint Stride;
    public uint PixelFormat;
}

/// <summary>
/// Raw C ABI. Only <see cref="EngineSession"/> calls these; nothing else in the
/// frontend sees native pointers. All strings are passed as explicit
/// pointer/length pairs (UTF-8, or UTF-16 for paths) with no terminators.
/// </summary>
internal static unsafe partial class NativeMethods
{
    private const string Library = "cutcap_engine";

    /// <summary>The ABI this frontend was written against.</summary>
    public const uint AbiVersion = 7;
    public const uint PixelRgba8Opaque = 1;

    [LibraryImport(Library, EntryPoint = "cutcap_abi_version")]
    public static partial uint AbiVersionOfLibrary();

    [LibraryImport(Library, EntryPoint = "cutcap_last_error")]
    public static partial NativeStatus LastError(byte* buffer, nuint capacity, out nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_create")]
    public static partial NativeStatus SessionCreate(uint abiVersion, out ulong handle);

    [LibraryImport(Library, EntryPoint = "cutcap_session_destroy")]
    public static partial NativeStatus SessionDestroy(ulong handle);

    [LibraryImport(Library, EntryPoint = "cutcap_session_new_project")]
    public static partial NativeStatus NewProject(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_set_preview_mode")]
    public static partial NativeStatus SetPreviewMode(SessionSafeHandle session, uint mode);
    [LibraryImport(Library, EntryPoint = "cutcap_session_seek_timeline")]
    public static partial NativeStatus SeekTimeline(SessionSafeHandle session, long ticks);
    [LibraryImport(Library, EntryPoint = "cutcap_session_seek_timeline_ms")]
    public static partial NativeStatus SeekTimelineMilliseconds(SessionSafeHandle session, long milliseconds);
    [LibraryImport(Library, EntryPoint = "cutcap_session_place_clip")]
    public static partial NativeStatus PlaceClip(SessionSafeHandle session, byte* media, nuint mediaLength,
        byte* track, nuint trackLength, long position, uint unit);

    [LibraryImport(Library, EntryPoint = "cutcap_session_move_clip")]
    public static partial NativeStatus MoveClip(SessionSafeHandle session, byte* clip, nuint clipLength,
        byte* track, nuint trackLength, long position, uint unit, uint anchor);

    [LibraryImport(Library, EntryPoint = "cutcap_session_trim_clip")]
    public static partial NativeStatus TrimClip(SessionSafeHandle session, byte* clip, nuint clipLength,
        uint edge, long position, uint unit);

    [LibraryImport(Library, EntryPoint = "cutcap_session_split_clip")]
    public static partial NativeStatus SplitClip(SessionSafeHandle session, byte* clip, nuint clipLength,
        long position, uint unit);

    [LibraryImport(Library, EntryPoint = "cutcap_session_split_clip_at_playhead")]
    public static partial NativeStatus SplitClipAtPlayhead(SessionSafeHandle session, byte* clip, nuint clipLength);

    [LibraryImport(Library, EntryPoint = "cutcap_session_seek_timeline_text")]
    public static partial NativeStatus SeekTimelineText(SessionSafeHandle session, byte* text, nuint length);
    [LibraryImport(Library, EntryPoint = "cutcap_session_cancel_timeline_preview")]
    public static partial NativeStatus CancelTimelinePreview(SessionSafeHandle session);
    [LibraryImport(Library, EntryPoint = "cutcap_session_play_timeline")]
    public static partial NativeStatus PlayTimeline(SessionSafeHandle session);
    [LibraryImport(Library, EntryPoint = "cutcap_session_pause_timeline")]
    public static partial NativeStatus PauseTimeline(SessionSafeHandle session);
    [LibraryImport(Library, EntryPoint = "cutcap_session_stop_timeline")]
    public static partial NativeStatus StopTimeline(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_save_project")]
    public static partial NativeStatus SaveProject(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_save_project_as")]
    public static partial NativeStatus SaveProjectAs(SessionSafeHandle session, char* path, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_open_project")]
    public static partial NativeStatus OpenProject(SessionSafeHandle session, char* path, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_relink_media")]
    public static partial NativeStatus RelinkMedia(SessionSafeHandle session, byte* id, nuint idLength, char* path, nuint pathLength);

    [LibraryImport(Library, EntryPoint = "cutcap_session_add_track")]
    public static partial NativeStatus AddTrack(SessionSafeHandle session, uint kind);

    [LibraryImport(Library, EntryPoint = "cutcap_session_select_track")]
    public static partial NativeStatus SelectTrack(SessionSafeHandle session, byte* id, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_clear_selection")]
    public static partial NativeStatus ClearSelection(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_select_media")]
    public static partial NativeStatus SelectMedia(SessionSafeHandle session, byte* id, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_import")]
    public static partial NativeStatus Import(SessionSafeHandle session, char* path, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_cancel_import")]
    public static partial NativeStatus CancelImport(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_seek")]
    public static partial NativeStatus Seek(SessionSafeHandle session, long milliseconds);

    [LibraryImport(Library, EntryPoint = "cutcap_session_seek_text")]
    public static partial NativeStatus SeekText(SessionSafeHandle session, byte* text, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_cancel_preview")]
    public static partial NativeStatus CancelPreview(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_play")]
    public static partial NativeStatus Play(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_pause")]
    public static partial NativeStatus Pause(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_stop")]
    public static partial NativeStatus Stop(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_pump")]
    public static partial NativeStatus Pump(SessionSafeHandle session, out uint changed);

    [LibraryImport(Library, EntryPoint = "cutcap_session_view")]
    public static partial NativeStatus View(SessionSafeHandle session, byte* buffer, nuint capacity, out nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_timeline")]
    public static partial NativeStatus Timeline(SessionSafeHandle session, byte* buffer, nuint capacity, out nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_insert_clip")]
    public static partial NativeStatus InsertClip(
        SessionSafeHandle session, byte* media, nuint mediaLength, byte* track, nuint trackLength);

    [LibraryImport(Library, EntryPoint = "cutcap_session_remove_clip")]
    public static partial NativeStatus RemoveClip(SessionSafeHandle session, byte* id, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_select_clip")]
    public static partial NativeStatus SelectClip(SessionSafeHandle session, byte* id, nuint length);

    [LibraryImport(Library, EntryPoint = "cutcap_session_undo")]
    public static partial NativeStatus Undo(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_redo")]
    public static partial NativeStatus Redo(SessionSafeHandle session);

    [LibraryImport(Library, EntryPoint = "cutcap_session_frame_info")]
    public static partial NativeStatus FrameInfo(SessionSafeHandle session, out NativeFrameInfo info);

    [LibraryImport(Library, EntryPoint = "cutcap_session_copy_frame")]
    public static partial NativeStatus CopyFrame(
        SessionSafeHandle session, ulong generation, byte* destination, nuint length, nuint stride);
}

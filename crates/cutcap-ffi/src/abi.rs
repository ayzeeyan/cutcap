//! Versioned C ABI for the Avalonia frontend. See docs/NATIVE_ABI.md.
//!
//! Rules enforced here:
//! - Sessions are opaque `u64` handles in a registry, never raw pointers, so a
//!   stale, forged or double-destroyed handle returns `CUTCAP_INVALID_HANDLE`
//!   instead of touching freed memory.
//! - Every export catches panics; no unwind crosses the boundary. A panic poisons
//!   only that session, whose later calls return `CUTCAP_PANIC`.
//! - The frontend owns every buffer it passes; Rust only reads or writes it for
//!   the duration of the call and validates every length first. Rust owns the
//!   decoded frame; the frontend copies it out and never holds a Rust pointer.
//! - No callbacks: the frontend pulls state, so no delegate can be collected
//!   while native code still references it.
#![allow(unsafe_code)]

use crate::session::{Session, SessionError};
use cutcap_contracts::{ClipId, MediaId, TrackId, TrackKind};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Bump on any incompatible change to a signature, struct layout, status code
/// or view JSON field the frontend depends on.
/// ABI 4 (Phase 3B): clip insert/remove/select, undo/redo, the separately
/// fetched timeline document and the view's `timeline_generation`/`history`.
pub const ABI_VERSION: u32 = 4;

pub const CUTCAP_OK: i32 = 0;
pub const CUTCAP_INVALID_ARGUMENT: i32 = 1;
pub const CUTCAP_INVALID_HANDLE: i32 = 2;
pub const CUTCAP_ABI_MISMATCH: i32 = 3;
pub const CUTCAP_BUFFER_TOO_SMALL: i32 = 4;
pub const CUTCAP_NO_FRAME: i32 = 5;
pub const CUTCAP_STALE_FRAME: i32 = 6;
pub const CUTCAP_BUSY: i32 = 7;
pub const CUTCAP_FAILED: i32 = 8;
pub const CUTCAP_PANIC: i32 = 9;

/// `pixel_format` value: packed RGBA8, opaque alpha, top row first.
pub const CUTCAP_PIXEL_RGBA8_OPAQUE: u32 = 1;
/// Upper bound on text the frontend may pass in one call.
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CutCapFrameInfo {
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    /// Bytes per source row (`width * 4`).
    pub stride: u32,
    pub pixel_format: u32,
}

type Shared = Arc<Mutex<Session>>;

static SESSIONS: Mutex<BTreeMap<u64, Shared>> = Mutex::new(BTreeMap::new());
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

struct Fail {
    code: i32,
    message: String,
}

impl Fail {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    fn argument(message: &str) -> Self {
        Self::new(CUTCAP_INVALID_ARGUMENT, message)
    }
}

impl From<SessionError> for Fail {
    fn from(error: SessionError) -> Self {
        let code = match &error {
            SessionError::Engine(engine) if engine.code == cutcap_contracts::ErrorCode::Busy => {
                CUTCAP_BUSY
            }
            SessionError::Invalid(_) => CUTCAP_INVALID_ARGUMENT,
            _ => CUTCAP_FAILED,
        };
        Self::new(code, error.to_string())
    }
}

fn set_last_error(message: &str) {
    LAST_ERROR.with(|last| {
        let mut last = last.borrow_mut();
        last.clear();
        last.push_str(message);
    });
}

/// Runs an export body: catches panics and records the error message.
fn guard(body: impl FnOnce() -> Result<(), Fail>) -> i32 {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => {
            set_last_error("");
            CUTCAP_OK
        }
        Ok(Err(fail)) => {
            set_last_error(&fail.message);
            fail.code
        }
        Err(_) => {
            set_last_error("CutCap's engine hit an internal error. Restart CutCap.");
            CUTCAP_PANIC
        }
    }
}

fn registry() -> Result<std::sync::MutexGuard<'static, BTreeMap<u64, Shared>>, Fail> {
    SESSIONS
        .lock()
        .map_err(|_| Fail::new(CUTCAP_PANIC, "The session registry is poisoned."))
}

fn register(session: Session) -> Result<u64, Fail> {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    registry()?.insert(handle, Arc::new(Mutex::new(session)));
    Ok(handle)
}

fn with_session<T>(
    handle: u64,
    body: impl FnOnce(&mut Session) -> Result<T, Fail>,
) -> Result<T, Fail> {
    let shared = registry()?
        .get(&handle)
        .cloned()
        .ok_or_else(|| Fail::new(CUTCAP_INVALID_HANDLE, "Unknown or closed engine session."))?;
    let mut session = shared
        .lock()
        .map_err(|_| Fail::new(CUTCAP_PANIC, "This engine session failed earlier."))?;
    body(&mut session)
}

/// # Safety
/// `ptr` must be valid for reads of `len` bytes for the duration of the call,
/// or `len` must be zero.
unsafe fn input<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], Fail> {
    if len == 0 {
        return Ok(&[]);
    }
    if ptr.is_null() || len > MAX_INPUT_BYTES {
        return Err(Fail::argument("Invalid input buffer."));
    }
    // SAFETY: non-null and bounded; validity for `len` bytes is the caller's contract.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// # Safety
/// As for [`input`]; the bytes must be UTF-8.
unsafe fn input_str<'a>(ptr: *const u8, len: usize) -> Result<&'a str, Fail> {
    // SAFETY: forwarded caller contract.
    let bytes = unsafe { input(ptr, len) }?;
    std::str::from_utf8(bytes).map_err(|_| Fail::argument("Text must be UTF-8."))
}

fn parse_uuid(text: &str) -> Result<uuid::Uuid, Fail> {
    uuid::Uuid::parse_str(text).map_err(|_| Fail::argument("Invalid identifier."))
}

/// # Safety
/// `out` must be null or valid for one write of `T`.
unsafe fn write_out<T>(out: *mut T, value: T) -> Result<(), Fail> {
    if out.is_null() || !out.is_aligned() {
        return Err(Fail::argument("Invalid output pointer."));
    }
    // SAFETY: non-null and aligned; writable for one `T` per the caller's contract.
    unsafe { out.write(value) };
    Ok(())
}

/// Copies `bytes` into a caller buffer, reporting the required length first.
///
/// # Safety
/// `buffer` must be valid for writes of `capacity` bytes (or `capacity` zero);
/// `out_len` must be valid for one write.
unsafe fn copy_text(
    bytes: &[u8],
    buffer: *mut u8,
    capacity: usize,
    out_len: *mut usize,
) -> Result<(), Fail> {
    // SAFETY: forwarded caller contract.
    unsafe { write_out(out_len, bytes.len()) }?;
    if capacity < bytes.len() {
        return Err(Fail::new(
            CUTCAP_BUFFER_TOO_SMALL,
            "Buffer too small; retry with the reported length.",
        ));
    }
    if bytes.is_empty() {
        return Ok(());
    }
    if buffer.is_null() {
        return Err(Fail::argument("Invalid output buffer."));
    }
    // SAFETY: `buffer` is non-null and writable for `capacity >= bytes.len()` bytes;
    // the source is a Rust-owned slice that cannot overlap a caller buffer.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buffer, bytes.len()) };
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn cutcap_abi_version() -> u32 {
    ABI_VERSION
}

/// Copies the calling thread's last error message (UTF-8, no terminator).
///
/// # Safety
/// See [`copy_text`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_last_error(
    buffer: *mut u8,
    capacity: usize,
    out_len: *mut usize,
) -> i32 {
    let message = LAST_ERROR.with(|last| last.borrow().clone());
    // Do not route through `guard`: it would overwrite the message being read.
    match catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: forwarded caller contract.
        unsafe { copy_text(message.as_bytes(), buffer, capacity, out_len) }
    })) {
        Ok(Ok(())) => CUTCAP_OK,
        Ok(Err(fail)) => fail.code,
        Err(_) => CUTCAP_PANIC,
    }
}

/// Starts a session (command worker, decoder, first project).
///
/// # Safety
/// `out_handle` must be valid for one `u64` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_create(abi_version: u32, out_handle: *mut u64) -> i32 {
    guard(|| {
        if abi_version != ABI_VERSION {
            return Err(Fail::new(
                CUTCAP_ABI_MISMATCH,
                format!(
                    "The engine library speaks ABI {ABI_VERSION}; the frontend expects {abi_version}."
                ),
            ));
        }
        init_logging();
        let handle = register(Session::start()?)?;
        // SAFETY: forwarded caller contract.
        if let Err(fail) = unsafe { write_out(out_handle, handle) } {
            // Do not leak a session the caller cannot name.
            if let Some(session) = registry()?.remove(&handle) {
                drop(session);
            }
            return Err(fail);
        }
        Ok(())
    })
}

/// Shuts the session down (cancels probe/decode, joins workers) and releases it.
/// A second call with the same handle returns `CUTCAP_INVALID_HANDLE`.
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_destroy(handle: u64) -> i32 {
    guard(|| {
        let shared = registry()?
            .remove(&handle)
            .ok_or_else(|| Fail::new(CUTCAP_INVALID_HANDLE, "Unknown or closed engine session."))?;
        // Waits for any in-flight call on another thread, then stops the workers.
        let result = match shared.lock() {
            Ok(mut session) => session.shutdown().map_err(Fail::from),
            Err(_) => Err(Fail::new(
                CUTCAP_PANIC,
                "This engine session failed earlier.",
            )),
        };
        drop(shared);
        result
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_new_project(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.new_project()?)))
}

/// `kind`: 0 video, 1 audio.
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_add_track(handle: u64, kind: u32) -> i32 {
    guard(|| {
        let kind = match kind {
            0 => TrackKind::Video,
            1 => TrackKind::Audio,
            _ => return Err(Fail::argument("Unknown track kind.")),
        };
        with_session(handle, |s| Ok(s.add_track(kind)?))
    })
}

/// Selects a track by its id (UUID text from the view).
///
/// # Safety
/// `id` must be valid for reads of `id_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_select_track(
    handle: u64,
    id: *const u8,
    id_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let text = unsafe { input_str(id, id_len) }?;
        let id = TrackId::try_from(parse_uuid(text)?)
            .map_err(|_| Fail::argument("Invalid track identifier."))?;
        with_session(handle, |s| Ok(s.select_track(id)?))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_clear_selection(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.clear_selection()?)))
}

/// # Safety
/// `ptr` must be valid for reads of `len` bytes.
unsafe fn clip_id(ptr: *const u8, len: usize) -> Result<ClipId, Fail> {
    // SAFETY: forwarded caller contract.
    let text = unsafe { input_str(ptr, len) }?;
    ClipId::try_from(parse_uuid(text)?).map_err(|_| Fail::argument("Invalid clip identifier."))
}

/// Phase 3B (ABI 4): appends a ready video source to the end of a
/// video track. Accepted means queued; the clip appears in the next view.
///
/// # Safety
/// `media`/`track` must be valid for reads of their lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_insert_clip(
    handle: u64,
    media: *const u8,
    media_len: usize,
    track: *const u8,
    track_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let media = unsafe { input_str(media, media_len) }?;
        let media = MediaId::try_from(parse_uuid(media)?)
            .map_err(|_| Fail::argument("Invalid media identifier."))?;
        // SAFETY: forwarded caller contract.
        let track = unsafe { input_str(track, track_len) }?;
        let track = TrackId::try_from(parse_uuid(track)?)
            .map_err(|_| Fail::argument("Invalid track identifier."))?;
        with_session(handle, |s| Ok(s.insert_clip(media, track)?))
    })
}

/// Removes a clip (the media asset and file stay). Undoable.
///
/// # Safety
/// `id` must be valid for reads of `id_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_remove_clip(
    handle: u64,
    id: *const u8,
    id_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let id = unsafe { clip_id(id, id_len) }?;
        with_session(handle, |s| Ok(s.remove_clip(id)?))
    })
}

/// Selects a clip on the timeline. Selection never dirties the document.
///
/// # Safety
/// `id` must be valid for reads of `id_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_select_clip(
    handle: u64,
    id: *const u8,
    id_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let id = unsafe { clip_id(id, id_len) }?;
        with_session(handle, |s| Ok(s.select_clip(id)?))
    })
}

/// Undoes the newest clip edit; "nothing to undo" arrives as a view status.
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_undo(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.undo()?)))
}

#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_redo(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.redo()?)))
}

/// Shows a registered media asset in the source viewer.
///
/// # Safety
/// `id` must be valid for reads of `id_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_select_media(
    handle: u64,
    id: *const u8,
    id_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let text = unsafe { input_str(id, id_len) }?;
        let id = MediaId::try_from(parse_uuid(text)?)
            .map_err(|_| Fail::argument("Invalid media identifier."))?;
        with_session(handle, |s| Ok(s.select_media(id)?))
    })
}

/// Probes and registers a local file. `path` is UTF-16 (Windows wide string,
/// no terminator), so any Windows path round-trips exactly.
///
/// # Safety
/// `path` must be valid, 2-byte aligned reads of `path_len` UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_import(
    handle: u64,
    path: *const u16,
    path_len: usize,
) -> i32 {
    guard(|| {
        if path.is_null() || !path.is_aligned() || path_len == 0 || path_len > 32_767 {
            return Err(Fail::argument("Invalid file path."));
        }
        // SAFETY: non-null, aligned and bounded; readable per the caller's contract.
        let units = unsafe { std::slice::from_raw_parts(path, path_len) };
        let path = wide_path(units)?;
        with_session(handle, |s| Ok(s.import(path)?))
    })
}

#[cfg(windows)]
fn wide_path(units: &[u16]) -> Result<PathBuf, Fail> {
    use std::os::windows::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_wide(units)))
}

/// Enqueue Save to the current document path.
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_save_project(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.save_project(None)?)))
}

/// Enqueue Save As. Completion/dirty state is reported in project.document.
/// # Safety
/// `path` is valid for `path_len` aligned UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_save_project_as(
    handle: u64,
    path: *const u16,
    path_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract; helper validates bounds/alignment.
        let path = unsafe { input_path(path, path_len) }?;
        with_session(handle, |s| Ok(s.save_project(Some(path))?))
    })
}

/// Open/validate off-thread, then atomically adopt. A failure preserves state.
/// # Safety
/// `path` is valid for `path_len` aligned UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_open_project(
    handle: u64,
    path: *const u16,
    path_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let path = unsafe { input_path(path, path_len) }?;
        with_session(handle, |s| Ok(s.open_project(path)?))
    })
}

/// Re-probe and compare original identity before changing a media reference.
/// # Safety
/// `id` is valid for `id_len` bytes, `path` for `path_len` aligned UTF-16 units.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_relink_media(
    handle: u64,
    id: *const u8,
    id_len: usize,
    path: *const u16,
    path_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller buffer contracts, bounded before access.
        let text = unsafe { input_str(id, id_len) }?;
        let id = MediaId::try_from(parse_uuid(text)?)
            .map_err(|_| Fail::argument("Invalid media identifier."))?;
        // SAFETY: forwarded caller contract.
        let path = unsafe { input_path(path, path_len) }?;
        with_session(handle, |s| Ok(s.relink_media(id, path)?))
    })
}

unsafe fn input_path(path: *const u16, path_len: usize) -> Result<PathBuf, Fail> {
    if path.is_null() || !path.is_aligned() || path_len == 0 || path_len > 32_767 {
        return Err(Fail::argument("Invalid project/source path."));
    }
    // SAFETY: checked non-null, alignment and bounded length, caller supplies readable memory.
    let units = unsafe { std::slice::from_raw_parts(path, path_len) };
    if units.contains(&0) {
        return Err(Fail::argument("A path cannot contain a null character."));
    }
    wide_path(units)
}

#[cfg(not(windows))]
fn wide_path(units: &[u16]) -> Result<PathBuf, Fail> {
    String::from_utf16(units)
        .map(PathBuf::from)
        .map_err(|_| Fail::argument("Invalid file path."))
}

#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_cancel_import(handle: u64) -> i32 {
    guard(|| {
        with_session(handle, |s| {
            s.cancel_import();
            Ok(())
        })
    })
}

/// Accepts source Play/Resume; decoding and scheduling remain asynchronous.
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_play(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.play()?)))
}
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_pause(handle: u64) -> i32 {
    guard(|| {
        with_session(handle, |s| {
            s.pause();
            Ok(())
        })
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_stop(handle: u64) -> i32 {
    guard(|| with_session(handle, |s| Ok(s.stop()?)))
}

/// Requests the first frame at or after `milliseconds` into the selected source.
#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_seek(handle: u64, milliseconds: i64) -> i32 {
    guard(|| {
        if !(0..=cutcap_contracts::MAX_MEDIA_SECONDS * 1000).contains(&milliseconds) {
            return Err(Fail::argument(
                "Source position must be between zero and 24 hours.",
            ));
        }
        with_session(handle, |s| {
            if !s.source_ready() {
                return Err(Fail::argument("Select a source before seeking."));
            }
            s.seek(milliseconds);
            Ok(())
        })
    })
}

/// Parses go-to text (`1500`, `1.5s`, `00:01.500`) and seeks.
///
/// # Safety
/// `text` must be valid for reads of `text_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_seek_text(
    handle: u64,
    text: *const u8,
    text_len: usize,
) -> i32 {
    guard(|| {
        // SAFETY: forwarded caller contract.
        let text = unsafe { input_str(text, text_len) }?;
        with_session(handle, |s| Ok(s.seek_text(text)?))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn cutcap_session_cancel_preview(handle: u64) -> i32 {
    guard(|| {
        with_session(handle, |s| {
            s.cancel_preview();
            Ok(())
        })
    })
}

/// Drains bounded worker results. Writes 1 to `out_changed` when the view changed.
///
/// # Safety
/// `out_changed` must be valid for one `u32` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_pump(handle: u64, out_changed: *mut u32) -> i32 {
    guard(|| {
        let changed = with_session(handle, |s| Ok(s.pump()))?;
        // SAFETY: forwarded caller contract.
        unsafe { write_out(out_changed, u32::from(changed)) }
    })
}

/// Copies the view JSON (UTF-8). With too small a buffer, writes the required
/// length and returns `CUTCAP_BUFFER_TOO_SMALL`; the same revision is returned
/// on the retry.
///
/// # Safety
/// See [`copy_text`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_view(
    handle: u64,
    buffer: *mut u8,
    capacity: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        with_session(handle, |s| {
            let json = s.view_json()?;
            // SAFETY: forwarded caller contract.
            unsafe { copy_text(json.as_bytes(), buffer, capacity, out_len) }
        })
    })
}

/// Copies the timeline document (UTF-8 JSON `{generation, clips}`), with the
/// same buffer protocol as [`cutcap_session_view`]. Fetch it only when the
/// view's `timeline_generation` differs from the last generation copied.
///
/// # Safety
/// See [`copy_text`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_timeline(
    handle: u64,
    buffer: *mut u8,
    capacity: usize,
    out_len: *mut usize,
) -> i32 {
    guard(|| {
        with_session(handle, |s| {
            let json = s.timeline_json()?;
            // SAFETY: forwarded caller contract.
            unsafe { copy_text(json.as_bytes(), buffer, capacity, out_len) }
        })
    })
}

/// Describes the latest presented frame, or returns `CUTCAP_NO_FRAME`.
///
/// # Safety
/// `out_info` must be valid for one `CutCapFrameInfo` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_frame_info(
    handle: u64,
    out_info: *mut CutCapFrameInfo,
) -> i32 {
    guard(|| {
        let info = with_session(handle, |s| {
            let frame = s
                .frame()
                .ok_or_else(|| Fail::new(CUTCAP_NO_FRAME, "No frame is available."))?;
            Ok(CutCapFrameInfo {
                generation: frame.generation,
                width: frame.image.width(),
                height: frame.image.height(),
                stride: u32::try_from(frame.image.stride())
                    .map_err(|_| Fail::new(CUTCAP_FAILED, "Frame stride overflow."))?,
                pixel_format: CUTCAP_PIXEL_RGBA8_OPAQUE,
            })
        })?;
        // SAFETY: forwarded caller contract.
        unsafe { write_out(out_info, info) }
    })
}

/// Copies frame `generation` row by row into a caller buffer with `dst_stride`
/// bytes per row (for example a locked bitmap). Returns `CUTCAP_STALE_FRAME`
/// if a newer frame replaced it, so a caller never shows mismatched metadata.
///
/// # Safety
/// `dst` must be valid for writes of `dst_len` bytes for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cutcap_session_copy_frame(
    handle: u64,
    generation: u64,
    dst: *mut u8,
    dst_len: usize,
    dst_stride: usize,
) -> i32 {
    guard(|| {
        with_session(handle, |s| {
            let frame = s
                .frame()
                .ok_or_else(|| Fail::new(CUTCAP_NO_FRAME, "No frame is available."))?;
            if frame.generation != generation || !s.copy_allowed(generation) {
                return Err(Fail::new(
                    CUTCAP_STALE_FRAME,
                    "A newer frame replaced this one.",
                ));
            }
            let image = &frame.image;
            let row = image.stride();
            let rows = image.height() as usize;
            let required = dst_stride
                .checked_mul(rows - 1)
                .and_then(|n| n.checked_add(row))
                .ok_or_else(|| Fail::argument("Destination size overflow."))?;
            if dst.is_null()
                || dst_len > isize::MAX as usize
                || dst_stride < row
                || dst_len < required
            {
                return Err(Fail::argument(
                    "Destination buffer is too small for this frame.",
                ));
            }
            // SAFETY: `dst` is non-null and the caller guarantees `dst_len` writable
            // bytes; every row write below stays within `required <= dst_len`.
            let target = unsafe { std::slice::from_raw_parts_mut(dst, dst_len) };
            for (index, source) in image.pixels().chunks_exact(row).enumerate() {
                let start = index * dst_stride;
                target[start..start + row].copy_from_slice(source);
            }
            s.frame_copied(generation)?;
            Ok(())
        })
    })
}

fn init_logging() {
    // A second session or a host-installed subscriber is not an error.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cutcap_engine=info,cutcap_media=info".into()),
        )
        .with_ansi(false)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
pub(crate) fn register_for_test(session: Session) -> u64 {
    register(session).unwrap_or_else(|fail| panic!("{}", fail.message))
}

#[cfg(test)]
pub(crate) fn last_error_for_test() -> String {
    LAST_ERROR.with(|last| last.borrow().clone())
}

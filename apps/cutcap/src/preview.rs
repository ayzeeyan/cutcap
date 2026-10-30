//! The replaceable presentation adapter. No codec or wgpu objects reach Slint.
use crate::AppWindow;

/// Only this adapter writes preview availability. A later frame mailbox and
/// CPU/texture presenters belong behind this module, never inside the model.
pub fn initialize(window: &AppWindow) {
    window.set_preview_status("Preview not connected".into());
    window.set_preview_detail("Media backend unavailable.".into());
}

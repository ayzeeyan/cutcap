//! CutCap engine host for the Avalonia frontend.
//!
//! `session` owns the command worker (`bridge`/`engine`) and the media host
//! (`media`); `view` derives display DTOs; `abi` is the only module with
//! `unsafe` and the only one the frontend links against.
pub mod abi;
mod bridge;
mod engine;
mod history;
pub mod media;
mod persistence;
mod playback;
pub mod session;
mod timeline_plan;
mod timeline_playback;
mod timeline_preview;
pub mod view;

#[cfg(test)]
mod tests;

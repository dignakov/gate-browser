//! Web pages in GPUI. [`WebView`] is a page, the same on every platform; an
//! engine behind it ([`engine`]) renders it. Its events reach the app as
//! [`WebEvent`]s.

mod engine;
mod view;

pub use view::{WebEvent, WebView};

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Something the page waits on the user for: a JavaScript dialog or a
/// permission request. The page stays usable; the question waits in its pane.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    Alert { message: String },
    Confirm { message: String },
    Prompt { message: String, default: String },
    /// "Leave site?" before navigating away or reloading.
    Leave { reload: bool },
    Permission { origin: String, what: Vec<&'static str> },
}

/// What a page's media is doing. Media counts once it played audibly.
/// Ordered by precedence: a page with any media playing is playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Playback {
    #[default]
    None,
    Paused,
    Playing,
}

/// A page zoom step (see [`WebView::zoom`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    Reset,
}

/// How [`init`] went.
#[derive(Debug, PartialEq, Eq)]
pub enum Started {
    /// The engine runs in this process.
    Here,
    /// Another instance already runs on this profile; it was handed this
    /// launch's arguments (see [`on_relaunch`]) and this process should exit.
    Elsewhere,
}

/// Runs this process as one of the engine's helpers when the engine
/// launched it as one. Call first thing in `main`, before anything else
/// starts threads: `Some(code)` means exit with `code`.
pub fn run_helper_process() -> Option<i32> {
    engine::cef::run_helper_process()
}

/// Starts the engine and begins pumping its work from GPUI.
/// `profile` holds cookies, cache and storage; one process at a time owns it.
/// `extensions`: unpacked extension folders to load (see [`extension_id`]).
/// `gpu_frames`: pages hand frames over as GPU buffers where the platform
/// can; otherwise as pixels through the CPU.
pub fn init(profile: &Path, extensions: &[PathBuf], gpu_frames: bool, cx: &mut gpui::App) -> Result<Started> {
    engine::cef::init(profile, extensions, gpu_frames, cx)
}

/// The id Chromium gives the unpacked extension in `dir` (its pages live at
/// `chrome-extension://<id>/`): the first 128 bits of the SHA-256 of its
/// absolute path, as letters a–p. `dir` must be the path [`init`] got.
pub fn extension_id(dir: &Path) -> String {
    engine::cef::extension_id(dir)
}

/// Calls `handler` with the arguments (not switches) of every later launch
/// on the same profile, which hands them here and exits.
pub fn on_relaunch(handler: impl Fn(Vec<String>, &mut gpui::App) + 'static) {
    engine::cef::on_relaunch(handler)
}

/// Shuts the engine down. Every [`WebView`] must have been closed.
pub fn shutdown() {
    engine::cef::shutdown()
}

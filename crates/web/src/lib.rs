//! Web pages in GPUI. [`WebView`] is a page, the same on every platform; an
//! engine behind it ([`engine`]) renders it. Its events reach the app as
//! [`WebEvent`]s.

mod engine;
mod view;

pub use engine::Popup;
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

/// A file a page is downloading, as it goes: into the Downloads folder (see
/// [`download_path`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    /// The engine's, unique within the page that started it.
    pub id: u64,
    /// The file's name, as saved.
    pub name: String,
    pub path: PathBuf,
    pub received: u64,
    pub total: Option<u64>,
    pub state: DownloadState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DownloadState {
    Running,
    Done,
    Cancelled,
    Failed(String),
}

/// A page's right-click menu, drawn by the app (see
/// [`WebView::context_menu`] and [`WebView::choose_context`]).
#[derive(Clone, Debug, PartialEq)]
pub struct ContextMenu {
    /// Where it opens, in window coordinates.
    pub at: gpui::Point<gpui::Pixels>,
    pub items: Vec<ContextMenuItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContextMenuItem {
    Command { label: String, enabled: bool },
    Separator,
}

/// A notification a page shows (`new Notification(...)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    /// The engine's, unique within the page.
    pub id: u64,
    pub title: String,
    pub body: String,
}

/// Where a download named `suggested` is saved: the Downloads folder,
/// numbered past files already there ("report (1).pdf").
pub fn download_path(suggested: &str) -> PathBuf {
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
    // Only a file name: no folders, nothing the file system forbids.
    let name: String = Path::new(suggested)
        .file_name()
        .map(|n| n.to_string_lossy().chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { '_' } else { c }).collect())
        .filter(|n: &String| !n.trim().is_empty() && n != "." && n != "..")
        .unwrap_or_else(|| "download".to_string());
    let first = dir.join(&name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|path| !path.exists())
        .expect("a free name")
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
    match engine::engine() {
        #[cfg(feature = "cef")]
        engine::Engine::Cef => engine::cef::run_helper_process(),
        #[cfg(gate_wpe)]
        engine::Engine::Wpe => None,
        #[cfg(windows)]
        engine::Engine::WebView2 => None,
    }
}

/// Starts the engine and begins pumping its work from GPUI.
/// `profile` holds cookies, cache and storage; one process at a time owns it.
/// `extensions`: unpacked extension folders to load (see [`extension_id`]).
/// `gpu_frames`: pages hand frames over as GPU buffers where the platform
/// can; otherwise as pixels through the CPU. (Both are Chromium's.)
#[cfg_attr(not(feature = "cef"), allow(unused_variables))]
pub fn init(profile: &Path, extensions: &[PathBuf], gpu_frames: bool, cx: &mut gpui::App) -> Result<Started> {
    match engine::engine() {
        #[cfg(feature = "cef")]
        engine::Engine::Cef => engine::cef::init(profile, extensions, gpu_frames, cx),
        #[cfg(gate_wpe)]
        engine::Engine::Wpe => engine::wpe::init(profile, cx),
        #[cfg(windows)]
        engine::Engine::WebView2 => engine::webview2::init(profile, extensions, cx),
    }
}

/// The engine pages run on, in words ("Chromium", "WebKit").
pub fn engine_name() -> &'static str {
    match engine::engine() {
        #[cfg(feature = "cef")]
        engine::Engine::Cef => "Chromium",
        #[cfg(gate_wpe)]
        engine::Engine::Wpe => "WebKit",
        #[cfg(windows)]
        engine::Engine::WebView2 => "Edge",
    }
}

/// The id Chromium gives the unpacked extension in `dir` (its pages live at
/// `chrome-extension://<id>/`): the first 128 bits of the SHA-256 of its
/// absolute path, as letters a–p. `dir` must be the path [`init`] got.
pub fn extension_id(dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    // Chromium hashes the path made absolute with symlinks resolved.
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    hash[..16].iter().flat_map(|b| [b >> 4, b & 15]).map(|n| (b'a' + n) as char).collect()
}

/// Calls `handler` with the arguments (not switches) of every later launch
/// on the same profile, which hands them here and exits.
pub fn on_relaunch(handler: impl Fn(Vec<String>, &mut gpui::App) + 'static) {
    match engine::engine() {
        #[cfg(feature = "cef")]
        engine::Engine::Cef => engine::cef::on_relaunch(handler),
        #[cfg(gate_wpe)]
        engine::Engine::Wpe => engine::wpe::on_relaunch(handler),
        #[cfg(windows)]
        engine::Engine::WebView2 => engine::webview2::on_relaunch(handler),
    }
}

/// Pages prefer a dark color scheme (`prefers-color-scheme: dark`), or a
/// light one. Before the first page or after: later pages follow it too.
pub fn set_dark_mode(dark: bool) {
    match engine::engine() {
        #[cfg(feature = "cef")]
        engine::Engine::Cef => engine::cef::set_dark_mode(dark),
        #[cfg(gate_wpe)]
        engine::Engine::Wpe => engine::wpe::set_dark_mode(dark),
        #[cfg(windows)]
        engine::Engine::WebView2 => engine::webview2::set_dark_mode(dark),
    }
}

/// Gate's own shortcuts (Workspace bindings): engines whose pages hold the
/// keyboard hand these back as [`WebEvent::Key`] instead of keeping them.
/// Whether Ctrl is down now, where GPUI may not hear it: on Windows a page
/// (Edge's own window) takes the keys while it has focus. `None` where
/// GPUI's modifiers are always current.
pub fn control_held() -> Option<bool> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
        // SAFETY: reads this thread's keyboard state.
        Some(unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0)
    }
    #[cfg(not(windows))]
    None
}

pub fn set_app_shortcuts(shortcuts: Vec<gpui::Keystroke>) {
    #[cfg(windows)]
    engine::webview2::set_app_shortcuts(shortcuts);
    #[cfg(not(windows))]
    let _ = shortcuts;
}

/// Shuts the engine down. Every [`WebView`] must have been closed.
pub fn shutdown() {
    match engine::engine() {
        #[cfg(feature = "cef")]
        engine::Engine::Cef => engine::cef::shutdown(),
        #[cfg(gate_wpe)]
        engine::Engine::Wpe => engine::wpe::shutdown(),
        #[cfg(windows)]
        engine::Engine::WebView2 => engine::webview2::shutdown(),
    }
}

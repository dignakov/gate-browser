//! Chromium (CEF) embedded in GPUI.
//!
//! CEF runs windowless: every page renders off screen and hands us its pixels,
//! which [`WebView`] copies into a GPUI atlas tile (only the damaged parts) and
//! paints like any other image. That keeps pages inside GPUI's scene, so they
//! clip, round and sit under floating chrome like the rest of the UI.
//!
//! CEF shares GPUI's main thread: `external_message_pump` lets CEF ask for
//! work (`on_schedule_message_pump_work`) and [`pump`] runs it from a GPUI
//! task. CEF callbacks never touch GPUI state directly; they queue events on
//! the view's shared state and wake the pump, which hands them to the views.

mod app;
mod client;
mod glib;
mod keys;
mod pump;
mod renderer;
mod view;

pub use client::{Ask, Playback};
pub use view::{WebEvent, WebView, Zoom};

use anyhow::{Context as _, Result, bail};
use cef::CefString;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Where the CEF distribution (libcef.so, paks, locales) lives.
const CEF_DIR: &str = env!("OPUS_CEF_DIR");

/// Runs this process as one of CEF's helpers (renderer, GPU, utility) when
/// CEF launched it as one. Call first thing in `main`, before anything else
/// starts threads: `Some(code)` means exit with `code`.
pub fn run_helper_process() -> Option<i32> {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let mut app = app::build();
    // The browser process gets -1: it carries on and calls `init`.
    let code = cef::execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    (code >= 0).then_some(code)
}

/// How [`init`] went.
#[derive(Debug, PartialEq, Eq)]
pub enum Started {
    /// CEF runs in this process.
    Here,
    /// Another instance already runs on this profile; it was handed this
    /// launch's arguments (see [`on_relaunch`]) and this process should exit.
    Elsewhere,
}

/// Starts CEF in the browser process (after [`run_helper_process`] said it is
/// one) and begins pumping its work from GPUI.
/// `profile` holds cookies, cache and storage; one process at a time owns it.
/// `extensions`: unpacked extension folders to load (see [`extension_id`]).
pub fn init(profile: &Path, extensions: &[PathBuf], cx: &mut gpui::App) -> Result<Started> {
    let args = cef::args::Args::new();
    // Read by the app object while CEF parses the command line, below.
    let _ = EXTENSIONS.set(extensions.to_vec());
    let mut app = app::build();
    std::fs::create_dir_all(profile).with_context(|| format!("creating {}", profile.display()))?;
    let exe = std::env::current_exe().context("finding this executable")?;
    let dir = PathBuf::from(CEF_DIR);
    let settings = cef::Settings {
        no_sandbox: 1,
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        multi_threaded_message_loop: 0,
        browser_subprocess_path: path_string(&exe),
        resources_dir_path: path_string(&dir),
        locales_dir_path: path_string(&dir.join("locales")),
        root_cache_path: path_string(profile),
        cache_path: path_string(&profile.join("default")),
        persist_session_cookies: 1,
        log_severity: cef::LogSeverity::WARNING,
        ..Default::default()
    };
    pump::start(cx);
    let ok = cef::initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut());
    if ok == 1 {
        return Ok(Started::Here);
    }
    let code = cef::get_exit_code();
    if code == cef::sys::cef_resultcode_t::CEF_RESULT_CODE_NORMAL_EXIT_PROCESS_NOTIFIED as i32 {
        return Ok(Started::Elsewhere);
    }
    bail!("CEF failed to initialize (result code {code})")
}

/// The extensions [`init`] was asked to load.
pub(crate) static EXTENSIONS: OnceLock<Vec<PathBuf>> = OnceLock::new();

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

thread_local! {
    /// Arguments of later launches, waiting for the app to take them.
    static RELAUNCHES: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
}

/// Calls `handler` with the arguments (not switches) of every later launch
/// on the same profile, which hands them here and exits.
pub fn on_relaunch(handler: impl Fn(Vec<String>, &mut gpui::App) + 'static) {
    pump::add_drain(move |cx| {
        for args in RELAUNCHES.take() {
            handler(args, cx);
        }
        true
    });
}

pub(crate) fn relaunched(args: Vec<String>) {
    RELAUNCHES.with_borrow_mut(|r| r.push(args));
    pump::wake();
}

/// Shuts CEF down. Every [`WebView`] must have been closed.
pub fn shutdown() {
    pump::stop();
    cef::shutdown();
}

fn path_string(path: &Path) -> CefString {
    CefString::from(path.to_string_lossy().as_ref())
}

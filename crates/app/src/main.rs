// Release builds on Windows open no console window beside Gate's.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Gate: a Chromium browser with a sidebar of tabs, herdr-style
//! splits and zeron's look.

mod activity;
mod assets;
mod backdrop;
mod chrome;
mod db;
mod look;
mod palette;
mod render;
mod session;
mod settings;
mod settings_view;
mod text_input;
mod theme;
mod ublock;
mod url;
mod workspace;

use gpui::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set by [`restart`]: once CEF has shut down, start again.
static RESTART: AtomicBool = AtomicBool::new(false);

/// Quits and starts again (the session comes back): updates and startup
/// settings apply.
pub fn restart(cx: &mut App) {
    RESTART.store(true, Ordering::SeqCst);
    cx.quit();
}

fn main() {
    // CEF starts its renderer/GPU helpers from this same binary.
    if let Some(code) = web::run_helper_process() {
        std::process::exit(code);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("GATE_LOG").unwrap_or_else(|_| "warn,gate=info,web=info".into()),
        )
        .init();
    // Anything not a flag is a page to open.
    let urls: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).map(|a| url::resolve(&a)).collect();

    let profile = profile_dir();
    let settings = settings::Settings::load(&profile.join("settings.json"));
    // Before Chromium starts: a downloaded update goes in now.
    let paths = ublock::Paths::new(&profile);
    let installed = ublock::prepare(&paths);
    if installed.is_none() {
        tracing::warn!("uBlock Origin Lite isn't installed; ad blocking is off");
    }
    let ublock = installed.clone().filter(|_| settings.ad_blocking);
    let store = settings::Store {
        path: profile.join("settings.json"),
        ublock: ublock.as_deref().map(web::extension_id),
        ublock_missing: installed.is_none(),
        ublock_version: installed.as_deref().and_then(ublock::version),
        ublock_paths: paths,
        ublock_update: Default::default(),
        settings,
    };

    gpui_platform::application().with_assets(assets::Assets).run(move |cx: &mut App| {
        match web::init(&profile, ublock.as_slice(), cx) {
            Ok(web::Started::Here) => {}
            // The running instance opens our pages.
            Ok(web::Started::Elsewhere) => return cx.quit(),
            Err(error) => {
                eprintln!("gate: {error:#}");
                // A non-zero status; the quit hook does not run without CEF.
                std::process::exit(1);
            }
        }
        assets::load_fonts(cx);
        text_input::bind_keys(cx);
        workspace::bind_keys(cx);

        let db = match db::Db::open(&profile.join("gate.db")) {
            Ok(db) => db,
            Err(error) => {
                eprintln!("gate: opening the database: {error}");
                std::process::exit(1);
            }
        };
        let session = session::Session::load(&db);
        let bounds = session.window_bounds().unwrap_or_else(|| WindowBounds::Windowed(Bounds::centered(None, size(px(1360.), px(880.)), cx)));
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                window_min_size: Some(size(px(640.), px(420.))),
                titlebar: Some(TitlebarOptions { title: Some("Gate".into()), appears_transparent: true, ..Default::default() }),
                app_owns_titlebar_drag: true,
                // We draw all the chrome; the compositor draws none.
                window_decorations: Some(WindowDecorations::Client),
                // Transparent corners around the rounded window.
                window_background: WindowBackgroundAppearance::Transparent,
                app_id: Some("gate".into()),
                ..Default::default()
            },
            move |window, cx| {
                // Transparent only at the rounded corners; every pixel text
                // lands on is opaque, so it keeps subpixel smoothing.
                window.set_opaque_content(true);
                cx.new(|cx| workspace::Workspace::new(urls, store, db, session, window, cx))
            },
        );
        let window = match window {
            Ok(window) => window,
            Err(error) => {
                eprintln!("gate: opening the window: {error:#}");
                return cx.quit();
            }
        };
        // Later launches hand their pages to this window.
        web::on_relaunch(move |args, cx| {
            let _ = window.update(cx, |workspace, window, cx| {
                workspace.open_urls(args.iter().map(|a| url::resolve(a)).collect(), window, cx);
                window.activate_window();
            });
        });
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.on_app_quit(move |cx| {
            // The last word on what was open, before pages close.
            let _ = window.update(cx, |workspace, window, cx| workspace.save_session(window, cx));
            async {
                web::shutdown();
                // After CEF let go of the profile, so the new one owns it.
                if RESTART.load(Ordering::SeqCst) {
                    match std::env::current_exe() {
                        Ok(exe) => {
                            if let Err(error) = std::process::Command::new(exe).spawn() {
                                eprintln!("gate: restarting: {error}");
                            }
                        }
                        Err(error) => eprintln!("gate: restarting: {error}"),
                    }
                }
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// Cookies, cache and storage: `%LOCALAPPDATA%\Gate` on Windows,
/// `$XDG_DATA_HOME/gate` elsewhere.
fn profile_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home_dir().join("AppData/Local"));
        base.join("Gate")
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home_dir().join(".local/share"));
        let profile = base.join("gate");
        adopt_old_profile(&base.join("browser-opus"), &profile);
        profile
    }
}

/// The user's home folder.
pub fn home_dir() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    PathBuf::from(std::env::var_os(var).unwrap_or_default())
}

#[cfg(not(windows))]
/// The browser was called browser-opus before it was Gate: a profile kept
/// under the old name moves to the new one (spaces, history, settings, site
/// data), once, if there is none under the new name yet.
fn adopt_old_profile(old: &std::path::Path, profile: &std::path::Path) {
    if profile.exists() || !old.is_dir() {
        return;
    }
    // Chromium's lock names the process using the profile ("host-pid"):
    // a browser-opus still running keeps its profile until it quits.
    #[cfg(target_os = "linux")]
    if let Ok(lock) = std::fs::read_link(old.join("SingletonLock"))
        && let Some(pid) = lock.to_string_lossy().rsplit('-').next().map(str::to_owned)
        && std::path::Path::new("/proc").join(&pid).exists()
    {
        eprintln!("gate: browser-opus is still running; its profile moves to Gate after it quits");
        return;
    }
    if let Err(error) = std::fs::rename(old, profile) {
        eprintln!("gate: moving {} to {}: {error}", old.display(), profile.display());
        return;
    }
    let _ = std::fs::rename(profile.join("browser-opus.db"), profile.join("gate.db"));
    // SQLite's write-ahead files go with their database.
    for ext in ["-wal", "-shm"] {
        let _ = std::fs::rename(profile.join(format!("browser-opus.db{ext}")), profile.join(format!("gate.db{ext}")));
    }
}

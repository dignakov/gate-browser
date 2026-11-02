//! Chromium (CEF), rendering windowless: every page renders off screen and
//! hands over its frames, which go straight into a GPUI atlas tile (only
//! the damaged parts) and are painted like any other image.
//!
//! CEF shares GPUI's main thread: `external_message_pump` lets CEF ask for
//! work (`on_schedule_message_pump_work`) and [`pump`] runs it from a GPUI
//! task.

mod app;
mod client;
mod frames;
#[cfg(target_os = "linux")]
mod glib;
#[cfg(target_os = "linux")]
mod gpu_frames;
mod keys;
mod pump;
mod renderer;

use crate::engine::{Button, Events, Input, Page};
use crate::{Started, Zoom};
use anyhow::{Context as _, Result, bail};
use cef::{ImplBrowser as _, ImplBrowserHost as _, ImplFrame as _, *};
use client::{Reply, Shared};
use gpui::{Bounds, Corners, Pixels, Point, RenderImage, ScrollDelta, Window, point, px, size};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// The CEF distribution the build used (development runs).
const BUILT_CEF_DIR: &str = env!("OPUS_CEF_DIR");

/// Where CEF's runtime files (paks, locales, ICU data) are: beside the
/// executable in a packaged Gate, else in the distribution it was built with.
fn cef_dir(exe: &Path) -> PathBuf {
    match exe.parent() {
        Some(dir) if dir.join("icudtl.dat").is_file() => dir.to_path_buf(),
        _ => PathBuf::from(BUILT_CEF_DIR),
    }
}

/// Runs this process as one of CEF's helpers (renderer, GPU, utility) when
/// CEF launched it as one: `Some(code)` means exit with `code`.
pub(crate) fn run_helper_process() -> Option<i32> {
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let mut app = app::build();
    // The browser process gets -1: it carries on and calls `init`.
    let code = cef::execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    (code >= 0).then_some(code)
}

/// Starts CEF in the browser process and begins pumping its work from GPUI.
pub(crate) fn init(profile: &Path, extensions: &[PathBuf], gpu_frames: bool, cx: &mut gpui::App) -> Result<Started> {
    let args = cef::args::Args::new();
    let _ = GPU_FRAMES.set(gpu_frames);
    // Read by the app object while CEF parses the command line, below.
    let _ = EXTENSIONS.set(extensions.to_vec());
    let mut app = app::build();
    std::fs::create_dir_all(profile).with_context(|| format!("creating {}", profile.display()))?;
    let exe = std::env::current_exe().context("finding this executable")?;
    let dir = cef_dir(&exe);
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

/// Whether [`init`] was asked for GPU frames.
static GPU_FRAMES: OnceLock<bool> = OnceLock::new();

#[cfg(target_os = "linux")]
fn gpu_frames_wanted() -> bool {
    GPU_FRAMES.get().copied().unwrap_or(false)
}

/// The extensions [`init`] was asked to load.
static EXTENSIONS: OnceLock<Vec<PathBuf>> = OnceLock::new();

thread_local! {
    /// Arguments of later launches, waiting for the app to take them.
    static RELAUNCHES: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
}

/// See [`crate::on_relaunch`].
pub(crate) fn on_relaunch(handler: impl Fn(Vec<String>, &mut gpui::App) + 'static) {
    pump::add_drain(move |cx| {
        for args in RELAUNCHES.take() {
            handler(args, cx);
        }
        true
    });
}

fn relaunched(args: Vec<String>) {
    RELAUNCHES.with_borrow_mut(|r| r.push(args));
    pump::wake();
}

/// Pages prefer dark, or light: Chromium's color scheme for the profile.
pub(crate) fn set_dark_mode(dark: bool) {
    let variant = if dark { sys::cef_color_variant_t::CEF_COLOR_VARIANT_DARK } else { sys::cef_color_variant_t::CEF_COLOR_VARIANT_LIGHT };
    if let Some(context) = request_context_get_global_context() {
        context.set_chrome_color_scheme(variant.into(), 0);
    }
}

/// Shuts CEF down. Every page must have been closed.
pub(crate) fn shutdown() {
    pump::stop();
    cef::shutdown();
}

fn path_string(path: &Path) -> CefString {
    CefString::from(path.to_string_lossy().as_ref())
}

/// A page in a windowless CEF browser.
pub(crate) struct CefPage {
    shared: Rc<RefCell<Shared>>,
    /// Buttons held over the page, as CEF event flags.
    buttons: u32,
    closed: bool,
}

impl CefPage {
    pub(crate) fn new(url: &str, events: Events, window: &mut Window) -> Self {
        let mut shared = Shared::new(url, events);
        shared.scale = window.scale_factor();
        // Frames go straight into the window's atlas as they arrive.
        shared.frames = Some(frames::Frames { atlas: window.atlas_handle() });
        // As GPU buffers, when asked for and the window's GPU can import them.
        #[cfg(target_os = "linux")]
        if gpu_frames_wanted() && !gpu_frames::broken() && window.dmabuf_target().is_some() {
            shared.gpu = Some(Default::default());
        }
        let page = Self { shared: Rc::new(RefCell::new(shared)), buttons: 0, closed: false };
        page.create_browser();
        page
    }

    fn create_browser(&self) {
        let url = self.shared.borrow().url.clone();
        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
            shared_texture_enabled: self.takes_gpu_frames() as i32,
            runtime_style: RuntimeStyle::ALLOY,
            ..Default::default()
        };
        let browser_settings = BrowserSettings {
            windowless_frame_rate: 60,
            // Opaque white until the page paints, like other browsers.
            background_color: 0xFFFF_FFFF,
            ..Default::default()
        };
        let mut client = client::build(&self.shared);
        let created = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&CefString::from(url.as_str())),
            Some(&browser_settings),
            None,
            None,
        );
        if created != 1 {
            tracing::error!("CEF could not create a browser for {url}");
        }
    }

    /// Whether this page's browser hands frames over as GPU buffers.
    fn takes_gpu_frames(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.shared.borrow().gpu.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// GPU frames failed: a browser takes one kind of frame for its life, so
    /// a new one (loading the same page) takes CPU frames.
    fn fall_back_to_cpu_frames(&mut self) {
        {
            let mut shared = self.shared.borrow_mut();
            let shared = &mut *shared;
            shared.gpu_failed = false;
            #[cfg(target_os = "linux")]
            if shared.gpu.take().is_none() {
                return;
            }
            if let Some(frames) = &shared.frames {
                frames.release(&mut shared.page);
                frames.release(&mut shared.popup);
            }
        }
        if self.closed {
            return;
        }
        self.with_host(|h| h.close_browser(1));
        self.create_browser();
    }

    /// Runs `f` with the browser, without holding a borrow of the shared
    /// state (CEF may call back into it).
    fn with_browser(&self, f: impl FnOnce(&Browser)) {
        let browser = self.shared.borrow().browser.clone();
        if let Some(browser) = browser {
            f(&browser);
        }
    }

    fn with_host(&self, f: impl FnOnce(&BrowserHost)) {
        self.with_browser(|b| {
            if let Some(host) = b.host() {
                f(&host);
            }
        });
    }

    fn mouse_event(&self, at: Point<f32>, modifiers: &gpui::Modifiers) -> MouseEvent {
        MouseEvent { x: at.x.round() as i32, y: at.y.round() as i32, modifiers: keys::modifiers(modifiers) | self.buttons }
    }
}

fn cef_button(button: Button) -> (MouseButtonType, u32) {
    match button {
        Button::Left => (MouseButtonType::LEFT, keys::LEFT_BUTTON),
        Button::Middle => (MouseButtonType::MIDDLE, keys::MIDDLE_BUTTON),
        Button::Right => (MouseButtonType::RIGHT, keys::RIGHT_BUTTON),
    }
}

impl Page for CefPage {
    fn load(&mut self, url: &str) {
        self.with_browser(|b| {
            if let Some(frame) = b.main_frame() {
                frame.load_url(Some(&CefString::from(url)));
            }
        });
    }
    fn back(&mut self) {
        self.with_browser(|b| b.go_back());
    }
    fn forward(&mut self) {
        self.with_browser(|b| b.go_forward());
    }
    fn reload(&mut self) {
        self.with_browser(|b| b.reload());
    }
    fn stop(&mut self) {
        self.with_browser(|b| b.stop_load());
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.shared.borrow_mut().hidden = hidden;
        self.with_host(|h| h.was_hidden(hidden as i32));
    }

    fn set_focus(&mut self, focused: bool) {
        self.shared.borrow_mut().focused = focused;
        self.with_host(|h| h.set_focus(focused as i32));
    }

    /// Tells CEF the page's new size or scale, when they changed.
    fn layout(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        let want = (f32::from(bounds.size.width).round() as i32, f32::from(bounds.size.height).round() as i32);
        let (size_changed, scale_changed) = {
            let mut shared = self.shared.borrow_mut();
            let changed = (shared.view_size != want, shared.scale != scale);
            shared.view_size = want;
            shared.scale = scale;
            changed
        };
        if scale_changed {
            self.with_host(|h| h.notify_screen_info_changed());
        }
        if size_changed || scale_changed {
            self.with_host(|h| h.was_resized());
        }
    }

    fn input(&mut self, input: Input<'_>) {
        match input {
            Input::MouseDown { at, button, clicks, modifiers } => {
                let (button, flag) = cef_button(button);
                self.buttons |= flag;
                let event = self.mouse_event(at, &modifiers);
                self.with_host(|h| h.send_mouse_click_event(Some(&event), button, 0, clicks.clamp(1, 3) as i32));
            }
            Input::MouseUp { at, button, clicks, modifiers } => {
                let (button, flag) = cef_button(button);
                let event = self.mouse_event(at, &modifiers);
                self.buttons &= !flag;
                self.with_host(|h| h.send_mouse_click_event(Some(&event), button, 1, clicks.clamp(1, 3) as i32));
            }
            Input::MouseMove { at, modifiers } => {
                let event = self.mouse_event(at, &modifiers);
                self.with_host(|h| h.send_mouse_move_event(Some(&event), 0));
            }
            Input::MouseLeave => {
                let event = MouseEvent { x: -1, y: -1, modifiers: 0 };
                self.with_host(|h| h.send_mouse_move_event(Some(&event), 1));
            }
            Input::Scroll { at, delta, modifiers } => {
                // Chromium scrolls 40px per wheel line on Linux.
                let delta = match delta {
                    ScrollDelta::Pixels(p) => point(f32::from(p.x), f32::from(p.y)),
                    ScrollDelta::Lines(l) => point(l.x * 40.0, l.y * 40.0),
                };
                let event = self.mouse_event(at, &modifiers);
                self.with_host(|h| h.send_mouse_wheel_event(Some(&event), delta.x.round() as i32, delta.y.round() as i32));
            }
            Input::KeyDown { keystroke, repeat } => {
                let mut flags = keys::modifiers(&keystroke.modifiers);
                if repeat {
                    flags |= keys::IS_REPEAT;
                }
                let mut event = KeyEvent {
                    type_: KeyEventType::RAWKEYDOWN,
                    modifiers: flags,
                    windows_key_code: keys::windows_key_code(keystroke),
                    ..Default::default()
                };
                self.with_host(|h| h.send_key_event(Some(&event)));
                if let Some(ch) = keys::typed_char(keystroke) {
                    event.type_ = KeyEventType::CHAR;
                    event.windows_key_code = ch as i32;
                    event.character = ch;
                    event.unmodified_character = ch;
                    self.with_host(|h| h.send_key_event(Some(&event)));
                }
            }
            Input::KeyUp { keystroke } => {
                let event = KeyEvent {
                    type_: KeyEventType::KEYUP,
                    modifiers: keys::modifiers(&keystroke.modifiers),
                    windows_key_code: keys::windows_key_code(keystroke),
                    ..Default::default()
                };
                self.with_host(|h| h.send_key_event(Some(&event)));
            }
        }
    }

    /// Zooms as Chromium does: its steps (25% to 500%), remembered per site.
    fn zoom(&mut self, zoom: Zoom) {
        let command = match zoom {
            Zoom::In => ZoomCommand::IN,
            Zoom::Out => ZoomCommand::OUT,
            Zoom::Reset => ZoomCommand::RESET,
        };
        self.with_host(|h| h.zoom(command));
    }

    fn zoom_percent(&self) -> u32 {
        let mut level = 0.0;
        self.with_host(|h| level = h.zoom_level());
        // Chromium's zoom levels are powers of 1.2.
        (1.2f64.powf(level) * 100.0).round() as u32
    }

    fn set_muted(&mut self, muted: bool) {
        self.with_host(|h| h.set_audio_muted(muted as i32));
    }

    fn play_pause(&mut self) {
        let (command, frames) = {
            let shared = self.shared.borrow();
            let (command, wanted) = match shared.playback() {
                crate::Playback::Playing => ("pause", crate::Playback::Playing),
                crate::Playback::Paused => ("play", crate::Playback::Paused),
                crate::Playback::None => return,
            };
            let frames: Vec<String> = shared.media.iter().filter(|(_, s)| **s == wanted).map(|(f, _)| f.clone()).collect();
            (command, frames)
        };
        self.with_browser(|browser| {
            for id in &frames {
                let Some(frame) = browser.frame_by_identifier(Some(&CefString::from(id.as_str()))) else { continue };
                let Some(mut message) = process_message_create(Some(&CefString::from(renderer::MEDIA_CONTROL))) else { continue };
                if let Some(args) = message.argument_list() {
                    args.set_string(0, Some(&CefString::from(command)));
                }
                frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
            }
        });
    }

    fn answer(&mut self, accept: bool, input: Option<&str>) {
        let pending = self.shared.borrow_mut().ask.take();
        let Some((_, reply)) = pending else { return };
        match reply {
            Reply::Dialog(callback) => callback.cont(accept as i32, input.map(CefString::from).as_ref()),
            Reply::Media(callback, requested) => {
                if accept {
                    callback.cont(requested)
                } else {
                    callback.cancel()
                }
            }
            Reply::Prompt(callback, _) => callback.cont(if accept {
                PermissionRequestResult::ACCEPT
            } else {
                PermissionRequestResult::DENY
            }),
        }
    }

    fn show_dev_tools(&mut self) {
        // DevTools open in their own native window for now.
        self.with_host(|h| {
            let info = WindowInfo { runtime_style: RuntimeStyle::ALLOY, ..Default::default() };
            h.show_dev_tools(Some(&info), None, None, None);
        });
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            self.with_host(|h| h.close_browser(1));
        }
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window) {
        let scale = window.scale_factor();
        let shared = self.shared.borrow();
        let mut lost = false;
        if let Some(image) = &shared.page.image {
            lost |= !paint_tile(image, bounds.origin, bounds, corners, scale, window);
        }
        let popup_origin = shared.popup_rect.as_ref().map(|r| bounds.origin + point(px(r.x as f32), px(r.y as f32)));
        if let (Some(image), Some(origin)) = (&shared.popup.image, popup_origin) {
            lost |= !paint_tile(image, origin, bounds, Corners::default(), scale, window);
        }
        drop(shared);
        // A tile the atlas dropped needs a whole new frame.
        if lost {
            self.with_host(|h| h.invalidate(PaintElementType::VIEW));
        }
    }

    fn pump(&mut self) {
        if self.shared.borrow().gpu_failed {
            self.fall_back_to_cpu_frames();
        }
    }
}

impl Drop for CefPage {
    fn drop(&mut self) {
        self.close();
    }
}

/// Paints a surface's atlas tile at `origin`; false when the atlas lost the
/// tile (its content is gone until a whole new frame arrives).
fn paint_tile(
    image: &Arc<RenderImage>,
    origin: Point<Pixels>,
    clip: Bounds<Pixels>,
    corners: Corners<Pixels>,
    scale: f32,
    window: &mut Window,
) -> bool {
    match window.insert_blank_image(image, 0) {
        Ok(false) => {}
        Ok(true) => return false,
        Err(error) => {
            tracing::warn!("web frame tile: {error:#}");
            return false;
        }
    }
    let pixels = image.size(0);
    let shown = size(px(pixels.width.0 as f32 / scale), px(pixels.height.0 as f32 / scale));
    window.with_content_mask(Some(gpui::ContentMask { bounds: clip }), |window| {
        let _ = window.paint_image(Bounds::new(origin, shown), corners, image.clone(), 0, false);
    });
    true
}

//! Edge (WebView2) on Windows, in visual-hosting mode: each page is a
//! DirectComposition visual mounted in zui's native layer, between GPUI's
//! base plane and its overlay plane (deferred UI: menus, dialogs, the
//! palette). Edge renders and presents it with its own GPU pipeline; Gate
//! only places and clips it. GPUI keeps the pointer and forwards what reaches
//! the page; the page holds the keyboard while focused and hands back Gate's
//! shortcuts. Callbacks only queue events. Adapted from Zeron (MIT).

mod instance;

use crate::engine::{Button, Events, Input, Page, PageEvent};
use crate::{Ask, Playback, Started, Zoom};
use anyhow::{Context as _, Result};
use gpui::{Bounds, Corners, CursorStyle, Keystroke, Modifiers, Pixels, RenderImage, ScrollDelta, Window};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
use windows::core::{HSTRING, Interface, PWSTR};

// ---------------------------------------------------------------- engine

/// Pages prefer dark (see [`crate::set_dark_mode`]).
static DARK: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
enum Environment {
    #[default]
    Idle,
    Pending(Vec<Box<dyn FnOnce(Result<ICoreWebView2Environment, String>)>>),
    Ready(ICoreWebView2Environment),
    Failed(String),
}

#[derive(Default)]
struct Engine {
    /// The WebView2 user data folder (cookies, cache, storage, extensions).
    folder: Option<PathBuf>,
    /// Extension folders to install (uBlock Origin Lite).
    extensions: Vec<PathBuf>,
    /// Installed into this environment already.
    extensions_added: bool,
    environment: Environment,
    /// Live pages, for settings that apply to all of them.
    pages: Vec<Weak<Host>>,
    relaunch: Option<Rc<dyn Fn(Vec<String>, &mut gpui::App)>>,
    /// Gate's own shortcuts: pressed in a page, they go to Gate.
    shortcuts: Vec<Keystroke>,
    /// Counts presented frames; see [`sweep`].
    frame: u64,
    sweep_queued: bool,
    /// Windows whose system drags go to their pages (see [`native_drag`]).
    drag_windows: Vec<isize>,
}

thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::default());
}

/// Starts the engine: claims the profile (another Gate on it takes this
/// launch's pages instead). Edge itself starts with the first page.
pub(crate) fn init(profile: &Path, extensions: &[PathBuf], cx: &mut gpui::App) -> Result<Started> {
    let folder = profile.join("webview2");
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let Some(relaunches) = instance::claim(profile, &args)? else {
        return Ok(Started::Elsewhere);
    };
    tracing::info!("Edge (WebView2): profile {}", folder.display());
    ENGINE.with_borrow_mut(|e| {
        e.folder = Some(folder);
        e.extensions = extensions.to_vec();
    });
    cx.spawn(async move |cx| {
        while let Ok(args) = relaunches.recv().await {
            let handler = ENGINE.with_borrow(|e| e.relaunch.clone());
            if let Some(handler) = handler {
                cx.update(|cx| handler(args, cx));
            }
        }
    })
    .detach();
    Ok(Started::Here)
}

/// See [`crate::on_relaunch`].
pub(crate) fn on_relaunch(handler: impl Fn(Vec<String>, &mut gpui::App) + 'static) {
    ENGINE.with_borrow_mut(|e| e.relaunch = Some(Rc::new(handler)));
}

/// See [`crate::set_app_shortcuts`].
pub(crate) fn set_app_shortcuts(shortcuts: Vec<Keystroke>) {
    tracing::info!("Edge (WebView2): {} shortcuts go to Gate from pages", shortcuts.len());
    ENGINE.with_borrow_mut(|e| e.shortcuts = shortcuts);
}

/// See [`crate::set_dark_mode`].
pub(crate) fn set_dark_mode(dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    for host in ENGINE.with_borrow(|e| e.pages.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()) {
        host.apply_color_scheme();
    }
}

pub(crate) fn shutdown() {
    for host in ENGINE.with_borrow_mut(|e| std::mem::take(&mut e.pages)) {
        if let Some(host) = host.upgrade() {
            host.close_native();
        }
    }
}

/// Runs `done` with the environment, creating it on first use.
fn with_environment(done: impl FnOnce(Result<ICoreWebView2Environment, String>) + 'static) {
    enum Next {
        Now(Result<ICoreWebView2Environment, String>),
        Wait,
        Create,
    }
    let mut done: Option<Box<dyn FnOnce(Result<ICoreWebView2Environment, String>)>> = Some(Box::new(done));
    let next = ENGINE.with_borrow_mut(|e| match &mut e.environment {
        Environment::Ready(environment) => Next::Now(Ok(environment.clone())),
        Environment::Failed(error) => Next::Now(Err(error.clone())),
        Environment::Pending(waiters) => {
            waiters.extend(done.take());
            Next::Wait
        }
        Environment::Idle => {
            e.environment = Environment::Pending(done.take().into_iter().collect());
            Next::Create
        }
    });
    match next {
        Next::Now(outcome) => {
            if let Some(done) = done {
                done(outcome);
            }
        }
        Next::Wait => {}
        Next::Create => create_environment(),
    }
}

/// The environment, or why there is none, to everyone waiting for it.
fn environment_done(outcome: Result<ICoreWebView2Environment, String>) {
    let next = match &outcome {
        Ok(environment) => Environment::Ready(environment.clone()),
        Err(error) => Environment::Failed(error.clone()),
    };
    let waiters = match ENGINE.with_borrow_mut(|e| std::mem::replace(&mut e.environment, next)) {
        Environment::Pending(waiters) => waiters,
        _ => Vec::new(),
    };
    for waiter in waiters {
        waiter(outcome.clone());
    }
}

fn create_environment() {
    let Some(folder) = ENGINE.with_borrow(|e| e.folder.clone()) else {
        return environment_done(Err("Edge (WebView2) was not started".into()));
    };
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: setting a field of an options object nothing else holds yet.
    unsafe { options.set_are_browser_extensions_enabled(true) };
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(|result, environment| {
        match result.and_then(|()| environment.ok_or_else(windows::core::Error::empty)) {
            Ok(environment) => {
                let mut version = PWSTR::null();
                // SAFETY: reading a string property of a live environment.
                if unsafe { environment.BrowserVersionString(&mut version) }.is_ok() {
                    tracing::info!("Edge (WebView2): runtime {}", take_pwstr(version));
                }
                environment_done(Ok(environment));
            }
            Err(error) => environment_done(Err(runtime_error(&error))),
        }
        Ok(())
    }));
    // SAFETY: the folder string and handler outlive the call.
    let created = unsafe {
        CreateCoreWebView2EnvironmentWithOptions(None, &HSTRING::from(folder.to_string_lossy().as_ref()), &options, &handler)
    };
    if let Err(error) = created {
        environment_done(Err(runtime_error(&error)));
    }
}

fn runtime_error(error: &windows::core::Error) -> String {
    tracing::error!("Edge (WebView2) unavailable: {error}");
    format!("Pages need the Microsoft Edge WebView2 Runtime ({error}).")
}

/// Forgets the environment after its browser process exited: the next page
/// starts a new one.
fn forget_environment(environment: &ICoreWebView2Environment) {
    ENGINE.with_borrow_mut(|e| {
        if matches!(&e.environment, Environment::Ready(ready) if ready == environment) {
            e.environment = Environment::Idle;
            e.extensions_added = false;
        }
    });
}

/// Installs the extensions into the profile `webview` belongs to, once.
fn add_extensions(webview: &ICoreWebView2) {
    let extensions = ENGINE.with_borrow_mut(|e| {
        if e.extensions_added {
            return Vec::new();
        }
        e.extensions_added = true;
        e.extensions.clone()
    });
    if extensions.is_empty() {
        return;
    }
    let profile = webview
        .cast::<ICoreWebView2_13>()
        .and_then(|w| unsafe { w.Profile() })
        .and_then(|p| p.cast::<ICoreWebView2Profile7>());
    let profile = match profile {
        Ok(profile) => profile,
        Err(error) => return tracing::warn!("Edge (WebView2): no extensions in this runtime: {error}"),
    };
    for dir in extensions {
        let shown = dir.display().to_string();
        let handler = ProfileAddBrowserExtensionCompletedHandler::create(Box::new(move |result, extension| {
            match (result, extension) {
                (Ok(()), Some(extension)) => {
                    let mut name = PWSTR::null();
                    // SAFETY: reading a string property of a live extension.
                    let name = unsafe { extension.Name(&mut name) }.map(|()| take_pwstr(name)).unwrap_or_default();
                    tracing::info!("Edge (WebView2): extension {name} from {shown}");
                }
                (result, _) => tracing::warn!("Edge (WebView2): extension {shown} not added: {result:?}"),
            }
            Ok(())
        }));
        // SAFETY: the path string and handler outlive the call.
        if let Err(error) = unsafe { profile.AddBrowserExtension(&HSTRING::from(dir.to_string_lossy().as_ref()), &handler) } {
            tracing::warn!("Edge (WebView2): extension {} not added: {error}", dir.display());
        }
    }
}

// ---------------------------------------------------------------- frames

/// After a presented frame, unmounts pages it didn't paint: a pane not drawn
/// (another's zoomed over it, its space left) mustn't stay on screen. Queued
/// by the first page painted in a frame; runs before the pages' own updates.
fn queue_sweep(window: &mut Window) {
    let queue = ENGINE.with_borrow_mut(|e| !std::mem::replace(&mut e.sweep_queued, true));
    if queue {
        window.on_present(|| {
            let (frame, pages) = ENGINE.with_borrow_mut(|e| {
                e.sweep_queued = false;
                e.pages.retain(|p| p.strong_count() > 0);
                (e.frame, e.pages.iter().filter_map(Weak::upgrade).collect::<Vec<_>>())
            });
            for host in pages {
                if host.painted.get() != frame {
                    host.has_area.set(false);
                    host.update_visibility();
                }
            }
            ENGINE.with_borrow_mut(|e| e.frame += 1);
        });
    }
}

/// A system drag over a window with pages: the data, and the page it's over.
struct Drag {
    data: windows::Win32::System::Com::IDataObject,
    page: Option<Rc<Host>>,
}

thread_local! {
    static DRAG: RefCell<Option<Drag>> = const { RefCell::new(None) };
}

/// The page shown at `position` (physical pixels in `hwnd`'s client area),
/// and the point within it.
fn page_at(hwnd: HWND, position: gpui::Point<gpui::DevicePixels>) -> Option<(Rc<Host>, POINT)> {
    let (x, y) = (position.x.0 as f32, position.y.0 as f32);
    let pages: Vec<Rc<Host>> = ENGINE.with_borrow(|e| e.pages.iter().filter_map(Weak::upgrade).collect());
    pages.into_iter().find_map(|host| {
        let mounted = host.native.borrow().as_ref().is_some_and(|n| n.mounted);
        let g = host.geometry.get();
        let (px, py) = (x - g.origin.0, y - g.origin.1);
        let inside = px >= g.clip[0] && py >= g.clip[1] && px < g.clip[2] && py < g.clip[3];
        (host.hwnd == hwnd && mounted && inside).then(|| (host, POINT { x: px as i32, y: py as i32 }))
    })
}

fn drop_target(host: &Host) -> Option<ICoreWebView2CompositionController3> {
    host.native.borrow().as_ref().and_then(|n| n.composition.cast().ok())
}

/// GPUI's window takes system drags (OLE); Edge in visual hosting gets them
/// only from its host. Drags over a page go to it, including the ones the
/// page itself started (HTML5 drag and drop); the rest stay GPUI's. COM
/// calls run with no state borrowed: OLE may dispatch messages inside them.
fn native_drag(hwnd: HWND, drag: gpui::NativeDrag) -> Option<u32> {
    use gpui::NativeDragKind as Kind;
    match drag.kind {
        Kind::Enter => {
            // SAFETY: GPUI hands the drag's IDataObject, borrowed for the call.
            let data = unsafe { windows::Win32::System::Com::IDataObject::from_raw_borrowed(&drag.data) }?.clone();
            DRAG.set(Some(Drag { data, page: None }));
        }
        Kind::Leave => {
            let page = DRAG.take().and_then(|d| d.page)?;
            if let Some(target) = drop_target(&page) {
                // SAFETY: a COM call on the live page.
                let _ = unsafe { target.DragLeave() };
            }
            return Some(0);
        }
        Kind::Over | Kind::Drop => {}
    }
    let (data, current) = DRAG.with_borrow(|d| d.as_ref().map(|d| (d.data.clone(), d.page.clone())))?;
    if drag.kind == Kind::Drop {
        DRAG.set(None);
    }
    let under = page_at(hwnd, drag.position);
    let mut effect = drag.allowed;
    let mut entered = false;
    if current.as_ref().map(Rc::as_ptr) != under.as_ref().map(|(h, _)| Rc::as_ptr(h)) {
        if let Some(target) = current.as_deref().and_then(drop_target) {
            // SAFETY: a COM call on the live page.
            let _ = unsafe { target.DragLeave() };
        }
        if let Some((page, point)) = &under
            && let Some(target) = drop_target(page)
        {
            // SAFETY: COM calls on the live page; `effect` outlives them.
            let result = unsafe { target.DragEnter(&data, drag.key_state, *point, &mut effect) };
            tracing::debug!("Edge (WebView2): drag entered a page at {point:?}: {result:?}, effect {effect}");
            entered = true;
        }
        DRAG.with_borrow_mut(|d| {
            if let Some(d) = d {
                d.page = under.as_ref().map(|(h, _)| h.clone());
            }
        });
    }
    let (page, point) = under?;
    let target = drop_target(&page)?;
    match drag.kind {
        Kind::Over if !entered => {
            // SAFETY: a COM call on the live page.
            let _ = unsafe { target.DragOver(drag.key_state, point, &mut effect) };
        }
        Kind::Drop => {
            // SAFETY: a COM call on the live page.
            let result = unsafe { target.Drop(&data, drag.key_state, point, &mut effect) };
            tracing::debug!("Edge (WebView2): dropped on a page: {result:?}, effect {effect}");
            // The drop released the button, which the page never heard of
            // while the drag held the pointer.
            page.pressed.set(0);
        }
        _ => {}
    }
    Some(effect)
}

/// Which window holds the keyboard, for the log: Gate's own, a child of it
/// (Edge's), or another.
fn describe_focus(gate: HWND) -> String {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, IsChild};
    // SAFETY: plain Win32 queries.
    unsafe {
        let focus = GetFocus();
        let mut name = [0u16; 128];
        let len = GetClassNameW(focus, &mut name) as usize;
        let class = String::from_utf16_lossy(&name[..len]);
        let whose = if focus.is_invalid() {
            "no window of this thread"
        } else if focus == gate {
            "Gate's window"
        } else if IsChild(gate, focus).as_bool() {
            "a child of Gate's window"
        } else {
            "another window"
        };
        format!("{whose} ({class})")
    }
}

fn current_frame() -> u64 {
    ENGINE.with_borrow(|e| e.frame)
}

// ---------------------------------------------------------------- page

/// A question the page waits on, answered through its deferral.
enum Pending {
    Dialog(ICoreWebView2ScriptDialogOpeningEventArgs, ICoreWebView2Deferral),
    Permission(ICoreWebView2PermissionRequestedEventArgs, ICoreWebView2Deferral),
}

/// The COM objects of a created page.
struct Native {
    environment: ICoreWebView2Environment,
    composition: ICoreWebView2CompositionController,
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    /// The page's visual, its rounded clip, and the layer and device
    /// generation they belong to.
    visual: Option<Mount>,
    mounted: bool,
    applied: Option<Geometry>,
    shown: Option<bool>,
}

struct Mount {
    visual: IDCompositionVisual,
    clip: IDCompositionRectangleClip,
    layer: IDCompositionVisual,
    generation: u64,
}

/// Where the page is, in physical pixels: its origin in the window, its
/// size, the visible part (in its own coordinates) and its corner radii.
#[derive(Clone, Copy, Default, PartialEq)]
struct Geometry {
    origin: (f32, f32),
    size: (i32, i32),
    clip: [f32; 4],
    radii: [f32; 4],
    scale: f32,
}

/// What the view last heard, to report only changes.
#[derive(Default, PartialEq)]
struct Reported {
    loading: bool,
    can_back: bool,
    can_forward: bool,
}

pub(crate) struct Host {
    events: Events,
    hwnd: HWND,
    native: RefCell<Option<Native>>,
    /// Loaded once the page exists.
    pending_url: RefCell<Option<String>>,
    hidden: Cell<bool>,
    has_area: Cell<bool>,
    focused: Cell<bool>,
    closed: Cell<bool>,
    geometry: Cell<Geometry>,
    /// The frame ([`current_frame`]) this page was last painted in.
    painted: Cell<u64>,
    /// Pointer buttons pressed in the page (bit 0 left, 1 right, 2 middle).
    pressed: Cell<u32>,
    hovered: Cell<bool>,
    /// The last press was a middle click or held Ctrl: a window it opens
    /// goes behind.
    background_click: Cell<bool>,
    loading: Cell<bool>,
    reported: RefCell<Reported>,
    ask: RefCell<Option<Pending>>,
    /// Each document's media (by its watcher's id; see [`MEDIA_SCRIPT`]).
    media: RefCell<HashMap<String, Playback>>,
    /// The page's frames, to hand media commands to their watchers.
    frames: RefCell<Vec<ICoreWebView2Frame2>>,
}

pub(crate) struct WebView2Page {
    host: Rc<Host>,
}

impl WebView2Page {
    pub(crate) fn new(url: &str, events: Events, window: &mut Window) -> Self {
        use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        let hwnd = match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => HWND(handle.hwnd.get() as *mut _),
            other => {
                tracing::error!("Edge (WebView2) needs a Win32 window: {other:?}");
                HWND::default()
            }
        };
        if let Err(error) = window.enable_scene_overlay() {
            tracing::error!("Edge (WebView2): no overlay plane for pages: {error:#}");
        }
        let first = ENGINE.with_borrow_mut(|e| {
            let key = hwnd.0 as isize;
            let first = !e.drag_windows.contains(&key);
            if first {
                e.drag_windows.push(key);
            }
            first
        });
        if first {
            window.on_native_drag(move |drag| native_drag(hwnd, drag));
        }
        let host = Rc::new(Host {
            events,
            hwnd,
            native: RefCell::new(None),
            pending_url: RefCell::new(Some(url.to_string())),
            hidden: Cell::new(false),
            has_area: Cell::new(false),
            focused: Cell::new(false),
            closed: Cell::new(false),
            geometry: Cell::new(Geometry::default()),
            painted: Cell::new(u64::MAX),
            pressed: Cell::new(0),
            hovered: Cell::new(false),
            background_click: Cell::new(false),
            loading: Cell::new(true),
            reported: RefCell::new(Reported::default()),
            ask: RefCell::new(None),
            media: RefCell::new(HashMap::new()),
            frames: RefCell::new(Vec::new()),
        });
        ENGINE.with_borrow_mut(|e| e.pages.push(Rc::downgrade(&host)));
        Host::create(&host);
        Self { host }
    }
}

impl Host {
    fn push(&self, event: PageEvent) {
        let _ = self.events.try_send(event);
    }

    fn create(host: &Rc<Self>) {
        let weak = Rc::downgrade(host);
        with_environment(move |environment| {
            let Some(host) = weak.upgrade() else { return };
            if host.closed.get() {
                return;
            }
            match environment {
                Ok(environment) => {
                    if let Err(error) = Host::create_controller(&host, &environment) {
                        host.fail(&runtime_error(&error));
                    }
                }
                Err(error) => host.fail(&error),
            }
        });
    }

    fn create_controller(host: &Rc<Self>, environment: &ICoreWebView2Environment) -> windows::core::Result<()> {
        let weak = Rc::downgrade(host);
        let attach_to = environment.clone();
        let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(move |result, composition| {
            let Some(host) = weak.upgrade() else { return Ok(()) };
            match result.and_then(|()| composition.ok_or_else(windows::core::Error::empty)) {
                Ok(composition) if host.closed.get() => {
                    if let Ok(controller) = composition.cast::<ICoreWebView2Controller>() {
                        // SAFETY: closing a controller nothing else holds.
                        let _ = unsafe { controller.Close() };
                    }
                }
                Ok(composition) => {
                    if let Err(error) = Host::attach(&host, composition, &attach_to) {
                        host.fail(&runtime_error(&error));
                    }
                }
                Err(error) => host.fail(&runtime_error(&error)),
            }
            Ok(())
        }));
        // SAFETY: the window outlives its pages; the handler outlives the call.
        unsafe { environment.cast::<ICoreWebView2Environment3>()?.CreateCoreWebView2CompositionController(host.hwnd, &handler) }
    }

    fn fail(&self, message: &str) {
        tracing::error!("Edge (WebView2) page failed: {message}");
        self.loading.set(false);
        self.report_loading();
        self.push(PageEvent::Crashed);
    }

    fn attach(host: &Rc<Self>, composition: ICoreWebView2CompositionController, environment: &ICoreWebView2Environment) -> windows::core::Result<()> {
        let controller: ICoreWebView2Controller = composition.cast()?;
        // SAFETY: COM calls on live objects, on the thread that created them.
        let webview = unsafe { controller.CoreWebView2()? };
        unsafe {
            let settings = webview.Settings()?;
            settings.SetAreDevToolsEnabled(true)?;
            // Gate asks the page's questions in its own cards.
            settings.SetAreDefaultScriptDialogsEnabled(false)?;
            settings.SetIsStatusBarEnabled(true)?;
            settings.SetIsZoomControlEnabled(true)?;
            if let Ok(controller) = controller.cast::<ICoreWebView2Controller3>() {
                // GPUI owns DPI: the scale follows the window's.
                controller.SetShouldDetectMonitorScaleChanges(false)?;
            }
            controller.SetIsVisible(false)?;
            Host::listen(host, &webview, &composition, &controller)?;
        }
        add_extensions(&webview);
        *host.native.borrow_mut() = Some(Native {
            environment: environment.clone(),
            composition,
            controller,
            webview,
            visual: None,
            mounted: false,
            applied: None,
            shown: None,
        });
        host.apply_color_scheme();
        host.apply_geometry();
        host.update_visibility();
        if host.focused.get() {
            host.move_focus();
        }
        if let Some(url) = host.pending_url.borrow_mut().take() {
            host.navigate(&url);
        }
        tracing::info!("Edge (WebView2): page ready");
        Ok(())
    }

    /// Subscribes to the page's events.
    unsafe fn listen(
        host: &Rc<Self>,
        webview: &ICoreWebView2,
        composition: &ICoreWebView2CompositionController,
        controller: &ICoreWebView2Controller,
    ) -> windows::core::Result<()> {
        let mut token = 0i64;
        unsafe {
            let weak = Rc::downgrade(host);
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, _| {
                    if let Some(host) = weak.upgrade() {
                        host.loading.set(true);
                        host.report_loading();
                        host.push(PageEvent::Progress(0.1));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_ContentLoading(
                &ContentLoadingEventHandler::create(Box::new(move |_, _| {
                    if let Some(host) = weak.upgrade() {
                        host.push(PageEvent::Progress(0.5));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    if let Some(host) = weak.upgrade() {
                        if let Some(args) = args {
                            let mut ok = windows::core::BOOL::default();
                            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                            if args.IsSuccess(&mut ok).is_ok() && !ok.as_bool() && args.WebErrorStatus(&mut status).is_ok() {
                                tracing::info!("Edge (WebView2): navigation failed ({})", status.0);
                            }
                        }
                        host.loading.set(false);
                        host.report_loading();
                        host.push(PageEvent::Progress(1.0));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(host), Some(sender)) = (weak.upgrade(), sender) {
                        let mut source = PWSTR::null();
                        if sender.Source(&mut source).is_ok() {
                            host.push(PageEvent::Url(take_pwstr(source)));
                        }
                        host.report_loading();
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_HistoryChanged(
                &HistoryChangedEventHandler::create(Box::new(move |_, _| {
                    if let Some(host) = weak.upgrade() {
                        host.report_loading();
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_DocumentTitleChanged(
                &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(host), Some(sender)) = (weak.upgrade(), sender) {
                        let mut title = PWSTR::null();
                        if sender.DocumentTitle(&mut title).is_ok() {
                            host.push(PageEvent::Title(take_pwstr(title)));
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let (Some(host), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    // Gate opens it as a space; Edge opens no window of its own.
                    args.SetHandled(true)?;
                    let mut clicked = windows::core::BOOL::default();
                    args.IsUserInitiated(&mut clicked)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let url = take_pwstr(uri);
                    // WebView2 has no popup blocker: only what a click opened.
                    if clicked.as_bool() && !url.is_empty() {
                        host.push(PageEvent::OpenTab { url, background: host.background_click.get() });
                    } else {
                        tracing::info!("Edge (WebView2): blocked a popup ({url})");
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let (Some(host), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    args.ProcessFailedKind(&mut kind)?;
                    tracing::warn!("Edge (WebView2): process failed (kind {})", kind.0);
                    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                        || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                    {
                        host.forget_media();
                    }
                    match kind {
                        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => {
                            if let Some(native) = host.close_native() {
                                forget_environment(&native.environment);
                            }
                            host.push(PageEvent::Crashed);
                        }
                        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                        | COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => host.push(PageEvent::Crashed),
                        // Edge restarts GPU and helper processes itself.
                        _ => {}
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_ContainsFullScreenElementChanged(
                &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(host), Some(sender)) = (weak.upgrade(), sender) {
                        let mut full = windows::core::BOOL::default();
                        sender.ContainsFullScreenElement(&mut full)?;
                        host.push(PageEvent::Fullscreen(full.as_bool()));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_ScriptDialogOpening(
                &ScriptDialogOpeningEventHandler::create(Box::new(move |_, args| {
                    let (Some(host), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND::default();
                    args.Kind(&mut kind)?;
                    let mut message = PWSTR::null();
                    args.Message(&mut message)?;
                    let message = take_pwstr(message);
                    let ask = match kind {
                        COREWEBVIEW2_SCRIPT_DIALOG_KIND_ALERT => Ask::Alert { message },
                        COREWEBVIEW2_SCRIPT_DIALOG_KIND_CONFIRM => Ask::Confirm { message },
                        COREWEBVIEW2_SCRIPT_DIALOG_KIND_PROMPT => {
                            let mut default = PWSTR::null();
                            args.DefaultText(&mut default)?;
                            Ask::Prompt { message, default: take_pwstr(default) }
                        }
                        _ => Ask::Leave { reload: false },
                    };
                    let deferral = args.GetDeferral()?;
                    host.replace_ask(Some(Pending::Dialog(args, deferral)));
                    host.push(PageEvent::Ask(Some(ask)));
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                    let (Some(host), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                    args.PermissionKind(&mut kind)?;
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    let origin = origin_of(&take_pwstr(uri));
                    let deferral = args.GetDeferral()?;
                    host.replace_ask(Some(Pending::Permission(args, deferral)));
                    host.push(PageEvent::Ask(Some(Ask::Permission { origin, what: vec![permission_name(kind)] })));
                    Ok(())
                })),
                &mut token,
            )?;
            Host::watch_media(host, webview)?;
            if let Ok(webview15) = webview.cast::<ICoreWebView2_15>() {
                let weak = Rc::downgrade(host);
                webview15.add_FaviconChanged(
                    &FaviconChangedEventHandler::create(Box::new(move |sender, _| {
                        if let (Some(host), Some(sender)) = (weak.upgrade(), sender) {
                            host.fetch_favicon(&sender);
                        }
                        Ok(())
                    })),
                    &mut token,
                )?;
            }
            let weak = Rc::downgrade(host);
            composition.add_CursorChanged(
                &CursorChangedEventHandler::create(Box::new(move |sender, _| {
                    if let (Some(host), Some(sender)) = (weak.upgrade(), sender) {
                        let mut id = 0u32;
                        sender.SystemCursorId(&mut id)?;
                        // The cursor itself: Chromium draws some (row and
                        // column resize, grab, zoom) with its own images,
                        // which have no system id.
                        let mut cursor = windows::Win32::UI::WindowsAndMessaging::HCURSOR::default();
                        let _ = sender.Cursor(&mut cursor);
                        host.push(if cursor.is_invalid() {
                            PageEvent::Cursor(cursor_style(id))
                        } else {
                            PageEvent::NativeCursor { cursor: gpui::NativeCursor(cursor.0 as usize), fallback: cursor_style(id) }
                        });
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let gpui_window = host.hwnd;
            controller.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    tracing::debug!("Edge (WebView2): page got focus; keyboard in {}", describe_focus(gpui_window));
                    Ok(())
                })),
                &mut token,
            )?;
            controller.add_LostFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    tracing::debug!("Edge (WebView2): page lost focus; keyboard in {}", describe_focus(gpui_window));
                    Ok(())
                })),
                &mut token,
            )?;
            let weak = Rc::downgrade(host);
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let (Some(host), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                        return Ok(());
                    }
                    let mut key = 0u32;
                    args.VirtualKey(&mut key)?;
                    let keystroke = accelerator(key);
                    let shortcut = keystroke.as_ref().is_some_and(|keystroke| {
                        ENGINE.with_borrow(|e| e.shortcuts.iter().any(|k| k.modifiers == keystroke.modifiers && k.key == keystroke.key))
                    });
                    tracing::debug!("Edge (WebView2): key 0x{key:02x} in a page: {:?}, Gate's: {shortcut}", keystroke.as_ref().map(|k| k.unparse()));
                    if let Some(keystroke) = keystroke.filter(|_| shortcut) {
                        args.SetHandled(true)?;
                        host.push(PageEvent::Key(keystroke));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    fn replace_ask(&self, ask: Option<Pending>) {
        // A question still open is dismissed (Cancel/Deny) for the new one.
        if let Some(old) = std::mem::replace(&mut *self.ask.borrow_mut(), ask) {
            answer_pending(old, false, None);
        }
    }

    fn report_loading(&self) {
        let (can_back, can_forward) = match &*self.native.borrow() {
            Some(native) => unsafe {
                let mut back = windows::core::BOOL::default();
                let mut forward = windows::core::BOOL::default();
                let _ = native.webview.CanGoBack(&mut back);
                let _ = native.webview.CanGoForward(&mut forward);
                (back.as_bool(), forward.as_bool())
            },
            None => (false, false),
        };
        let now = Reported { loading: self.loading.get(), can_back, can_forward };
        let mut reported = self.reported.borrow_mut();
        if *reported != now {
            self.push(PageEvent::Loading { loading: now.loading, can_go_back: now.can_back, can_go_forward: now.can_forward });
            *reported = now;
        }
    }

    /// The page's media, over all its documents: playing while any plays.
    fn playback(&self) -> Playback {
        let media = self.media.borrow();
        if media.values().any(|p| *p == Playback::Playing) {
            Playback::Playing
        } else if media.values().any(|p| *p == Playback::Paused) {
            Playback::Paused
        } else {
            Playback::None
        }
    }

    /// A document's watcher reported `message` ({gateMedia, id}).
    fn media_message(&self, message: &str) {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(message) else { return };
        let (Some(state), Some(id)) = (message["gateMedia"].as_str(), message["id"].as_str()) else { return };
        let before = self.playback();
        {
            let mut media = self.media.borrow_mut();
            match state {
                "playing" => media.insert(id.to_string(), Playback::Playing),
                "paused" => media.insert(id.to_string(), Playback::Paused),
                _ => media.remove(id),
            };
        }
        let now = self.playback();
        if now != before {
            self.push(PageEvent::Playback(now));
        }
    }

    fn forget_media(&self) {
        let had = self.playback() != Playback::None;
        self.media.borrow_mut().clear();
        self.frames.borrow_mut().clear();
        if had {
            self.push(PageEvent::Playback(Playback::None));
        }
    }

    /// Runs the media watcher in every document, as the other engines do:
    /// WebView2 only says whether a page makes sound, not that its media
    /// waits paused. Watchers in frames talk through their frame.
    unsafe fn watch_media(host: &Rc<Self>, webview: &ICoreWebView2) -> windows::core::Result<()> {
        let mut token = 0i64;
        unsafe {
            webview.AddScriptToExecuteOnDocumentCreated(
                &HSTRING::from(media_script()),
                None::<&ICoreWebView2AddScriptToExecuteOnDocumentCreatedCompletedHandler>,
            )?;
            let weak = Rc::downgrade(host);
            webview.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                    if let (Some(host), Some(args)) = (weak.upgrade(), args) {
                        let mut json = PWSTR::null();
                        args.WebMessageAsJson(&mut json)?;
                        host.media_message(&take_pwstr(json));
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            let Ok(webview4) = webview.cast::<ICoreWebView2_4>() else { return Ok(()) };
            let weak = Rc::downgrade(host);
            webview4.add_FrameCreated(
                &FrameCreatedEventHandler::create(Box::new(move |_, args| {
                    let (Some(host), Some(args)) = (weak.upgrade(), args) else { return Ok(()) };
                    let Ok(frame) = args.Frame()?.cast::<ICoreWebView2Frame2>() else { return Ok(()) };
                    let mut token = 0i64;
                    let messages = Rc::downgrade(&host);
                    frame.add_WebMessageReceived(
                        &FrameWebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                            if let (Some(host), Some(args)) = (messages.upgrade(), args) {
                                let mut json = PWSTR::null();
                                args.WebMessageAsJson(&mut json)?;
                                host.media_message(&take_pwstr(json));
                            }
                            Ok(())
                        })),
                        &mut token,
                    )?;
                    let gone = Rc::downgrade(&host);
                    frame.add_Destroyed(
                        &FrameDestroyedEventHandler::create(Box::new(move |sender, _| {
                            if let (Some(host), Some(sender)) = (gone.upgrade(), sender) {
                                host.frames.borrow_mut().retain(|f| f.cast::<ICoreWebView2Frame>().ok().as_ref() != Some(&sender));
                            }
                            Ok(())
                        })),
                        &mut token,
                    )?;
                    host.frames.borrow_mut().push(frame);
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    fn fetch_favicon(self: &Rc<Self>, webview: &ICoreWebView2) {
        let Ok(webview) = webview.cast::<ICoreWebView2_15>() else { return };
        let weak = Rc::downgrade(self);
        let handler = GetFaviconCompletedHandler::create(Box::new(move |result, stream| {
            let Some(host) = weak.upgrade() else { return Ok(()) };
            let picture = result.ok().and(stream).and_then(|stream| favicon(&read_stream(&stream)));
            host.push(PageEvent::Favicon(picture));
            Ok(())
        }));
        // SAFETY: the handler outlives the call.
        let _ = unsafe { webview.GetFavicon(COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG, &handler) };
    }

    fn apply_color_scheme(&self) {
        let Some(webview) = self.native.borrow().as_ref().map(|n| n.webview.clone()) else { return };
        let scheme = if DARK.load(Ordering::Relaxed) {
            COREWEBVIEW2_PREFERRED_COLOR_SCHEME_DARK
        } else {
            COREWEBVIEW2_PREFERRED_COLOR_SCHEME_LIGHT
        };
        let profile = webview.cast::<ICoreWebView2_13>().and_then(|w| unsafe { w.Profile() });
        if let Ok(profile) = profile {
            // SAFETY: a property set on a live profile.
            let _ = unsafe { profile.SetPreferredColorScheme(scheme) };
        }
    }

    fn navigate(&self, url: &str) {
        match &*self.native.borrow() {
            // SAFETY: a COM call on a live page.
            Some(native) => {
                if let Err(error) = unsafe { native.webview.Navigate(&HSTRING::from(url)) } {
                    tracing::warn!("Edge (WebView2): can't open {url}: {error}");
                }
            }
            None => *self.pending_url.borrow_mut() = Some(url.to_string()),
        }
    }

    fn with_webview(&self, f: impl FnOnce(&ICoreWebView2)) {
        let webview = self.native.borrow().as_ref().map(|n| n.webview.clone());
        if let Some(webview) = webview {
            f(&webview);
        }
    }

    fn with_controller(&self, f: impl FnOnce(&ICoreWebView2Controller)) {
        let controller = self.native.borrow().as_ref().map(|n| n.controller.clone());
        if let Some(controller) = controller {
            f(&controller);
        }
    }

    fn move_focus(&self) {
        // SAFETY: a COM call on a live page.
        self.with_controller(|c| unsafe {
            let result = c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            tracing::debug!("Edge (WebView2): focus moved to the page: {result:?}; keyboard now in {}", describe_focus(self.hwnd));
        });
    }

    /// Gives the keyboard back to Gate's window, if the page holds it.
    fn release_focus(&self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
        use windows::Win32::UI::WindowsAndMessaging::IsChild;
        // SAFETY: plain Win32 focus calls on Gate's own window.
        unsafe {
            let focus = GetFocus();
            if !focus.is_invalid() && focus != self.hwnd && IsChild(self.hwnd, focus).as_bool() {
                let _ = SetFocus(Some(self.hwnd));
            }
        }
    }

    fn close_native(&self) -> Option<Native> {
        let native = self.native.borrow_mut().take()?;
        // SAFETY: COM calls on the page being closed.
        unsafe {
            if let Some(mount) = &native.visual
                && native.mounted
            {
                let _ = mount.layer.RemoveVisual(&mount.visual);
            }
            let _ = native.controller.Close();
        }
        Some(native)
    }

    // ---- placement

    /// Mounts, places and clips the page for a presented frame.
    fn sync(&self, geometry: Geometry, has_area: bool, composition: Option<gpui::NativeComposition>) {
        self.geometry.set(geometry);
        self.has_area.set(has_area);
        if let Some(composition) = composition {
            self.remount(composition);
        }
        self.apply_geometry();
        self.update_visibility();
    }

    /// (Re)creates the page's visual for the renderer's device generation
    /// (the first frame, or after GPU recovery).
    fn remount(&self, composition: gpui::NativeComposition) {
        let mut native = self.native.borrow_mut();
        let Some(native) = native.as_mut() else { return };
        // Handles are only valid for their generation; an older one's
        // pointers may already be freed.
        if native.visual.as_ref().is_some_and(|m| m.generation >= composition.generation) {
            return;
        }
        // SAFETY: the renderer's device and layer, valid for this generation.
        let mounted = unsafe {
            let (Some(device), Some(layer)) = (
                IDCompositionDevice::from_raw_borrowed(&composition.device),
                IDCompositionVisual::from_raw_borrowed(&composition.layer),
            ) else {
                return;
            };
            device.CreateVisual().and_then(|visual| {
                let clip = device.CreateRectangleClip()?;
                visual.SetClip(&clip)?;
                native.composition.SetRootVisualTarget(&visual)?;
                Ok(Mount { visual, clip, layer: layer.clone(), generation: composition.generation })
            })
        };
        match mounted {
            Ok(mount) => {
                tracing::info!("Edge (WebView2): page mounted (device generation {})", mount.generation);
                native.visual = Some(mount);
                native.mounted = false;
                native.applied = None;
            }
            Err(error) => tracing::error!("Edge (WebView2): no visual for the page: {error}"),
        }
    }

    fn apply_geometry(&self) {
        let mut native = self.native.borrow_mut();
        let Some(native) = native.as_mut() else { return };
        let next = self.geometry.get();
        let previous = native.applied.replace(next);
        if previous == Some(next) {
            return;
        }
        // SAFETY: COM calls on the live page and its visual.
        unsafe {
            if let Some(mount) = &native.visual {
                if previous.is_none_or(|p| p.origin != next.origin) {
                    let _ = mount.visual.SetOffsetX2(next.origin.0);
                    let _ = mount.visual.SetOffsetY2(next.origin.1);
                }
                if previous.is_none_or(|p| p.clip != next.clip || p.radii != next.radii) {
                    let [left, top, right, bottom] = next.clip;
                    let [tl, tr, br, bl] = next.radii;
                    let clip = &mount.clip;
                    let _ = clip.SetLeft2(left);
                    let _ = clip.SetTop2(top);
                    let _ = clip.SetRight2(right);
                    let _ = clip.SetBottom2(bottom);
                    let _ = clip.SetTopLeftRadiusX2(tl);
                    let _ = clip.SetTopLeftRadiusY2(tl);
                    let _ = clip.SetTopRightRadiusX2(tr);
                    let _ = clip.SetTopRightRadiusY2(tr);
                    let _ = clip.SetBottomRightRadiusX2(br);
                    let _ = clip.SetBottomRightRadiusY2(br);
                    let _ = clip.SetBottomLeftRadiusX2(bl);
                    let _ = clip.SetBottomLeftRadiusY2(bl);
                }
            }
            if previous.is_none_or(|p| p.size != next.size) {
                let _ = native.controller.SetBounds(RECT { left: 0, top: 0, right: next.size.0, bottom: next.size.1 });
            }
            if next.scale > 0.
                && previous.is_none_or(|p| p.scale != next.scale)
                && let Ok(controller) = native.controller.cast::<ICoreWebView2Controller3>()
            {
                let _ = controller.SetRasterizationScale(f64::from(next.scale));
            }
        }
    }

    /// Shown pages render and are mounted; hidden ones are taken off screen
    /// and Edge throttles them (timers, painting) as background tabs.
    fn update_visibility(&self) {
        let visible = !self.hidden.get() && !self.closed.get() && self.has_area.get();
        let mut native = self.native.borrow_mut();
        let Some(native) = native.as_mut() else { return };
        // SAFETY: COM calls on the live page and its visual.
        unsafe {
            if native.shown.replace(visible) != Some(visible) {
                let _ = native.controller.SetIsVisible(visible);
            }
            if let Some(mount) = &native.visual
                && native.mounted != visible
            {
                let _ = if visible {
                    mount.layer.AddVisual(&mount.visual, true, None::<&IDCompositionVisual>)
                } else {
                    mount.layer.RemoveVisual(&mount.visual)
                };
                native.mounted = visible;
            }
        }
        if !visible {
            self.pressed.set(0);
        }
    }

    // ---- input

    fn point(&self, at: gpui::Point<f32>) -> POINT {
        let scale = self.geometry.get().scale.max(1.0);
        POINT { x: (at.x * scale).round() as i32, y: (at.y * scale).round() as i32 }
    }

    fn mouse(&self, kind: COREWEBVIEW2_MOUSE_EVENT_KIND, data: u32, at: gpui::Point<f32>, modifiers: &Modifiers) {
        let composition = self.native.borrow().as_ref().map(|n| n.composition.clone());
        if let Some(composition) = composition {
            // SAFETY: a COM call on the live page.
            let _ = unsafe { composition.SendMouseInput(kind, virtual_keys(self.pressed.get(), modifiers), data, self.point(at)) };
        }
    }
}

fn answer_pending(pending: Pending, accept: bool, input: Option<&str>) {
    // SAFETY: COM calls on event arguments held for this answer.
    unsafe {
        match pending {
            Pending::Dialog(args, deferral) => {
                if accept {
                    if let Some(text) = input {
                        let _ = args.SetResultText(&HSTRING::from(text));
                    }
                    let _ = args.Accept();
                }
                let _ = deferral.Complete();
            }
            Pending::Permission(args, deferral) => {
                let _ = args.SetState(if accept { COREWEBVIEW2_PERMISSION_STATE_ALLOW } else { COREWEBVIEW2_PERMISSION_STATE_DENY });
                let _ = deferral.Complete();
            }
        }
    }
}

impl Page for WebView2Page {
    fn load(&mut self, url: &str) {
        self.host.navigate(url);
    }
    fn back(&mut self) {
        // SAFETY: COM calls on the live page.
        self.host.with_webview(|w| unsafe {
            let _ = w.GoBack();
        });
    }
    fn forward(&mut self) {
        self.host.with_webview(|w| unsafe {
            let _ = w.GoForward();
        });
    }
    fn reload(&mut self) {
        if self.host.native.borrow().is_none() {
            // The browser process died: start the page over.
            Host::create(&self.host);
            return;
        }
        self.host.with_webview(|w| unsafe {
            let _ = w.Reload();
        });
    }
    fn stop(&mut self) {
        self.host.with_webview(|w| unsafe {
            let _ = w.Stop();
        });
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.host.hidden.set(hidden);
        self.host.update_visibility();
    }

    fn set_focus(&mut self, focused: bool) {
        self.host.focused.set(focused);
        if focused {
            self.host.move_focus();
        } else {
            self.host.release_focus();
        }
    }

    fn layout(&mut self, _bounds: Bounds<Pixels>, _scale: f32) {}

    fn input(&mut self, input: Input<'_>) {
        let host = &self.host;
        match input {
            Input::MouseDown { at, button, clicks, modifiers } => {
                let (bit, kind) = match button {
                    Button::Left if clicks >= 2 && clicks % 2 == 0 => (1, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK),
                    Button::Left => (1, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN),
                    Button::Right => (2, COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN),
                    Button::Middle => (4, COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN),
                };
                host.background_click.set(button == Button::Middle || modifiers.control);
                host.pressed.set(host.pressed.get() | bit);
                host.hovered.set(true);
                host.mouse(kind, 0, at, &modifiers);
            }
            Input::MouseUp { at, button, modifiers, .. } => {
                let (bit, kind) = match button {
                    Button::Left => (1, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP),
                    Button::Right => (2, COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP),
                    Button::Middle => (4, COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP),
                };
                host.pressed.set(host.pressed.get() & !bit);
                host.mouse(kind, 0, at, &modifiers);
            }
            Input::MouseMove { at, modifiers } => {
                host.hovered.set(true);
                host.mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, 0, at, &modifiers);
            }
            Input::MouseLeave => {
                if host.hovered.replace(false) {
                    host.mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE, 0, gpui::point(0., 0.), &Modifiers::default());
                }
            }
            Input::Scroll { at, delta, modifiers } => {
                let units = wheel_units(delta);
                if units.y != 0. {
                    host.mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL, units.y.round() as i32 as u32, at, &modifiers);
                }
                if units.x != 0. {
                    host.mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL, (-units.x.round()) as i32 as u32, at, &modifiers);
                }
            }
            // The page holds Windows' keyboard focus while focused: keys reach
            // it directly. One that came through GPUI arrived before it did.
            Input::KeyDown { .. } | Input::KeyUp { .. } => {
                if host.focused.get() {
                    host.move_focus();
                }
            }
        }
    }

    fn zoom(&mut self, zoom: Zoom) {
        const STEPS: [f64; 17] = [0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];
        self.host.with_controller(|c| unsafe {
            let mut now = 1.0;
            let _ = c.ZoomFactor(&mut now);
            let next = match zoom {
                Zoom::In => STEPS.iter().copied().find(|s| *s > now + 0.001).unwrap_or(5.0),
                Zoom::Out => STEPS.iter().rev().copied().find(|s| *s < now - 0.001).unwrap_or(0.25),
                Zoom::Reset => 1.0,
            };
            let _ = c.SetZoomFactor(next);
        });
    }

    fn zoom_percent(&self) -> u32 {
        let mut factor = 1.0;
        self.host.with_controller(|c| unsafe {
            let _ = c.ZoomFactor(&mut factor);
        });
        (factor * 100.0).round() as u32
    }

    fn set_muted(&mut self, muted: bool) {
        self.host.with_webview(|w| {
            if let Ok(w) = w.cast::<ICoreWebView2_8>() {
                // SAFETY: a COM call on the live page.
                let _ = unsafe { w.SetIsMuted(muted) };
            }
        });
    }

    /// Pauses the documents playing, or resumes the ones paused, as the
    /// other engines do; their watchers report the outcome.
    fn play_pause(&mut self) {
        let host = &self.host;
        let (command, wanted) = match host.playback() {
            Playback::Playing => ("pause", Playback::Playing),
            Playback::Paused => ("play", Playback::Paused),
            Playback::None => return,
        };
        let ids: Vec<String> = host.media.borrow().iter().filter(|(_, p)| **p == wanted).map(|(id, _)| id.clone()).collect();
        let message = HSTRING::from(serde_json::json!({ "gateMedia": command, "ids": ids }).to_string());
        // SAFETY: COM calls on the live page and its frames.
        host.with_webview(|w| unsafe {
            let _ = w.PostWebMessageAsJson(&message);
        });
        for frame in host.frames.borrow().iter() {
            let _ = unsafe { frame.PostWebMessageAsJson(&message) };
        }
    }

    fn answer(&mut self, accept: bool, input: Option<&str>) {
        let pending = self.host.ask.borrow_mut().take();
        if let Some(pending) = pending {
            answer_pending(pending, accept, input);
        }
        self.host.push(PageEvent::Ask(None));
    }

    fn show_dev_tools(&mut self) {
        self.host.with_webview(|w| unsafe {
            let _ = w.OpenDevToolsWindow();
        });
    }

    fn close(&mut self) {
        if self.host.closed.replace(true) {
            return;
        }
        self.host.replace_ask(None);
        self.host.close_native();
        self.host.push(PageEvent::Closed);
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window) {
        let scale = window.scale_factor();
        let mask = window.content_mask().bounds;
        let visible = bounds.intersect(&mask);
        let (x, y) = (f32::from(bounds.origin.x) * scale, f32::from(bounds.origin.y) * scale);
        let geometry = Geometry {
            origin: (x.round(), y.round()),
            size: (
                (f32::from(bounds.size.width) * scale).round().max(0.) as i32,
                (f32::from(bounds.size.height) * scale).round().max(0.) as i32,
            ),
            clip: [
                (f32::from(visible.origin.x) * scale - x).max(0.),
                (f32::from(visible.origin.y) * scale - y).max(0.),
                (f32::from(visible.origin.x + visible.size.width) * scale - x).max(0.),
                (f32::from(visible.origin.y + visible.size.height) * scale - y).max(0.),
            ],
            radii: [corners.top_left, corners.top_right, corners.bottom_right, corners.bottom_left].map(|r| f32::from(r) * scale),
            scale,
        };
        let has_area = f32::from(visible.size.width) > 0. && f32::from(visible.size.height) > 0.;
        self.host.painted.set(current_frame());
        queue_sweep(window);
        let composition = window.native_composition();
        let host = Rc::downgrade(&self.host);
        window.on_present(move || {
            if let Some(host) = host.upgrade() {
                host.sync(geometry, has_area, composition);
            }
        });
    }
}

impl Drop for WebView2Page {
    fn drop(&mut self) {
        self.close();
    }
}

// ---------------------------------------------------------------- helpers

/// The media watcher shared with the other engines.
const MEDIA_SCRIPT: &str = include_str!("../../js/media.js");

/// The watcher wrapped for WebView2: it reports through the document's
/// `chrome.webview` and obeys commands naming its id. Taken before the
/// page's scripts run, so they can't intercept it.
fn media_script() -> String {
    format!(
        r#"(() => {{
  const webview = window.chrome && window.chrome.webview;
  if (!webview) return;
  const post = webview.postMessage.bind(webview);
  const listen = webview.addEventListener.bind(webview);
  const id = Math.random().toString(36).slice(2) + Date.now().toString(36);
  const control = ({MEDIA_SCRIPT})((state) => post({{ gateMedia: state, id }}));
  listen("message", (e) => {{
    const m = e.data;
    if (m && typeof m.gateMedia === "string" && Array.isArray(m.ids) && m.ids.includes(id)) control(m.gateMedia);
  }});
}})();"#
    )
}

fn read_stream(stream: &IStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let mut read = 0u32;
        // SAFETY: reads into a buffer of the length given.
        let result = unsafe { stream.Read(chunk.as_mut_ptr().cast(), chunk.len() as u32, Some(&mut read)) };
        if result.is_err() || read == 0 || bytes.len() > 4 << 20 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read as usize]);
    }
    bytes
}

/// A PNG favicon as GPUI's BGRA.
fn favicon(png: &[u8]) -> Option<Arc<RenderImage>> {
    let mut image = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.into_rgba8();
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(image)])))
}

fn origin_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).to_string()
}

fn permission_name(kind: COREWEBVIEW2_PERMISSION_KIND) -> &'static str {
    match kind {
        COREWEBVIEW2_PERMISSION_KIND_CAMERA => "camera",
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE => "microphone",
        COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION => "location",
        COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS => "notifications",
        COREWEBVIEW2_PERMISSION_KIND_CLIPBOARD_READ => "clipboard",
        COREWEBVIEW2_PERMISSION_KIND_MULTIPLE_AUTOMATIC_DOWNLOADS => "multiple downloads",
        COREWEBVIEW2_PERMISSION_KIND_MIDI_SYSTEM_EXCLUSIVE_MESSAGES => "MIDI devices",
        COREWEBVIEW2_PERMISSION_KIND_LOCAL_FONTS => "local fonts",
        COREWEBVIEW2_PERMISSION_KIND_WINDOW_MANAGEMENT => "window management",
        COREWEBVIEW2_PERMISSION_KIND_FILE_READ_WRITE => "files",
        COREWEBVIEW2_PERMISSION_KIND_AUTOPLAY => "autoplay",
        COREWEBVIEW2_PERMISSION_KIND_OTHER_SENSORS => "sensors",
        _ => "something",
    }
}

/// Wheel units: GPUI lines (3 per notch) or pixels (100 per notch), as
/// Win32's WHEEL_DELTA (120 per notch).
fn wheel_units(delta: ScrollDelta) -> gpui::Point<f32> {
    const WHEEL_DELTA: f32 = 120.;
    match delta {
        ScrollDelta::Lines(lines) => gpui::point(lines.x * WHEEL_DELTA / 3., lines.y * WHEEL_DELTA / 3.),
        ScrollDelta::Pixels(pixels) => gpui::point(f32::from(pixels.x) * WHEEL_DELTA / 100., f32::from(pixels.y) * WHEEL_DELTA / 100.),
    }
}

fn virtual_keys(pressed: u32, modifiers: &Modifiers) -> COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS {
    let mut keys = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE.0;
    for (bit, key) in [
        (1, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON),
        (2, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON),
        (4, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON),
    ] {
        if pressed & bit != 0 {
            keys |= key.0;
        }
    }
    if modifiers.shift {
        keys |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT.0;
    }
    if modifiers.control {
        keys |= COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL.0;
    }
    COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(keys)
}

/// Windows system cursor ids (`IDC_*`) as GPUI cursors.
fn cursor_style(id: u32) -> CursorStyle {
    match id {
        32513 => CursorStyle::IBeam,
        32515 => CursorStyle::Crosshair,
        32642 => CursorStyle::ResizeUpLeftDownRight,
        32643 => CursorStyle::ResizeUpRightDownLeft,
        32644 => CursorStyle::ResizeLeftRight,
        32645 => CursorStyle::ResizeUpDown,
        32646 => CursorStyle::ResizeUpDown,
        32648 => CursorStyle::OperationNotAllowed,
        32649 => CursorStyle::PointingHand,
        _ => CursorStyle::Arrow,
    }
}

/// The GPUI keystroke for a key the page got, when an app shortcut could be
/// one (with a modifier other than Shift, or a function key).
fn accelerator(key: u32) -> Option<Keystroke> {
    // SAFETY: GetKeyState only reads the thread's keyboard state.
    let down = |vk: VIRTUAL_KEY| unsafe { GetKeyState(i32::from(vk.0)) } < 0;
    let (control, alt, platform, shift) = (down(VK_CONTROL), down(VK_MENU), down(VK_LWIN) || down(VK_RWIN), down(VK_SHIFT));
    let name = match key {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(key)?.to_ascii_lowercase().to_string(),
        0x70..=0x7B => format!("f{}", key - 0x6F),
        0x09 => "tab".into(),
        0x0D => "enter".into(),
        0x25 => "left".into(),
        0x26 => "up".into(),
        0x27 => "right".into(),
        0x28 => "down".into(),
        0xBC => ",".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xDB => "[".into(),
        0xDD => "]".into(),
        0xBA => ";".into(),
        0xDE => "'".into(),
        0xC0 => "`".into(),
        0xBD => "-".into(),
        0xBB => "=".into(),
        0xDC => "\\".into(),
        _ => return None,
    };
    if !(control || alt || platform || matches!(key, 0x70..=0x7B)) {
        return None;
    }
    let mut combo = String::new();
    for (held, prefix) in [(control, "ctrl-"), (alt, "alt-"), (platform, "win-"), (shift, "shift-")] {
        if held {
            combo.push_str(prefix);
        }
    }
    combo.push_str(&name);
    Keystroke::parse(&combo).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_units_match_win32_notches() {
        assert_eq!(wheel_units(ScrollDelta::Lines(gpui::point(0., 3.))).y, 120.);
        let pixels = wheel_units(ScrollDelta::Pixels(gpui::point(gpui::px(-50.), gpui::px(100.))));
        assert_eq!((pixels.x, pixels.y), (-60., 120.));
    }

    #[test]
    fn origins_are_hosts() {
        assert_eq!(origin_of("https://meet.example.com/room?x=1"), "meet.example.com");
        assert_eq!(origin_of("http://localhost:3000"), "localhost:3000");
    }
}

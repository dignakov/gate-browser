//! A web page as a GPUI view.

use crate::client::{self, Ask, Pixmap, Playback, Reply, Shared};
use crate::keys;
use cef::{ImplBrowser as _, ImplBrowserHost as _, ImplFrame as _, *};
use gpui::{
    App, Bounds, Context, Corners, DevicePixels, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, Pixels, Point, Render, RenderImage, ScrollDelta, ScrollWheelEvent,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, point, px, size,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// What a page did, for whoever shows it.
#[derive(Clone, Debug, PartialEq)]
pub enum WebEvent {
    Created,
    /// The page took keyboard focus (a click, or focus given to it).
    Focused,
    Url(String),
    Title(String),
    /// The page named a favicon (internal: the view fetches it).
    FaviconUrl(Option<String>),
    /// The favicon's pixels arrived, or it went away.
    Favicon,
    Loading { loading: bool, can_go_back: bool, can_go_forward: bool },
    Progress(f64),
    Fullscreen(bool),
    /// The page asked for a new window; we open it as a tab.
    OpenTab { url: String, background: bool },
    /// A question appeared or went away (see [`WebView::ask`]).
    Ask,
    /// The page started or stopped using the camera or microphone.
    Media { camera: bool, microphone: bool },
    /// The page's media started, paused or went away.
    Playback(Playback),
    /// The page's renderer died.
    Crashed,
    /// New pixels or cursor (internal; not emitted).
    Repaint,
    Closed,
}

/// A page zoom step (see [`WebView::zoom`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    Reset,
}

/// GPU-side state of one painted surface.
#[derive(Default)]
struct Tile {
    image: Option<Arc<RenderImage>>,
}

pub struct WebView {
    shared: Rc<RefCell<Shared>>,
    focus: FocusHandle,
    /// Where the page was last painted, in window coordinates.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    page_tile: Rc<RefCell<Tile>>,
    popup_tile: Rc<RefCell<Tile>>,
    /// Buttons held over the page, as CEF event flags.
    buttons: u32,
    focused: bool,
    hidden: bool,
    url: String,
    title: String,
    loading: bool,
    can_go_back: bool,
    can_go_forward: bool,
    progress: f64,
    ask: Option<Ask>,
    camera: bool,
    microphone: bool,
    crashed: bool,
    playback: Playback,
    muted: bool,
    /// Rounds the page's corners (where the pane's card is rounded).
    corners: Corners<Pixels>,
    closed: bool,
    /// Restored but not shown yet: no browser until it is (see [`Self::sleeping`]).
    asleep: bool,
}

impl EventEmitter<WebEvent> for WebView {}

impl Focusable for WebView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl WebView {
    /// A page loading `url`.
    pub fn new(url: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::empty(url, window, cx);
        view.create_browser();
        view
    }

    /// A page that shows `title` (and `favicon`) but starts its browser,
    /// loading `url`, only when first shown or navigated: restoring many
    /// spaces costs nothing until they are opened.
    pub fn sleeping(url: &str, title: &str, favicon: Option<Arc<RenderImage>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::empty(url, window, cx);
        view.asleep = true;
        view.hidden = true;
        view.loading = false;
        view.title = title.to_string();
        view.shared.borrow_mut().favicon = favicon;
        view
    }

    /// Restored, and not opened since.
    pub fn is_asleep(&self) -> bool {
        self.asleep
    }

    fn create_browser(&mut self) {
        self.asleep = false;
        self.loading = true;
        let url = self.url.clone();
        let url = url.as_str();
        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
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
            Some(&CefString::from(url)),
            Some(&browser_settings),
            None,
            None,
        );
        if created != 1 {
            tracing::error!("CEF could not create a browser for {url}");
        }
    }

    fn empty(url: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let shared = Rc::new(RefCell::new(Shared::new()));
        shared.borrow_mut().scale = window.scale_factor();

        let weak = cx.entity().downgrade();
        let drain_shared = shared.clone();
        crate::pump::add_drain(move |cx| {
            let Some(view) = weak.upgrade() else { return false };
            if !drain_shared.borrow().events.is_empty() {
                view.update(cx, |view, cx| view.apply_events(cx));
            }
            true
        });

        let focus = cx.focus_handle();
        cx.on_focus(&focus, window, |this, _, cx| {
            this.focused = true;
            this.with_host(|h| h.set_focus(1));
            cx.emit(WebEvent::Focused);
        })
        .detach();
        cx.on_blur(&focus, window, |this, _, _| {
            this.focused = false;
            this.with_host(|h| h.set_focus(0));
        })
        .detach();

        Self {
            shared,
            focus,
            bounds: Rc::default(),
            page_tile: Rc::default(),
            popup_tile: Rc::default(),
            buttons: 0,
            focused: false,
            hidden: false,
            url: url.to_string(),
            title: String::new(),
            loading: true,
            can_go_back: false,
            can_go_forward: false,
            progress: 0.0,
            ask: None,
            camera: false,
            microphone: false,
            crashed: false,
            playback: Playback::None,
            muted: false,
            corners: Corners::default(),
            closed: false,
            asleep: false,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn is_loading(&self) -> bool {
        self.loading
    }
    pub fn can_go_back(&self) -> bool {
        self.can_go_back
    }
    pub fn can_go_forward(&self) -> bool {
        self.can_go_forward
    }
    pub fn progress(&self) -> f64 {
        self.progress
    }
    /// The question the page waits on the user for, if any.
    pub fn ask(&self) -> Option<&Ask> {
        self.ask.as_ref()
    }
    pub fn favicon(&self) -> Option<Arc<RenderImage>> {
        self.shared.borrow().favicon.clone()
    }
    /// Whether the page is using the camera and the microphone.
    pub fn media(&self) -> (bool, bool) {
        (self.camera, self.microphone)
    }
    pub fn crashed(&self) -> bool {
        self.crashed
    }
    /// What the page's media is doing (counted once it played audibly).
    pub fn playback(&self) -> Playback {
        self.playback
    }

    /// Pauses the page's playing media, or resumes what it paused.
    pub fn play_pause(&mut self) {
        let (command, frames) = {
            let shared = self.shared.borrow();
            let (command, wanted) = match shared.playback() {
                Playback::Playing => ("pause", Playback::Playing),
                Playback::Paused => ("play", Playback::Paused),
                Playback::None => return,
            };
            let frames: Vec<String> = shared.media.iter().filter(|(_, s)| **s == wanted).map(|(f, _)| f.clone()).collect();
            (command, frames)
        };
        self.with_browser(|browser| {
            for id in &frames {
                let Some(frame) = browser.frame_by_identifier(Some(&CefString::from(id.as_str()))) else { continue };
                let Some(mut message) = process_message_create(Some(&CefString::from(crate::renderer::MEDIA_CONTROL))) else { continue };
                if let Some(args) = message.argument_list() {
                    args.set_string(0, Some(&CefString::from(command)));
                }
                frame.send_process_message(ProcessId::RENDERER, Some(&mut message));
            }
        });
    }
    pub fn muted(&self) -> bool {
        self.muted
    }
    pub fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.muted = muted;
        self.with_host(|h| h.set_audio_muted(muted as i32));
        cx.notify();
    }
    /// The unread count a page shows in its title, as "(3) Inbox" does.
    pub fn unread(&self) -> Option<u32> {
        let rest = self.title.trim_start().strip_prefix('(')?;
        let (count, _) = rest.split_once(')')?;
        count.trim_end_matches('+').parse().ok().filter(|n| *n > 0)
    }

    pub fn set_corners(&mut self, corners: Corners<Pixels>) {
        self.corners = corners;
    }

    /// Answers the page's question: OK/Allow or Cancel/Deny, with the text
    /// typed for a prompt.
    pub fn answer(&mut self, accept: bool, input: Option<&str>, cx: &mut Context<Self>) {
        let pending = self.shared.borrow_mut().ask.take();
        self.ask = None;
        cx.emit(WebEvent::Ask);
        cx.notify();
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

    pub fn load(&mut self, url: &str) {
        self.url = url.to_string();
        if self.asleep {
            return self.create_browser();
        }
        self.with_browser(|b| {
            if let Some(frame) = b.main_frame() {
                frame.load_url(Some(&CefString::from(url)));
            }
        });
    }
    pub fn back(&mut self) {
        self.with_browser(|b| b.go_back());
    }
    pub fn forward(&mut self) {
        self.with_browser(|b| b.go_forward());
    }
    pub fn reload(&mut self) {
        self.with_browser(|b| b.reload());
    }
    pub fn stop(&mut self) {
        self.with_browser(|b| b.stop_load());
    }
    /// Zooms the page as Chromium does: its steps (25% to 500%), and
    /// remembered for the site.
    pub fn zoom(&mut self, zoom: Zoom) {
        let command = match zoom {
            Zoom::In => ZoomCommand::IN,
            Zoom::Out => ZoomCommand::OUT,
            Zoom::Reset => ZoomCommand::RESET,
        };
        self.with_host(|h| h.zoom(command));
    }

    /// The page's zoom in percent (100: none).
    pub fn zoom_percent(&self) -> u32 {
        let mut level = 0.0;
        self.with_host(|h| level = h.zoom_level());
        // Chromium's zoom levels are powers of 1.2.
        (1.2f64.powf(level) * 100.0).round() as u32
    }

    pub fn show_dev_tools(&mut self) {
        // DevTools open in their own native window for now.
        self.with_host(|h| {
            let info = WindowInfo { runtime_style: RuntimeStyle::ALLOY, ..Default::default() };
            h.show_dev_tools(Some(&info), None, None, None);
        });
    }

    /// A page that is not shown stops rendering (and its timers slow down).
    pub fn set_hidden(&mut self, hidden: bool) {
        if self.asleep && !hidden {
            self.hidden = false;
            return self.create_browser();
        }
        if self.hidden != hidden {
            self.hidden = hidden;
            self.with_host(|h| h.was_hidden(hidden as i32));
        }
    }

    /// Closes the page. The view stops painting; drop it after.
    pub fn close(&mut self) {
        self.closed = true;
        self.with_host(|h| h.close_browser(1));
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

    fn apply_events(&mut self, cx: &mut Context<Self>) {
        let events = std::mem::take(&mut self.shared.borrow_mut().events);
        let mut repaint = false;
        for event in events {
            match &event {
                WebEvent::Repaint => {
                    repaint = true;
                    continue;
                }
                WebEvent::Created => {
                    // Sizes may have changed before the browser existed.
                    self.with_host(|h| {
                        h.notify_screen_info_changed();
                        h.was_resized();
                    });
                    if self.focused {
                        self.with_host(|h| h.set_focus(1));
                    }
                    if self.hidden {
                        self.with_host(|h| h.was_hidden(1));
                    }
                }
                WebEvent::Url(url) => self.url = url.clone(),
                WebEvent::FaviconUrl(url) => {
                    self.fetch_favicon(url.clone());
                    continue;
                }
                WebEvent::Ask => self.ask = self.shared.borrow().ask.as_ref().map(|(ask, _)| ask.clone()),
                WebEvent::Media { camera, microphone } => {
                    self.camera = *camera;
                    self.microphone = *microphone;
                }
                WebEvent::Crashed => self.crashed = true,
                WebEvent::Playback(playback) => self.playback = *playback,
                WebEvent::Title(title) => self.title = title.clone(),
                WebEvent::Loading { loading, can_go_back, can_go_forward } => {
                    self.loading = *loading;
                    if *loading {
                        self.crashed = false;
                    }
                    self.can_go_back = *can_go_back;
                    self.can_go_forward = *can_go_forward;
                }
                WebEvent::Progress(p) => self.progress = *p,
                _ => {}
            }
            cx.emit(event);
            repaint = true;
        }
        if repaint {
            cx.notify();
        }
    }

    /// Fetches the page's favicon; [`WebEvent::Favicon`] follows when the
    /// pixels arrive. Sized for the sidebar's tiles at the display's scale.
    fn fetch_favicon(&mut self, url: Option<String>) {
        let (scale, unchanged) = {
            let shared = self.shared.borrow();
            (shared.scale, shared.favicon_url == url)
        };
        if unchanged {
            return;
        }
        {
            let mut shared = self.shared.borrow_mut();
            shared.favicon_url = url.clone();
            if url.is_none() {
                shared.favicon = None;
                shared.push(WebEvent::Favicon);
            }
        }
        let Some(url) = url else { return };
        let mut callback = client::favicon_callback(&self.shared, scale);
        let size = (32.0 * scale).ceil() as u32;
        self.with_host(|h| h.download_image(Some(&CefString::from(url.as_str())), 1, size, 0, Some(&mut callback)));
    }

    /// Tells CEF the page's new size or scale, when they changed.
    fn layout(&self, bounds: Bounds<Pixels>, scale: f32) {
        self.bounds.set(bounds);
        let want = (f32::from(bounds.size.width).round() as i32, f32::from(bounds.size.height).round() as i32);
        let changed = {
            let mut shared = self.shared.borrow_mut();
            let size_changed = shared.view_size != want;
            let scale_changed = shared.scale != scale;
            shared.view_size = want;
            shared.scale = scale;
            (size_changed, scale_changed)
        };
        if changed.1 {
            self.with_host(|h| h.notify_screen_info_changed());
        }
        if changed.0 || changed.1 {
            self.with_host(|h| h.was_resized());
        }
    }

    fn mouse_event(&self, position: Point<Pixels>, modifiers: &gpui::Modifiers) -> MouseEvent {
        let origin = self.bounds.get().origin;
        MouseEvent {
            x: f32::from(position.x - origin.x).round() as i32,
            y: f32::from(position.y - origin.y).round() as i32,
            modifiers: keys::modifiers(modifiers) | self.buttons,
        }
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some((button, flag)) = button(ev.button) else {
            // Back/forward mouse buttons navigate.
            if let MouseButton::Navigate(dir) = ev.button {
                match dir {
                    gpui::NavigationDirection::Back => self.back(),
                    gpui::NavigationDirection::Forward => self.forward(),
                }
            }
            return;
        };
        self.buttons |= flag;
        let event = self.mouse_event(ev.position, &ev.modifiers);
        let clicks = ev.click_count.clamp(1, 3) as i32;
        self.with_host(|h| h.send_mouse_click_event(Some(&event), button, 0, clicks));
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let Some((button, flag)) = button(ev.button) else { return };
        if self.buttons & flag == 0 {
            return;
        }
        let event = self.mouse_event(ev.position, &ev.modifiers);
        self.buttons &= !flag;
        let clicks = ev.click_count.clamp(1, 3) as i32;
        self.with_host(|h| h.send_mouse_click_event(Some(&event), button, 1, clicks));
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, _cx: &mut Context<Self>) {
        let inside = self.bounds.get().contains(&ev.position);
        // Outside the page only a drag that started on it continues.
        if !inside && self.buttons == 0 {
            return;
        }
        let _ = window;
        let event = self.mouse_event(ev.position, &ev.modifiers);
        self.with_host(|h| h.send_mouse_move_event(Some(&event), 0));
    }

    fn mouse_leave(&mut self) {
        if self.buttons != 0 {
            return;
        }
        let event = MouseEvent { x: -1, y: -1, modifiers: 0 };
        self.with_host(|h| h.send_mouse_move_event(Some(&event), 1));
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Chromium scrolls 40px per wheel line on Linux.
        let delta = match ev.delta {
            ScrollDelta::Pixels(p) => point(f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => point(l.x * 40.0, l.y * 40.0),
        };
        let event = self.mouse_event(ev.position, &ev.modifiers);
        self.with_host(|h| h.send_mouse_wheel_event(Some(&event), delta.x.round() as i32, delta.y.round() as i32));
        cx.stop_propagation();
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let mut flags = keys::modifiers(&k.modifiers);
        if ev.is_held {
            flags |= keys::IS_REPEAT;
        }
        let code = keys::windows_key_code(k);
        let mut event = KeyEvent {
            type_: KeyEventType::RAWKEYDOWN,
            modifiers: flags,
            windows_key_code: code,
            ..Default::default()
        };
        self.with_host(|h| h.send_key_event(Some(&event)));
        if let Some(ch) = keys::typed_char(k) {
            event.type_ = KeyEventType::CHAR;
            event.windows_key_code = ch as i32;
            event.character = ch;
            event.unmodified_character = ch;
            self.with_host(|h| h.send_key_event(Some(&event)));
        }
        cx.stop_propagation();
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let event = KeyEvent {
            type_: KeyEventType::KEYUP,
            modifiers: keys::modifiers(&k.modifiers),
            windows_key_code: keys::windows_key_code(k),
            ..Default::default()
        };
        self.with_host(|h| h.send_key_event(Some(&event)));
        cx.stop_propagation();
    }
}

fn button(b: MouseButton) -> Option<(MouseButtonType, u32)> {
    match b {
        MouseButton::Left => Some((MouseButtonType::LEFT, keys::LEFT_BUTTON)),
        MouseButton::Middle => Some((MouseButtonType::MIDDLE, keys::MIDDLE_BUTTON)),
        MouseButton::Right => Some((MouseButtonType::RIGHT, keys::RIGHT_BUTTON)),
        MouseButton::Navigate(_) => None,
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        if !self.closed {
            self.with_host(|h| h.close_browser(1));
        }
    }
}

/// Copies a frame's damage into its atlas tile and paints it at `origin`.
fn paint_frame(
    frame: &mut Pixmap,
    tile: &mut Tile,
    origin: Point<Pixels>,
    clip: Bounds<Pixels>,
    corners: Corners<Pixels>,
    scale: f32,
    window: &mut Window,
) {
    if frame.width <= 0 || frame.height <= 0 {
        return;
    }
    let pixels = size(DevicePixels(frame.width), DevicePixels(frame.height));
    let whole = Bounds::new(point(DevicePixels(0), DevicePixels(0)), pixels);
    if tile.image.as_ref().map(|i| i.size(0)) != Some(pixels) {
        if let Some(old) = tile.image.take() {
            let _ = window.drop_image(old);
        }
        tile.image = Some(Arc::new(RenderImage::blank(pixels)));
    }
    let image = tile.image.clone().unwrap();
    // A new tile, or one the atlas dropped, needs the whole frame.
    let created = match window.insert_blank_image(&image, 0) {
        Ok(created) => created,
        Err(error) => {
            tracing::warn!("web frame tile: {error:#}");
            return;
        }
    };
    let damage = std::mem::take(&mut frame.damage);
    let regions = if created { vec![whole] } else { damage };
    let stride = frame.width as u32 * 4;
    for region in regions {
        let region = region.intersect(&whole);
        if region.size.width.0 <= 0 || region.size.height.0 <= 0 {
            continue;
        }
        let start = (region.origin.y.0 as usize * frame.width as usize + region.origin.x.0 as usize) * 4;
        let update = gpui::AtlasTileUpdate::Pixels { bytes: &frame.pixels[start..], bytes_per_row: stride };
        match window.update_image(&image, 0, region, update, None) {
            Ok(true) => {}
            Ok(false) => tracing::error!("the atlas cannot update web frames in place"),
            Err(error) => tracing::warn!("web frame upload: {error:#}"),
        }
    }
    let shown = size(px(frame.width as f32 / scale), px(frame.height as f32 / scale));
    let bounds = Bounds::new(origin, shown);
    window.with_content_mask(Some(gpui::ContentMask { bounds: clip }), |window| {
        let _ = window.paint_image(bounds, corners, image, 0, false);
    });
}

impl Render for WebView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        let shared = self.shared.clone();
        let page_tile = self.page_tile.clone();
        let popup_tile = self.popup_tile.clone();
        let cursor = shared.borrow().cursor;
        let corners = self.corners;
        div()
            .id("web-view")
            .size_full()
            .track_focus(&self.focus)
            .cursor(cursor)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Back), cx.listener(Self::mouse_down))
            .on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Forward), cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Middle, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_hover(cx.listener(|this, hovered: &bool, _, _| {
                if !hovered {
                    this.mouse_leave();
                }
            }))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        entity.read(cx).layout(bounds, window.scale_factor());
                    },
                    move |bounds, _, window, _cx| {
                        let scale = window.scale_factor();
                        let mut shared = shared.borrow_mut();
                        let shared = &mut *shared;
                        paint_frame(&mut shared.page, &mut page_tile.borrow_mut(), bounds.origin, bounds, corners, scale, window);
                        if let Some(rect) = shared.popup_rect.clone() {
                            let origin = bounds.origin + point(px(rect.x as f32), px(rect.y as f32));
                            paint_frame(&mut shared.popup, &mut popup_tile.borrow_mut(), origin, bounds, Corners::default(), scale, window);
                        }
                    },
                )
                .size_full(),
            )
    }
}

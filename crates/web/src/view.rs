//! [`WebView`]: one page as a GPUI view, on whichever engine renders it. It
//! keeps what the page reported, forwards input, and has the engine paint.

use crate::engine::{self, Button, Input, Page, PageEvent};
use crate::{Ask, Playback, Zoom};
use gpui::{
    App, Bounds, Context, Corners, CursorStyle, EventEmitter, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, RenderImage, ScrollWheelEvent, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, point,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// What a page did, for the app.
#[derive(Clone, Debug)]
pub enum WebEvent {
    /// The page took keyboard focus (a click, or focus given to it).
    Focused,
    Url(String),
    Title(String),
    /// The favicon's pixels arrived, or it went away (see [`WebView::favicon`]).
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
    Closed,
}

pub struct WebView {
    /// Its engine's page; none while asleep (see [`Self::sleeping`]).
    page: Option<Box<dyn Page>>,
    events: engine::Events,
    focus: FocusHandle,
    /// Where the page was last laid out, in window coordinates.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Buttons held down on the page (a drag continues outside it).
    buttons: u8,
    focused: bool,
    hidden: bool,
    url: String,
    title: String,
    favicon: Option<Arc<RenderImage>>,
    cursor: CursorStyle,
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
        view.wake(window);
        view
    }

    /// A page that shows `title` (and `favicon`) but starts its engine page,
    /// loading `url`, only when first shown or navigated: restoring many
    /// spaces costs nothing until they are opened.
    pub fn sleeping(url: &str, title: &str, favicon: Option<Arc<RenderImage>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::empty(url, window, cx);
        view.hidden = true;
        view.loading = false;
        view.title = title.to_string();
        view.favicon = favicon;
        view
    }

    /// Restored, and not opened since.
    pub fn is_asleep(&self) -> bool {
        self.page.is_none() && !self.closed
    }

    fn empty(url: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (events, receiver) = async_channel::unbounded();
        // What the engine reports, in order, a batch at a time.
        cx.spawn(async move |this, cx| {
            while let Ok(first) = receiver.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = receiver.try_recv() {
                    batch.push(more);
                }
                if this.update(cx, |view, cx| view.apply(batch, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();

        let focus = cx.focus_handle();
        cx.on_focus(&focus, window, |this, _, cx| {
            this.focused = true;
            if let Some(page) = this.page.as_mut() {
                page.set_focus(true);
            }
            cx.emit(WebEvent::Focused);
        })
        .detach();
        cx.on_blur(&focus, window, |this, _, _| {
            this.focused = false;
            if let Some(page) = this.page.as_mut() {
                page.set_focus(false);
            }
        })
        .detach();

        Self {
            page: None,
            events,
            focus,
            bounds: Rc::default(),
            buttons: 0,
            focused: false,
            hidden: false,
            url: url.to_string(),
            title: String::new(),
            favicon: None,
            cursor: CursorStyle::Arrow,
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
        }
    }

    /// Starts the engine's page for `self.url`, with what the view holds.
    fn wake(&mut self, window: &mut Window) {
        if self.page.is_some() || self.closed {
            return;
        }
        self.loading = true;
        let mut page = engine::new_page(&self.url, self.events.clone(), window);
        page.set_hidden(self.hidden);
        page.set_focus(self.focused);
        if self.muted {
            page.set_muted(true);
        }
        self.page = Some(page);
    }

    fn with_page(&mut self, f: impl FnOnce(&mut dyn Page)) {
        if let Some(page) = self.page.as_mut() {
            f(page.as_mut());
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
        self.favicon.clone()
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
    pub fn muted(&self) -> bool {
        self.muted
    }
    /// The unread count a page shows in its title, as "(3) Inbox" does.
    pub fn unread(&self) -> Option<u32> {
        let rest = self.title.trim_start().strip_prefix('(')?;
        let (count, _) = rest.split_once(')')?;
        count.trim_end_matches('+').parse().ok().filter(|n| *n > 0)
    }

    /// Pauses the page's playing media, or resumes what it paused.
    pub fn play_pause(&mut self) {
        self.with_page(|p| p.play_pause());
    }
    pub fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.muted = muted;
        self.with_page(|p| p.set_muted(muted));
        cx.notify();
    }
    pub fn set_corners(&mut self, corners: Corners<Pixels>) {
        self.corners = corners;
    }

    /// Answers the page's question: OK/Allow or Cancel/Deny, with the text
    /// typed for a prompt.
    pub fn answer(&mut self, accept: bool, input: Option<&str>, cx: &mut Context<Self>) {
        self.ask = None;
        self.with_page(|p| p.answer(accept, input));
        cx.emit(WebEvent::Ask);
        cx.notify();
    }

    /// Goes to `url`, starting the engine's page if it sleeps.
    pub fn load(&mut self, url: &str) {
        self.url = url.to_string();
        // A sleeping page wakes (on the next paint) at its new address.
        self.with_page(|p| p.load(url));
    }
    pub fn back(&mut self) {
        self.with_page(|p| p.back());
    }
    pub fn forward(&mut self) {
        self.with_page(|p| p.forward());
    }
    pub fn reload(&mut self) {
        self.with_page(|p| p.reload());
    }
    pub fn stop(&mut self) {
        self.with_page(|p| p.stop());
    }
    /// Zooms the page as its engine does, remembered for the site.
    pub fn zoom(&mut self, zoom: Zoom) {
        self.with_page(|p| p.zoom(zoom));
    }
    /// The page's zoom in percent (100: none).
    pub fn zoom_percent(&self) -> u32 {
        self.page.as_ref().map_or(100, |p| p.zoom_percent())
    }
    pub fn show_dev_tools(&mut self) {
        self.with_page(|p| p.show_dev_tools());
    }

    /// A page that is not shown stops rendering (and its timers slow down).
    /// A sleeping page wakes when first shown (on its next paint).
    pub fn set_hidden(&mut self, hidden: bool) {
        if self.hidden != hidden {
            self.hidden = hidden;
            self.with_page(|p| p.set_hidden(hidden));
        }
    }

    /// Closes the page. The view stops painting; drop it after.
    pub fn close(&mut self) {
        self.closed = true;
        self.with_page(|p| p.close());
    }

    fn apply(&mut self, batch: Vec<PageEvent>, cx: &mut Context<Self>) {
        for event in batch {
            let out = match event {
                PageEvent::Repaint => None,
                PageEvent::Pump => {
                    self.with_page(|p| p.pump());
                    None
                }
                PageEvent::Cursor(cursor) => {
                    self.cursor = cursor;
                    None
                }
                PageEvent::Url(url) => {
                    self.url = url.clone();
                    Some(WebEvent::Url(url))
                }
                PageEvent::Title(title) => {
                    self.title = title.clone();
                    Some(WebEvent::Title(title))
                }
                PageEvent::Favicon(favicon) => {
                    self.favicon = favicon;
                    Some(WebEvent::Favicon)
                }
                PageEvent::Loading { loading, can_go_back, can_go_forward } => {
                    self.loading = loading;
                    if loading {
                        self.crashed = false;
                    }
                    self.can_go_back = can_go_back;
                    self.can_go_forward = can_go_forward;
                    Some(WebEvent::Loading { loading, can_go_back, can_go_forward })
                }
                PageEvent::Progress(p) => {
                    self.progress = p;
                    Some(WebEvent::Progress(p))
                }
                PageEvent::Fullscreen(f) => Some(WebEvent::Fullscreen(f)),
                PageEvent::OpenTab { url, background } => Some(WebEvent::OpenTab { url, background }),
                PageEvent::Ask(ask) => {
                    self.ask = ask;
                    Some(WebEvent::Ask)
                }
                PageEvent::Media { camera, microphone } => {
                    self.camera = camera;
                    self.microphone = microphone;
                    Some(WebEvent::Media { camera, microphone })
                }
                PageEvent::Playback(playback) => {
                    self.playback = playback;
                    Some(WebEvent::Playback(playback))
                }
                PageEvent::Crashed => {
                    self.crashed = true;
                    Some(WebEvent::Crashed)
                }
                PageEvent::Closed => Some(WebEvent::Closed),
            };
            if let Some(event) = out {
                cx.emit(event);
            }
        }
        cx.notify();
    }

    /// A position in the page's coordinates.
    fn at(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self.bounds.get().origin;
        point(f32::from(position.x - origin.x), f32::from(position.y - origin.y))
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(button) = button(ev.button) else {
            // Back/forward mouse buttons navigate.
            if let MouseButton::Navigate(dir) = ev.button {
                match dir {
                    gpui::NavigationDirection::Back => self.back(),
                    gpui::NavigationDirection::Forward => self.forward(),
                }
            }
            return;
        };
        self.buttons += 1;
        let at = self.at(ev.position);
        self.with_page(|p| p.input(Input::MouseDown { at, button, clicks: ev.click_count, modifiers: ev.modifiers }));
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, ev: &MouseUpEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        let Some(button) = button(ev.button) else { return };
        if self.buttons == 0 {
            return;
        }
        self.buttons -= 1;
        let at = self.at(ev.position);
        self.with_page(|p| p.input(Input::MouseUp { at, button, clicks: ev.click_count, modifiers: ev.modifiers }));
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        // Outside the page only a drag that started on it continues.
        if !self.bounds.get().contains(&ev.position) && self.buttons == 0 {
            return;
        }
        let at = self.at(ev.position);
        self.with_page(|p| p.input(Input::MouseMove { at, modifiers: ev.modifiers }));
    }

    fn mouse_leave(&mut self) {
        if self.buttons == 0 {
            self.with_page(|p| p.input(Input::MouseLeave));
        }
    }

    fn scroll(&mut self, ev: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let at = self.at(ev.position);
        self.with_page(|p| p.input(Input::Scroll { at, delta: ev.delta, modifiers: ev.modifiers }));
        cx.stop_propagation();
    }

    fn key_down(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_page(|p| p.input(Input::KeyDown { keystroke: &ev.keystroke, repeat: ev.is_held }));
        cx.stop_propagation();
    }

    fn key_up(&mut self, ev: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.with_page(|p| p.input(Input::KeyUp { keystroke: &ev.keystroke }));
        cx.stop_propagation();
    }
}

fn button(b: MouseButton) -> Option<Button> {
    match b {
        MouseButton::Left => Some(Button::Left),
        MouseButton::Middle => Some(Button::Middle),
        MouseButton::Right => Some(Button::Right),
        MouseButton::Navigate(_) => None,
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        self.close();
    }
}

impl Render for WebView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout_view = cx.entity();
        let paint_view = cx.entity();
        let bounds_cell = self.bounds.clone();
        div()
            .id("web-view")
            .size_full()
            .track_focus(&self.focus)
            .cursor(self.cursor)
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
                        bounds_cell.set(bounds);
                        let scale = window.scale_factor();
                        layout_view.update(cx, |view, _| {
                            // A sleeping page wakes when it is first shown.
                            if !view.hidden {
                                view.wake(window);
                            }
                            view.with_page(|p| p.layout(bounds, scale));
                        });
                    },
                    move |bounds, _, window, cx| {
                        paint_view.update(cx, |view, _| {
                            let corners = view.corners;
                            view.with_page(|p| p.paint(bounds, corners, window));
                        });
                    },
                )
                .size_full(),
            )
    }
}

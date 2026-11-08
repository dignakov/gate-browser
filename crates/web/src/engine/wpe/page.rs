//! A page on the WPE helper.

use super::present::{GPUI_SCROLL_SCALE, Presenter};
use super::{Route, Worker};
use crate::engine::{Button, Events, Input, Page};
use crate::{Playback, Zoom};
use gpui::{Bounds, Corners, Modifiers, Pixels, RenderImage, ScrollDelta, Window};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};

/// Zoom levels, as Chromium steps them.
const ZOOM_STEPS: &[f64] = &[0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 5.0];

/// Each site's zoom, for this run.
static SITE_ZOOM: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);

pub(super) fn host(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The zoom `url`'s site last had (1: none).
pub(super) fn site_zoom(url: &str) -> f64 {
    let zooms = SITE_ZOOM.lock().unwrap();
    host(url).and_then(|h| zooms.as_ref()?.get(h).copied()).unwrap_or(1.0)
}

/// What the page's vblank callbacks share with it.
struct Shown {
    presenter: Presenter,
    image: Option<Arc<RenderImage>>,
    hidden: bool,
    closed: bool,
}

pub(crate) struct WpePage {
    worker: Arc<Worker>,
    route: Arc<Route>,
    id: u32,
    shown: Rc<RefCell<Shown>>,
    /// Device pixels and scale the helper last heard.
    geometry: Option<(u32, u32, u32)>,
    active: Option<bool>,
}

impl WpePage {
    pub(crate) fn new(url: &str, events: Events, window: &mut Window) -> Self {
        let (worker, id, route) = super::add_page(events, window.dmabuf_target());
        worker.send(id, json!({"cmd":"create"}));
        let page = Self::with(worker, id, route);
        page.send(json!({"cmd":"load","url":url}));
        page
    }

    /// The page the helper opened for another (`window.open`), loading
    /// what its opener asked for.
    pub(crate) fn adopt(opened: super::Opened) -> Self {
        // The helper that opened it is the one running.
        let worker = super::worker().expect("a page the helper opened has a helper");
        Self::with(worker, opened.id, opened.route)
    }

    fn with(worker: Arc<Worker>, id: u32, route: Arc<Route>) -> Self {
        let presenter = Presenter::new(worker.outbox.clone(), id);
        Self {
            worker,
            route,
            id,
            shown: Rc::new(RefCell::new(Shown { presenter, image: None, hidden: false, closed: false })),
            geometry: None,
            active: None,
        }
    }

    fn send(&self, command: Value) {
        self.worker.send(self.id, command);
    }

    fn pointer(&self, kind: &str, at: gpui::Point<f32>, button: u32, clicks: usize, modifiers: &Modifiers) {
        self.send(json!({"cmd":kind,"x":at.x,"y":at.y,"button":button,"count":clicks,"mods":modifiers_mask(modifiers)}));
    }

    /// Feeds WebKit vblanks while it wants them and the page is shown.
    fn start_vblanks(&self, window: &Window) {
        let mut shown = self.shown.borrow_mut();
        if shown.presenter.ticking || !self.wants_vblank(&shown) {
            return;
        }
        shown.presenter.ticking = true;
        let weak = Rc::downgrade(&self.shown);
        let route = self.route.clone();
        window.on_next_display_frame(move |window, _| vblank(weak, route, window));
    }

    fn wants_vblank(&self, shown: &Shown) -> bool {
        wants_vblank(&self.route, shown)
    }
}

fn wants_vblank(route: &Route, shown: &Shown) -> bool {
    !shown.hidden && !shown.closed && route.surface.lock().unwrap().vsync
}

/// One display refresh: a vblank for WebKit, and another callback while it
/// still wants them.
fn vblank(weak: Weak<RefCell<Shown>>, route: Arc<Route>, window: &mut Window) {
    let Some(shown) = weak.upgrade() else { return };
    let again = {
        let mut shown = shown.borrow_mut();
        if wants_vblank(&route, &shown) {
            shown.presenter.vblank();
            true
        } else {
            shown.presenter.stop_ticking();
            false
        }
    };
    if again {
        window.on_next_display_frame(move |window, _| vblank(weak, route, window));
    }
}

/// GDK-style modifier bits, as the helper reads them.
fn modifiers_mask(m: &Modifiers) -> u32 {
    u32::from(m.shift) | (u32::from(m.control) << 2) | (u32::from(m.alt) << 3) | (u32::from(m.platform) << 26)
}

fn button_number(button: Button) -> u32 {
    match button {
        Button::Left => 1,
        Button::Middle => 2,
        Button::Right => 3,
    }
}

/// GPUI's name for a key, as an XKB keysym name (or the character it types).
fn key_name(keystroke: &gpui::Keystroke) -> &str {
    let printable = keystroke.key_char.as_deref().filter(|s| {
        s.chars().count() == 1 && !s.chars().any(char::is_control) && !keystroke.modifiers.control && !keystroke.modifiers.platform
    });
    match printable.unwrap_or(keystroke.key.as_str()) {
        "enter" => "Return",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "escape" => "Escape",
        "tab" => "Tab",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "space" => "space",
        "shift" => "Shift_L",
        "control" => "Control_L",
        "alt" => "Alt_L",
        "platform" | "super" => "Super_L",
        "capslock" => "Caps_Lock",
        "f1" => "F1",
        "f2" => "F2",
        "f3" => "F3",
        "f4" => "F4",
        "f5" => "F5",
        "f6" => "F6",
        "f7" => "F7",
        "f8" => "F8",
        "f9" => "F9",
        "f10" => "F10",
        "f11" => "F11",
        "f12" => "F12",
        key => key,
    }
}

impl Page for WpePage {
    fn load(&mut self, url: &str) {
        self.send(json!({"cmd":"load","url":url}));
    }
    fn back(&mut self) {
        self.send(json!({"cmd":"back"}));
    }
    fn forward(&mut self) {
        self.send(json!({"cmd":"forward"}));
    }
    fn reload(&mut self) {
        self.send(json!({"cmd":"reload"}));
    }
    fn stop(&mut self) {
        self.send(json!({"cmd":"stop"}));
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.shown.borrow_mut().hidden = hidden;
        self.send(json!({"cmd":"visible","value":u8::from(!hidden)}));
    }

    fn set_focus(&mut self, focused: bool) {
        self.send(json!({"cmd":"focus","value":u8::from(focused)}));
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        let geometry = (
            (f32::from(bounds.size.width) * scale).round().clamp(1., 8192.) as u32,
            (f32::from(bounds.size.height) * scale).round().clamp(1., 8192.) as u32,
            scale.to_bits(),
        );
        if self.geometry != Some(geometry) {
            self.geometry = Some(geometry);
            self.send(json!({"cmd":"resize","width":geometry.0,"height":geometry.1,"scale":scale}));
        }
    }

    fn input(&mut self, input: Input<'_>) {
        match input {
            Input::MouseDown { at, button, clicks, modifiers } => self.pointer("down", at, button_number(button), clicks, &modifiers),
            Input::MouseUp { at, button, clicks, modifiers } => self.pointer("up", at, button_number(button), clicks, &modifiers),
            Input::MouseMove { at, modifiers } => self.pointer("move", at, 0, 0, &modifiers),
            Input::MouseLeave => self.send(json!({"cmd":"leave"})),
            Input::Scroll { at, delta, modifiers } => {
                let (dx, dy, precise) = match delta {
                    ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y), true),
                    ScrollDelta::Lines(l) => (l.x, l.y, false),
                };
                self.send(json!({
                    "cmd": "scroll",
                    "x": at.x,
                    "y": at.y,
                    "dx": dx / GPUI_SCROLL_SCALE,
                    "dy": dy / GPUI_SCROLL_SCALE,
                    "precise": precise,
                    "stop": false,
                    "mods": modifiers_mask(&modifiers),
                }));
            }
            Input::KeyDown { keystroke, .. } => {
                self.send(json!({"cmd":"key_down","key":key_name(keystroke),"mods":modifiers_mask(&keystroke.modifiers)}));
            }
            Input::KeyUp { keystroke } => {
                self.send(json!({"cmd":"key_up","key":key_name(keystroke),"mods":modifiers_mask(&keystroke.modifiers)}));
            }
        }
    }

    fn share_clipboard(&mut self, text: Option<String>) {
        self.send(json!({"cmd":"clipboard","text":text.unwrap_or_default()}));
    }

    fn choose(&mut self, index: Option<usize>) {
        match index {
            Some(index) => self.send(json!({"cmd":"option","index":index})),
            None => self.send(json!({"cmd":"dismiss-menu"})),
        }
    }

    /// Zooms in Chromium's steps, remembered for the site.
    fn zoom(&mut self, zoom: Zoom) {
        let current = *self.route.zoom.lock().unwrap();
        let level = match zoom {
            Zoom::In => ZOOM_STEPS.iter().copied().find(|s| *s > current + 0.001).unwrap_or(current),
            Zoom::Out => ZOOM_STEPS.iter().rev().copied().find(|s| *s < current - 0.001).unwrap_or(current),
            Zoom::Reset => 1.0,
        };
        *self.route.zoom.lock().unwrap() = level;
        let url = self.route.state.lock().unwrap().url.clone();
        if let Some(host) = host(&url) {
            SITE_ZOOM.lock().unwrap().get_or_insert_default().insert(host.to_string(), level);
        }
        self.send(json!({"cmd":"zoom","level":level}));
    }

    fn zoom_percent(&self) -> u32 {
        (*self.route.zoom.lock().unwrap() * 100.0).round() as u32
    }

    fn set_muted(&mut self, muted: bool) {
        self.send(json!({"cmd":"mute","value":u8::from(muted)}));
    }

    fn play_pause(&mut self) {
        let command = match *self.route.playback.lock().unwrap() {
            Playback::Playing => "pause",
            Playback::Paused => "play",
            Playback::None => return,
        };
        self.send(json!({"cmd":"media","command":command}));
    }

    fn answer(&mut self, accept: bool, input: Option<&str>) {
        self.send(json!({"cmd":"answer","accept":u8::from(accept),"text":input.unwrap_or_default()}));
    }

    fn show_dev_tools(&mut self) {
        tracing::info!("WPE WebKit: developer tools are not available yet");
    }

    fn choose_context(&mut self, index: Option<usize>) {
        self.send(json!({"cmd":"context","index":index.map_or(-1, |i| i as i64)}));
    }

    fn cancel_download(&mut self, id: u64) {
        self.send(json!({"cmd":"cancel-download","download":id}));
    }

    fn notification_clicked(&mut self, id: u64) {
        self.send(json!({"cmd":"notification-clicked","notification":id}));
    }

    fn notification_closed(&mut self, id: u64) {
        self.send(json!({"cmd":"notification-closed","notification":id}));
    }

    fn close(&mut self) {
        let mut shown = self.shown.borrow_mut();
        if !shown.closed {
            shown.closed = true;
            self.send(json!({"cmd":"close"}));
            self.worker.forget(self.id);
            let _ = self.route.events.try_send(crate::engine::PageEvent::Closed);
        }
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window) {
        let active = window.is_window_active();
        if self.active.replace(active) != Some(active) {
            self.send(json!({"cmd":"active","value":u8::from(active)}));
        }
        {
            let mut shown = self.shown.borrow_mut();
            let shown = &mut *shown;
            shown.presenter.paint(&self.route.surface, &mut shown.image, bounds, corners, window);
        }
        self.start_vblanks(window);
    }
}

impl Drop for WpePage {
    fn drop(&mut self) {
        self.close();
    }
}

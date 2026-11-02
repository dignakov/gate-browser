//! Web engines behind one interface. [`crate::WebView`] is the same on every
//! platform; an engine renders its pages, takes commands and input through
//! [`Page`], and reports what pages do as [`PageEvent`]s on a channel (which
//! also works from an engine's own threads).

pub(crate) mod cef;

use crate::{Ask, Playback, Zoom};
use gpui::{Bounds, Corners, CursorStyle, Keystroke, Modifiers, Pixels, Point, RenderImage, ScrollDelta, Window};
use std::sync::Arc;

/// What a page did, from its engine to its view.
pub(crate) enum PageEvent {
    Url(String),
    Title(String),
    /// The page's icon, or none.
    Favicon(Option<Arc<RenderImage>>),
    Loading { loading: bool, can_go_back: bool, can_go_forward: bool },
    Progress(f64),
    Fullscreen(bool),
    /// The page asked for a new window; it opens as a space.
    OpenTab { url: String, background: bool },
    /// The question the page now waits on, or none.
    Ask(Option<Ask>),
    Media { camera: bool, microphone: bool },
    Playback(Playback),
    Crashed,
    Cursor(CursorStyle),
    /// New pixels to paint.
    Repaint,
    /// The engine wants a turn with the page on the main thread
    /// ([`Page::pump`]): work its callbacks couldn't do in place. (Only
    /// Linux's engines need one so far.)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Pump,
    Closed,
}

pub(crate) type Events = async_channel::Sender<PageEvent>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Button {
    Left,
    Middle,
    Right,
}

/// Input for a page, in its coordinates (logical pixels from its top left).
pub(crate) enum Input<'a> {
    MouseDown { at: Point<f32>, button: Button, clicks: usize, modifiers: Modifiers },
    MouseUp { at: Point<f32>, button: Button, clicks: usize, modifiers: Modifiers },
    MouseMove { at: Point<f32>, modifiers: Modifiers },
    MouseLeave,
    Scroll { at: Point<f32>, delta: ScrollDelta, modifiers: Modifiers },
    KeyDown { keystroke: &'a Keystroke, repeat: bool },
    KeyUp { keystroke: &'a Keystroke },
}

/// One page in an engine. Main thread only.
pub(crate) trait Page {
    fn load(&mut self, url: &str);
    fn back(&mut self);
    fn forward(&mut self);
    fn reload(&mut self);
    fn stop(&mut self);
    /// A hidden page stops rendering (its timers slow down).
    fn set_hidden(&mut self, hidden: bool);
    fn set_focus(&mut self, focused: bool);
    /// Where the page is laid out (window coordinates) and the display's
    /// scale; called every frame it is shown.
    fn layout(&mut self, bounds: Bounds<Pixels>, scale: f32);
    fn input(&mut self, input: Input<'_>);
    fn zoom(&mut self, zoom: Zoom);
    /// The page's zoom in percent (100: none).
    fn zoom_percent(&self) -> u32;
    fn set_muted(&mut self, muted: bool);
    /// Pauses the page's playing media, or resumes what it paused.
    fn play_pause(&mut self);
    /// Answers the page's question: OK/Allow or Cancel/Deny, with the text
    /// typed for a prompt.
    fn answer(&mut self, accept: bool, input: Option<&str>);
    fn show_dev_tools(&mut self);
    fn close(&mut self);
    /// Draws the page at `bounds` (engines whose pages GPUI paints) or puts
    /// its native view there (engines that present pages themselves).
    fn paint(&mut self, bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window);
    /// See [`PageEvent::Pump`].
    fn pump(&mut self) {}
}

/// A page for `url` in this platform's engine.
pub(crate) fn new_page(url: &str, events: Events, window: &mut Window) -> Box<dyn Page> {
    Box::new(cef::CefPage::new(url, events, window))
}

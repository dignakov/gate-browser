//! Web engines behind one interface. [`crate::WebView`] is the same on every
//! platform; an engine renders its pages, takes commands and input through
//! [`Page`], and reports what pages do as [`PageEvent`]s on a channel (which
//! also works from an engine's own threads).

#[cfg(feature = "cef")]
pub(crate) mod cef;
#[cfg(windows)]
pub(crate) mod webview2;
#[cfg(gate_wpe)]
pub(crate) mod wpe;

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
    /// The page's own cursor (engines whose cursors GPUI's styles can't
    /// all name), with the closest style for platforms without them.
    #[cfg_attr(not(windows), allow(dead_code))]
    NativeCursor { cursor: gpui::NativeCursor, fallback: CursorStyle },
    /// The page copied text (engines that don't reach the clipboard
    /// themselves).
    #[cfg_attr(not(gate_wpe), allow(dead_code))]
    Clipboard(String),
    /// A select's options to choose from, or none (engines that don't draw
    /// these menus themselves); answered with [`Page::choose`].
    #[cfg_attr(not(gate_wpe), allow(dead_code))]
    Menu(Option<Menu>),
    /// New pixels to paint.
    Repaint,
    /// The engine wants a turn with the page on the main thread
    /// ([`Page::pump`]): work its callbacks couldn't do in place. (Only
    /// Chromium on Linux needs one so far.)
    #[cfg_attr(not(all(target_os = "linux", feature = "cef")), allow(dead_code))]
    Pump,
    /// One of Gate's shortcuts, pressed while the page held the keyboard
    /// (engines whose pages take Windows' keyboard focus).
    #[cfg_attr(not(windows), allow(dead_code))]
    Key(Keystroke),
    Closed,
}

pub(crate) type Events = async_channel::Sender<PageEvent>;

/// A select's menu: its options, below the select at `at` (page coordinates).
#[derive(Clone, Debug)]
#[cfg_attr(not(gate_wpe), allow(dead_code))]
pub(crate) struct Menu {
    pub at: Point<f32>,
    pub width: f32,
    pub items: Vec<MenuItem>,
}

#[derive(Clone, Debug)]
#[cfg_attr(not(gate_wpe), allow(dead_code))]
pub(crate) struct MenuItem {
    pub label: String,
    pub enabled: bool,
    pub selected: bool,
    /// A heading over the options after it.
    pub group: bool,
}

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
    KeyDown {
        keystroke: &'a Keystroke,
        #[cfg_attr(not(feature = "cef"), allow(dead_code))]
        repeat: bool,
    },
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
    /// The clipboard's text, before input that may paste it (engines that
    /// don't read the clipboard themselves).
    fn share_clipboard(&mut self, _text: Option<String>) {}
    /// Chooses an option of the page's [`Menu`], or none (it closes).
    fn choose(&mut self, _index: Option<usize>) {}
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

/// The engines Gate can run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    #[cfg(feature = "cef")]
    Cef,
    #[cfg(gate_wpe)]
    Wpe,
    #[cfg(windows)]
    WebView2,
}

/// This run's engine: Edge (WebView2) on Windows, WPE WebKit on Linux,
/// Chromium elsewhere. A build with Chromium as well (the `cef` feature)
/// runs it with `GATE_ENGINE=cef`.
pub(crate) fn engine() -> Engine {
    #[cfg(feature = "cef")]
    let chromium = || {
        static CHROMIUM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *CHROMIUM.get_or_init(|| std::env::var("GATE_ENGINE").as_deref() == Ok("cef"))
    };
    #[cfg(windows)]
    {
        #[cfg(feature = "cef")]
        if chromium() {
            return Engine::Cef;
        }
        Engine::WebView2
    }
    #[cfg(all(not(windows), gate_wpe))]
    {
        #[cfg(feature = "cef")]
        if chromium() {
            return Engine::Cef;
        }
        Engine::Wpe
    }
    #[cfg(all(not(windows), not(gate_wpe)))]
    {
        Engine::Cef
    }
}

/// A page for `url` in this run's engine.
pub(crate) fn new_page(url: &str, events: Events, window: &mut Window) -> Box<dyn Page> {
    match engine() {
        #[cfg(feature = "cef")]
        Engine::Cef => Box::new(cef::CefPage::new(url, events, window)),
        #[cfg(gate_wpe)]
        Engine::Wpe => Box::new(wpe::WpePage::new(url, events, window)),
        #[cfg(windows)]
        Engine::WebView2 => Box::new(webview2::WebView2Page::new(url, events, window)),
    }
}

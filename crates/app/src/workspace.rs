//! The window's root view: tabs (each a split tree of web panes) and the
//! sidebar that lists them, which collapses to a rail.

use crate::text_input::{InputEvent, TextInput};
use crate::palette::{Intent, Palette};
use crate::db::Db;
use crate::session::Session;
use crate::settings::Store;
use crate::{theme, url};
use gpui::{
    App, AppContext as _, Context, Corners, Entity, FocusHandle, Focusable, KeyBinding, Pixels, Point, Subscription, Window,
    actions, px,
};
use layout::{Edge, Node, PaneId};
use std::collections::HashMap;
use web::{Ask, WebEvent, WebView};

actions!(
    workspace,
    [
        NewTab,
        NewTile,
        RecentSpace,
        RecentSpaceBack,
        ClosePane,
        CloseTab,
        FocusAddress,
        Reload,
        Back,
        Forward,
        SplitRight,
        SplitDown,
        ToggleSidebar,
        NextTab,
        PrevTab,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        DevTools,
        ZoomPane,
        RenameSpace,
        OpenSettings,
        OpenPalette,
        PaletteUp,
        PaletteDown,
        PageZoomIn,
        PageZoomOut,
        PageZoomReset,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("ctrl-t", NewTab, ctx),
        KeyBinding::new("ctrl-w", ClosePane, ctx),
        KeyBinding::new("ctrl-shift-w", CloseTab, ctx),
        KeyBinding::new("ctrl-l", FocusAddress, ctx),
        KeyBinding::new("ctrl-r", Reload, ctx),
        KeyBinding::new("f5", Reload, ctx),
        KeyBinding::new("alt-left", Back, ctx),
        KeyBinding::new("alt-right", Forward, ctx),
        KeyBinding::new("ctrl-\\", SplitRight, ctx),
        KeyBinding::new("ctrl-shift-\\", SplitDown, ctx),
        KeyBinding::new("ctrl-alt-enter", NewTile, ctx),
        KeyBinding::new("ctrl-b", ToggleSidebar, ctx),
        KeyBinding::new("ctrl-tab", RecentSpace, ctx),
        KeyBinding::new("ctrl-shift-tab", RecentSpaceBack, ctx),
        KeyBinding::new("ctrl-pagedown", NextTab, ctx),
        KeyBinding::new("ctrl-pageup", PrevTab, ctx),
        KeyBinding::new("ctrl-alt-left", FocusLeft, ctx),
        KeyBinding::new("ctrl-alt-right", FocusRight, ctx),
        KeyBinding::new("ctrl-alt-up", FocusUp, ctx),
        KeyBinding::new("ctrl-alt-down", FocusDown, ctx),
        KeyBinding::new("f12", DevTools, ctx),
        KeyBinding::new("ctrl-shift-enter", ZoomPane, ctx),
        KeyBinding::new("f2", RenameSpace, ctx),
        KeyBinding::new("ctrl-,", OpenSettings, ctx),
        KeyBinding::new("ctrl-shift-p", OpenPalette, ctx),
        KeyBinding::new("ctrl-=", PageZoomIn, ctx),
        KeyBinding::new("ctrl-+", PageZoomIn, ctx),
        KeyBinding::new("ctrl--", PageZoomOut, ctx),
        KeyBinding::new("ctrl-0", PageZoomReset, ctx),
        KeyBinding::new("up", PaletteUp, Some("Palette")),
        KeyBinding::new("down", PaletteDown, Some("Palette")),
    ]);
}

pub type TabId = u64;

/// A sidebar entry: one or more pages split side by side.
pub struct Tab {
    pub id: TabId,
    /// Given by the user; otherwise the focused page's title shows.
    pub name: Option<String>,
    pub root: Node,
    pub focused: PaneId,
    /// Shown alone, filling the tab, while set.
    pub zoomed: Option<PaneId>,
    /// Opened in the background (a middle click) and not looked at yet.
    pub unvisited: bool,
}

pub struct Pane {
    pub view: Entity<WebView>,
    /// The answer field while the page asks for text (`window.prompt`).
    pub prompt: Option<(Entity<TextInput>, Subscription)>,
    _events: Subscription,
}

/// Dragging the gap between two panes.
pub struct DividerDrag {
    pub path: Vec<usize>,
    pub index: usize,
    pub axis: layout::Axis,
    pub span: f32,
    pub last: Point<Pixels>,
}

/// A pane held by its header: a drag once the pointer moved far enough,
/// otherwise just a click.
pub struct PaneDrag {
    pub pane: PaneId,
    pub origin: Point<Pixels>,
    pub moved: bool,
    pub target: Option<DropTarget>,
}

/// A space held by its sidebar entry: dropped beside a pane, it joins that
/// space as a split; not moved, it was a click.
pub struct SpaceDrag {
    pub tab: TabId,
    pub origin: Point<Pixels>,
    pub clicks: usize,
    pub moved: bool,
    /// The pane it would go beside, and on which side.
    pub target: Option<(PaneId, Edge)>,
}

/// Where a dragged pane would go.
#[derive(Clone, Copy, PartialEq)]
pub enum DropTarget {
    /// Beside another pane (an edge), or in its place (a swap).
    Pane(PaneId, Option<Edge>),
    /// Out of its tab, into a space of its own (dropped on the sidebar),
    /// at this place in the list.
    NewSpace(usize),
}

/// How far a held header must move to become a drag.
pub const DRAG_THRESHOLD: f32 = 4.0;

pub struct Sidebar {
    /// Narrowed to a rail of tiles.
    pub collapsed: bool,
    /// Its width when not collapsed.
    pub width: f32,
    /// The share of the lists' height Spaces gets (Activity the rest).
    pub split: f32,
    pub drag: Option<SidebarDrag>,
}

/// Dragging one of the sidebar's edges.
pub enum SidebarDrag {
    /// Its right edge; `offset` is the pointer's distance from it.
    Width { offset: f32 },
    /// The line between Spaces and Activity.
    Split { last: f32, lists: f32 },
}

/// The settings dialog, while open.
pub struct SettingsDialog {
    pub section: SettingsSection,
    pub focus: FocusHandle,
    pub look: crate::look::LookUi,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    Look,
    Tiling,
    AdBlocking,
}

impl SettingsSection {
    pub const ALL: [Self; 3] = [Self::Look, Self::Tiling, Self::AdBlocking];

    pub fn title(self) -> &'static str {
        match self {
            Self::Look => "Look",
            Self::Tiling => "Tiling",
            Self::AdBlocking => "Ad blocking",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Look => "icons/palette.svg",
            Self::Tiling => "icons/columns-2.svg",
            Self::AdBlocking => "icons/shield.svg",
        }
    }
}

/// Ctrl+Tab in progress: the spaces in the order it steps through them and
/// where it is; releasing Ctrl goes there.
pub struct Switching {
    pub order: Vec<TabId>,
    pub at: usize,
}

/// A space being renamed: its id and the field.
pub struct Renaming {
    pub tab: TabId,
    pub input: Entity<TextInput>,
    _events: Subscription,
}

pub struct Workspace {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub panes: HashMap<PaneId, Pane>,
    next_id: u64,
    pub address: Entity<TextInput>,
    pub address_focused: bool,
    pub sidebar: Sidebar,
    pub renaming: Option<Renaming>,
    pub divider_drag: Option<DividerDrag>,
    pub pane_drag: Option<PaneDrag>,
    pub space_drag: Option<SpaceDrag>,
    /// Where each space's sidebar entry (or rail tile) was last laid out, by
    /// its place in `tabs`: a pane dragged there lands between them.
    pub space_rows: std::rc::Rc<std::cell::RefCell<Vec<(usize, gpui::Bounds<Pixels>)>>>,
    /// Where the panes were last laid out (a new tile's size comes of it).
    pub pane_area: layout::Rect,
    /// Spaces by when they were last shown, the shown one first.
    pub recent: Vec<TabId>,
    /// Ctrl+Tab held: stepping through `recent` (see `step_recent`).
    pub switching: Option<Switching>,
    /// Downloads pages started, newest first (see [`crate::notices`]).
    pub downloads: Vec<crate::notices::DownloadItem>,
    /// Notifications pages showed, newest first.
    pub notes: Vec<crate::notices::NoteItem>,
    pub store: Store,
    pub settings: Option<SettingsDialog>,
    pub palette: Option<Palette>,
    pub db: Db,
    /// Site icons read from the database, by host (`None`: it has none).
    favicons: HashMap<String, Option<std::sync::Arc<gpui::RenderImage>>>,
    /// The session as last saved (to skip saving it unchanged).
    pub saved_session: Option<String>,
    /// The background's images, and the render under way.
    pub backdrop: Option<crate::backdrop::Rendered>,
    backdrop_pending: Option<crate::backdrop::Key>,
    /// The last render that failed (not retried until something changes).
    backdrop_failed: Option<crate::backdrop::Key>,
    /// Background pictures for Settings, by key (`None`: being made).
    pub thumbs: HashMap<String, Option<std::sync::Arc<gpui::RenderImage>>>,
    /// The sidebar's text colors, for what shows behind it.
    pub ink: theme::Ink,
    /// The Settings slider being dragged.
    pub slider_drag: Option<crate::look::Slider>,
    pub focus: FocusHandle,
    _address_events: Subscription,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Workspace {
    pub fn new(urls: Vec<String>, store: Store, db: Db, session: Session, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| {
            let mut input = TextInput::new("Search or enter address", window, cx);
            input.paste_and_go = true;
            input
        });
        let address_events = cx.subscribe_in(&address, window, Self::address_event);
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            next_id: 1,
            address,
            address_focused: false,
            sidebar: Sidebar { collapsed: false, width: theme::SIDEBAR_WIDTH, split: 0.5, drag: None },
            renaming: None,
            space_rows: Default::default(),
            pane_area: layout::Rect::new(0.0, 0.0, 1280.0, 800.0),
            recent: Vec::new(),
            switching: None,
            downloads: Vec::new(),
            notes: Vec::new(),
            divider_drag: None,
            pane_drag: None,
            space_drag: None,
            store,
            settings: None,
            palette: None,
            db,
            favicons: HashMap::new(),
            saved_session: None,
            backdrop: None,
            backdrop_pending: None,
            backdrop_failed: None,
            thumbs: HashMap::new(),
            ink: theme::Ink::standard(),
            slider_drag: None,
            focus: cx.focus_handle(),
            _address_events: address_events,
        };
        Self::on_system_notification(window, cx);
        // Hourly: is a uBlock Origin Lite check due (daily)?
        cx.spawn(async move |this, cx| {
            loop {
                let due = this
                    .read_with(cx, |ws, _| {
                        let store = &ws.store;
                        store.settings.ublock_auto_update && store.ublock_version.is_some() && store.ublock_paths.check_due()
                    })
                    .unwrap_or(false);
                if due && this.update(cx, |ws, cx| ws.check_ublock(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(std::time::Duration::from_secs(60 * 60)).await;
            }
        })
        .detach();
        // A moment after anything changes, the session is saved.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(2)).await;
                if this.update_in(cx, |ws, window, cx| ws.save_session(window, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // Before the first page: pages start in the theme.
        this.apply_theme(window, cx);
        cx.observe_window_appearance(window, |ws, window, cx| ws.apply_theme(window, cx)).detach();
        this.restore(session, window, cx);
        // Pages asked for on the command line open as new spaces; with
        // nothing open at all, the palette asks where to go.
        if !urls.is_empty() || this.tabs.is_empty() {
            this.open_urls(urls, window, cx);
        } else {
            this.activate(this.active, window, cx);
        }
        this
    }

    /// Light or dark, as Settings › Look › Theme and the system say: for
    /// Gate and for the pages.
    pub fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dark = self.store.settings.theme.dark(window.appearance());
        web::set_dark_mode(dark);
        if theme::set_dark(dark) {
            cx.notify();
        }
    }

    /// Keeps the background's images made for the window's size and the
    /// settings; a render runs off the main thread, the old images showing
    /// (stretched) meanwhile.
    pub fn sync_backdrop(&mut self, width: f32, height: f32, scale: f32, cx: &mut Context<Self>) {
        use crate::backdrop::{self, Background, Key};
        let settings = &self.store.settings;
        if matches!(settings.background, Background::Solid { .. }) {
            self.backdrop = None;
            return;
        }
        let wallpaper = matches!(settings.background, Background::Wallpaper { .. });
        let key = Key {
            background: settings.background.clone(),
            width: (width * scale).round() as u32,
            height: (height * scale).round() as u32,
            blur: if wallpaper { (settings.blur * scale).round() as u32 } else { 0 },
        };
        let current = self.backdrop.as_ref().is_some_and(|b| b.key == key);
        if current || self.backdrop_pending.is_some() || self.backdrop_failed.as_ref() == Some(&key) {
            return;
        }
        self.backdrop_pending = Some(key.clone());
        let job = cx.background_executor().spawn(async move { backdrop::render(&key) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |ws, cx| {
                let key = ws.backdrop_pending.take();
                match result {
                    Ok(rendered) => ws.backdrop = rendered,
                    Err(error) => {
                        tracing::warn!("background: {error:#}");
                        ws.backdrop_failed = key;
                    }
                }
                // Asks again: the window or the setting may have moved on.
                cx.notify();
            });
        })
        .detach();
    }

    /// New sidebar ink: the address field (which keeps its colors) follows.
    pub fn set_ink(&mut self, ink: theme::Ink, cx: &mut Context<Self>) {
        if self.ink == ink {
            return;
        }
        self.ink = ink;
        self.address.update(cx, |input, cx| {
            input.text_color = Some(ink.text.into());
            input.placeholder_color = Some(ink.dim.into());
            cx.notify();
        });
    }

    /// A host's icon from the database (read once, then remembered).
    pub fn cached_favicon(&mut self, host: &str) -> Option<std::sync::Arc<gpui::RenderImage>> {
        if let Some(icon) = self.favicons.get(host) {
            return icon.clone();
        }
        let icon = self.db.favicon(host).and_then(|png| crate::session::favicon_image(&png));
        self.favicons.insert(host.to_string(), icon.clone());
        icon
    }

    pub(crate) fn fresh_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn new_pane(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let view = cx.new(|cx| WebView::new(url, window, cx));
        self.add_pane(view, window, cx)
    }

    /// Takes a page in as a pane (not yet in any space).
    pub(crate) fn add_pane(&mut self, view: Entity<WebView>, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let id = self.fresh_id();
        // Pages fill their card below its header: round the bottom.
        // Inside the card's 1px border, so its curve is one smaller.
        let r = px(theme::PANE_RADIUS - 1.0);
        view.update(cx, |v, _| v.set_corners(Corners { top_left: px(0.), top_right: px(0.), bottom_left: r, bottom_right: r }));
        let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.web_event(id, event, window, cx)
        });
        self.panes.insert(id, Pane { view, prompt: None, _events: events });
        id
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Where a pane lives: its tab's index.
    pub fn tab_of(&self, pane: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| t.root.contains(pane))
    }

    /// Shows a pane wherever it is: its tab comes to the front, it takes focus.
    pub fn reveal_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tab_of(pane) else { return };
        self.tabs[index].focused = pane;
        self.activate(index, window, cx);
    }

    pub fn focused_view(&self) -> Option<&Entity<WebView>> {
        let tab = self.active_tab()?;
        self.panes.get(&tab.focused).map(|p| &p.view)
    }

    pub fn open_tab(&mut self, url: &str, foreground: bool, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.new_pane(url, window, cx);
        self.space_for(pane, foreground, window, cx);
    }

    /// A new space after the active one, holding `pane`.
    fn space_for(&mut self, pane: PaneId, foreground: bool, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.fresh_id();
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(at, Tab { id, name: None, root: Node::pane(pane), focused: pane, zoomed: None, unvisited: !foreground });
        if foreground {
            self.activate(at, window, cx);
        } else {
            self.panes[&pane].view.update(cx, |v, _| v.set_hidden(true));
        }
        cx.notify();
    }

    /// Opens pages from outside (startup, a later launch): each a new
    /// space; none, and the palette asks where to go.
    pub fn open_urls(&mut self, urls: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        if urls.is_empty() {
            return self.open_palette(Intent::NewSpace, window, cx);
        }
        for url in urls {
            self.open_tab(&url, true, window, cx);
        }
    }

    pub fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.active = index;
        self.tabs[index].unvisited = false;
        let id = self.tabs[index].id;
        self.recent.retain(|t| *t != id);
        self.recent.insert(0, id);
        self.recent.truncate(64);
        // Pages of other tabs stop rendering until shown again.
        for (i, tab) in self.tabs.iter().enumerate() {
            for pane in tab.root.panes() {
                if let Some(p) = self.panes.get(&pane) {
                    p.view.update(cx, |v, _| v.set_hidden(i != index));
                }
            }
        }
        self.focus_pane(self.tabs[index].focused, window, cx);
        self.sync_address(cx);
        cx.notify();
    }

    pub fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        if !tab.root.contains(pane) {
            return;
        }
        tab.focused = pane;
        if let Some(p) = self.panes.get(&pane) {
            let handle = p.view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        self.sync_address(cx);
        cx.notify();
    }

    /// Shows the focused page's address, unless someone is typing one.
    fn sync_address(&mut self, cx: &mut Context<Self>) {
        if self.address_focused {
            return;
        }
        let url = self.focused_view().map(|v| v.read(cx).url().to_string()).unwrap_or_default();
        let shown = url::display(&url);
        self.address.update(cx, |input, cx| input.set_text(shown, cx));
    }

    fn web_event(&mut self, pane: PaneId, event: &WebEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            // Beside the page (middle or Ctrl+click): a tile or a space, as
            // Settings has it, the other with Shift.
            WebEvent::OpenTab { url, background: true, alternate } if self.store.settings.tiling_spiral != *alternate => {
                self.open_tile(pane, url, window, cx)
            }
            WebEvent::OpenTab { url, background, .. } => self.open_tab(url, !background, window, cx),
            // Windows the page opened (and keeps a hold of): spaces of their own.
            WebEvent::Popup => {
                let Some(opener) = self.panes.get(&pane).map(|p| p.view.clone()) else { return };
                for popup in opener.update(cx, |view, _| view.take_popups()) {
                    let view = cx.new(|cx| WebView::popup(popup, window, cx));
                    let popup = self.add_pane(view, window, cx);
                    self.space_for(popup, true, window, cx);
                }
            }
            WebEvent::CloseRequested => self.close_pane(pane, window, cx),
            WebEvent::Download(download) => self.download_changed(pane, download, cx),
            WebEvent::Notification(note) => self.note_shown(pane, note, window, cx),
            WebEvent::NotificationClosed(id) => self.note_closed(pane, *id, cx),
            // A shortcut pressed while the page held the keyboard. Replayed
            // once this update is over: its action handlers update the
            // workspace, which is busy here.
            WebEvent::Key(keystroke) => {
                let keystroke = keystroke.clone();
                window.defer(cx, move |window, cx| {
                    let shown = keystroke.unparse();
                    let handled = window.dispatch_keystroke(keystroke, cx);
                    tracing::debug!("shortcut {shown} from a page: handled {handled}");
                });
            }
            // The page took focus (clicked, usually): it is the focused pane.
            WebEvent::Focused => {
                if let Some(tab) = self.tabs.get_mut(self.active)
                    && tab.root.contains(pane)
                    && tab.focused != pane
                {
                    tab.focused = pane;
                    self.sync_address(cx);
                }
                cx.notify();
            }
            WebEvent::Ask => {
                self.sync_prompt(pane, window, cx);
                cx.notify();
            }
            WebEvent::Url(url) => {
                let title = self.panes.get(&pane).map(|p| p.view.read(cx).title().to_string()).unwrap_or_default();
                self.db.visit(url, &title);
                if self.active_tab().is_some_and(|t| t.focused == pane) {
                    self.sync_address(cx);
                }
                cx.notify();
            }
            WebEvent::Title(title) => {
                if let Some(p) = self.panes.get(&pane) {
                    self.db.set_title(p.view.read(cx).url(), title);
                }
                cx.notify();
            }
            // Kept so restored spaces show their icons before loading.
            WebEvent::Favicon => {
                if let Some(p) = self.panes.get(&pane) {
                    let view = p.view.read(cx);
                    if let Some(icon) = view.favicon() {
                        let host = url::host(view.url()).to_string();
                        if let Some(png) = crate::session::favicon_png(&icon) {
                            self.db.set_favicon(&host, &png);
                        }
                        self.favicons.insert(host, Some(icon));
                    }
                }
                cx.notify();
            }
            _ => cx.notify(),
        }
    }

    /// Gives a pane an answer field while its page asks for text.
    fn sync_prompt(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let default = match p.view.read(cx).ask() {
            Some(Ask::Prompt { default, .. }) => Some(default.clone()),
            _ => None,
        };
        let Some(default) = default else {
            p.prompt = None;
            return;
        };
        let input = cx.new(|cx| {
            let mut input = TextInput::new("", window, cx);
            input.set_text(default, cx);
            input
        });
        let events = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit(text) => this.answer(pane, true, Some(text.clone()), window, cx),
            InputEvent::Cancel => this.answer(pane, false, None, window, cx),
            _ => {}
        });
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        input.update(cx, |input, cx| input.select_all_text(cx));
        p.prompt = Some((input, events));
    }

    /// Answers the question a pane's page asks; the page gets the keyboard
    /// back (the answer field, which may have had it, goes away).
    pub fn answer(&mut self, pane: PaneId, accept: bool, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.panes.get_mut(&pane) else { return };
        let text = text.or_else(|| p.prompt.as_ref().map(|(input, _)| input.read(cx).text().to_string()));
        p.prompt = None;
        let view = p.view.clone();
        view.update(cx, |view, cx| view.answer(accept, text.as_deref(), cx));
        self.focus_pane(pane, window, cx);
    }

    fn address_event(&mut self, _: &Entity<TextInput>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Submit(text) => {
                let target = url::resolve(text);
                match self.focused_view().cloned() {
                    Some(view) => {
                        view.update(cx, |v, _| v.load(&target));
                        let handle = view.read(cx).focus_handle(cx);
                        window.focus(&handle, cx);
                    }
                    // No page to load it in: it gets a space of its own.
                    None => {
                        self.palette = None;
                        self.open_tab(&target, true, window, cx);
                    }
                }
            }
            InputEvent::Changed => {}
            InputEvent::Cancel => {
                self.address_focused = false;
                self.sync_address(cx);
                if let Some(view) = self.focused_view().cloned() {
                    let handle = view.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
            }
            InputEvent::Focused => {
                self.address_focused = true;
                cx.notify();
            }
            InputEvent::Blurred => {
                self.address_focused = false;
                self.sync_address(cx);
                cx.notify();
            }
        }
    }

    // ---- splits

    pub fn split(&mut self, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.active_tab().map(|t| t.focused) {
            self.split_pane(target, edge, window, cx);
        }
    }

    /// Asks (in the palette) what goes beside `target`; the split happens
    /// once something is chosen.
    pub fn split_pane(&mut self, target: PaneId, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_of(target).is_some() {
            self.open_palette(Intent::Split { target, edge }, window, cx);
        }
    }

    /// Where a new tile goes in the space at `index` (see
    /// [`layout::next_tile`]); none when it would be too small.
    pub fn next_tile(&self, index: usize, focused: PaneId) -> Option<(PaneId, Edge)> {
        let tab = self.tabs.get(index)?;
        layout::next_tile(&tab.root, self.pane_area, theme::GAP, focused, self.store.settings.tiling_spiral, (theme::TILE_MIN_WIDTH, theme::TILE_MIN_HEIGHT))
    }

    /// Opens `url` as the next tile of `opener`'s space, leaving focus where
    /// it is; as a space behind when no tile fits.
    pub fn open_tile(&mut self, opener: PaneId, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tab_of(opener) else { return self.open_tab(url, false, window, cx) };
        let focused = self.tabs[index].focused;
        let Some((target, edge)) = self.next_tile(index, focused) else { return self.open_tab(url, false, window, cx) };
        let pane = self.new_pane(url, window, cx);
        let tab = &mut self.tabs[index];
        tab.zoomed = None;
        tab.root.split(target, edge, pane);
        cx.notify();
    }

    /// Opens `address` in a new pane beside `target`.
    pub fn split_with(&mut self, target: PaneId, edge: Edge, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tab_of(target) else { return };
        let pane = self.new_pane(address, window, cx);
        let tab = &mut self.tabs[index];
        tab.zoomed = None;
        tab.root.split(target, edge, pane);
        tab.focused = pane;
        self.activate(index, window, cx);
    }

    /// Moves an open page from its space to beside `target` (a space it
    /// leaves empty goes away).
    pub fn move_beside(&mut self, pane: PaneId, target: PaneId, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(from), Some(_)) = (self.tab_of(pane), self.tab_of(target)) else { return };
        if !self.tabs[from].root.remove(pane) {
            // Its space's only page: the space goes, the page stays.
            self.tabs.remove(from);
            if from < self.active {
                self.active -= 1;
            }
        } else {
            let tab = &mut self.tabs[from];
            if tab.zoomed == Some(pane) {
                tab.zoomed = None;
            }
            if tab.focused == pane {
                tab.focused = tab.root.panes()[0];
            }
        }
        let Some(index) = self.tab_of(target) else { return };
        let tab = &mut self.tabs[index];
        tab.zoomed = None;
        tab.root.split(target, edge, pane);
        tab.focused = pane;
        self.activate(index, window, cx);
    }

    pub fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.tabs.iter().position(|t| t.root.contains(pane)) else { return };
        if !self.tabs[t].root.remove(pane) {
            // The tab's last pane: the tab goes with it.
            return self.close_tab(t, window, cx);
        }
        if let Some(p) = self.panes.remove(&pane) {
            p.view.update(cx, |v, _| v.close());
        }
        self.forget_page_notices(pane, cx);
        let tab = &mut self.tabs[t];
        if tab.zoomed == Some(pane) {
            tab.zoomed = None;
        }
        if tab.focused == pane {
            tab.focused = tab.root.panes()[0];
        }
        if t == self.active {
            let focused = self.tabs[t].focused;
            self.focus_pane(focused, window, cx);
        }
        cx.notify();
    }

    pub fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        for pane in tab.root.panes() {
            if let Some(p) = self.panes.remove(&pane) {
                p.view.update(cx, |v, _| v.close());
            }
            self.forget_page_notices(pane, cx);
        }
        if self.tabs.is_empty() {
            // The window empties; the palette asks where to go next.
            self.active = 0;
            self.sync_address(cx);
            return self.open_palette(Intent::NewSpace, window, cx);
        }
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.activate(self.active, window, cx);
    }

    pub fn move_pane(&mut self, pane: PaneId, target: PaneId, edge: Option<Edge>, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let moved = match edge {
            Some(edge) => tab.root.move_to(pane, target, edge),
            None => tab.root.swap(pane, target),
        };
        if moved {
            tab.zoomed = None;
            cx.notify();
        }
    }

    /// Takes `pane` out of its space into a new one at `at` in the list,
    /// put away there: the space it left stays shown.
    pub fn pane_to_new_space(&mut self, pane: PaneId, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.tab_of(pane) else { return };
        // A tab's last pane already is a space of its own.
        if !self.tabs[index].root.remove(pane) {
            return;
        }
        let tab = &mut self.tabs[index];
        if tab.zoomed == Some(pane) {
            tab.zoomed = None;
        }
        if tab.focused == pane {
            tab.focused = tab.root.panes()[0];
        }
        let id = self.fresh_id();
        let at = at.min(self.tabs.len());
        self.tabs.insert(at, Tab { id, name: None, root: Node::pane(pane), focused: pane, zoomed: None, unvisited: false });
        if at <= self.active {
            self.active += 1;
        }
        self.panes[&pane].view.update(cx, |v, _| v.set_hidden(true));
        let focused = self.tabs[self.active].focused;
        self.focus_pane(focused, window, cx);
        cx.notify();
    }

    /// Moves a whole space beside `target`, a pane of another space, in the
    /// half a new pane would get there: its splits keep their shares of
    /// that, and its sidebar entry goes.
    pub fn space_beside(&mut self, tab: TabId, target: PaneId, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.id == tab) else { return };
        if self.tab_of(target).is_none_or(|t| t == from) {
            return;
        }
        let moved = self.tabs.remove(from);
        if from < self.active {
            self.active -= 1;
        }
        let Some(index) = self.tab_of(target) else { return };
        let into = &mut self.tabs[index];
        into.zoomed = None;
        into.root.insert(target, edge, moved.root);
        into.focused = moved.focused;
        self.activate(index, window, cx);
    }

    pub fn play_pause(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if let Some(p) = self.panes.get(&pane) {
            p.view.update(cx, |v, _| v.play_pause());
        }
    }

    pub fn toggle_mute(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if let Some(p) = self.panes.get(&pane) {
            p.view.update(cx, |v, cx| {
                let muted = !v.muted();
                v.set_muted(muted, cx)
            });
        }
    }

    fn focus_direction(&mut self, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else { return };
        let area = layout::Rect::new(0.0, 0.0, 1000.0, 1000.0);
        if let Some(next) = tab.root.neighbor(tab.focused, edge, area) {
            self.focus_pane(next, window, cx);
        }
    }

    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The address lives in the full sidebar.
        self.sidebar.collapsed = false;
        let handle = self.address.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        self.address.update(cx, |input, cx| input.select_all_text(cx));
        cx.notify();
    }

    // ---- sidebar

    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar.collapsed = !self.sidebar.collapsed;
        cx.notify();
    }

    /// Follows the dragged edge; narrow enough, it collapses to the rail
    /// (and widens back out of it), as herdr's does.
    pub fn drag_sidebar_edge(&mut self, edge: f32, cx: &mut Context<Self>) {
        self.sidebar.collapsed = edge < theme::SIDEBAR_COLLAPSE;
        if !self.sidebar.collapsed {
            self.sidebar.width = edge.clamp(theme::SIDEBAR_MIN, theme::SIDEBAR_MAX);
        }
        cx.notify();
    }

    /// Spaces' share of `lists`, the height Spaces and Activity divide.
    pub fn spaces_height(&self, lists: f32) -> f32 {
        (self.sidebar.split * lists).clamp(theme::LIST_MIN, (lists - theme::LIST_MIN).max(theme::LIST_MIN))
    }

    /// Moves the line between Spaces and Activity by `delta` pixels.
    pub fn drag_sidebar_split(&mut self, delta: f32, lists: f32, cx: &mut Context<Self>) {
        if lists <= 2.0 * theme::LIST_MIN {
            return;
        }
        let spaces = (self.sidebar.split * lists + delta).clamp(theme::LIST_MIN, lists - theme::LIST_MIN);
        self.sidebar.split = spaces / lists;
        cx.notify();
    }

    // ---- renaming spaces

    /// What a space is called: its given name, else its focused page's title.
    pub fn space_name(&self, tab: &Tab, cx: &App) -> String {
        if let Some(name) = &tab.name {
            return name.clone();
        }
        self.panes.get(&tab.focused).map(|p| crate::render::page_title(p.view.read(cx))).unwrap_or_default()
    }

    pub fn start_rename(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(index) else { return };
        self.sidebar.collapsed = false;
        let (id, current) = (tab.id, self.space_name(tab, cx));
        let input = cx.new(|cx| {
            let mut input = TextInput::new("Name this space", window, cx);
            input.set_text(current, cx);
            input
        });
        let events = cx.subscribe_in(&input, window, move |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit(text) => this.finish_rename(Some(text.clone()), window, cx),
            InputEvent::Cancel => this.finish_rename(None, window, cx),
            // Clicking away keeps what was typed.
            InputEvent::Blurred => {
                let text = this.renaming.as_ref().map(|r| r.input.read(cx).text().to_string());
                this.finish_rename(text, window, cx)
            }
            InputEvent::Focused | InputEvent::Changed => {}
        });
        let handle = input.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        input.update(cx, |input, cx| input.select_all_text(cx));
        self.renaming = Some(Renaming { tab: id, input, _events: events });
        cx.notify();
    }

    /// Ends renaming: `Some` text names the space (empty text: back to its
    /// page's title), `None` leaves it as it was.
    fn finish_rename(&mut self, text: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else { return };
        if let Some(text) = text
            && let Some(tab) = self.tabs.iter_mut().find(|t| t.id == renaming.tab)
        {
            let text = text.trim();
            tab.name = (!text.is_empty()).then(|| text.to_string());
        }
        // The field had the keyboard; the page gets it back.
        if let Some(pane) = self.active_tab().map(|t| t.focused) {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    /// The gap around the sidebar when it floats as a card.
    pub fn sidebar_inset(&self) -> f32 {
        if self.store.settings.floating_sidebar { theme::GAP } else { 0.0 }
    }

    pub fn sidebar_width(&self) -> f32 {
        if self.sidebar.collapsed { theme::RAIL_WIDTH } else { self.sidebar.width }
    }

    // ---- actions

    /// Asks where to go; the space is made once something is chosen.
    pub fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(Intent::NewSpace, window, cx);
    }

    pub fn on_open_palette(&mut self, _: &OpenPalette, window: &mut Window, cx: &mut Context<Self>) {
        // A second press closes it, as herdr's does.
        if self.palette.is_some() && !self.tabs.is_empty() {
            self.close_palette(window, cx);
        } else {
            self.open_palette(Intent::Anything, window, cx);
        }
    }
    pub fn on_close_pane(&mut self, _: &ClosePane, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.active_tab().map(|t| t.focused) {
            self.close_pane(pane, window, cx);
        }
    }
    pub fn on_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(self.active, window, cx);
    }
    pub fn on_focus_address(&mut self, _: &FocusAddress, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_address(window, cx);
    }
    pub fn on_reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.focused_view().cloned() {
            v.update(cx, |v, _| v.reload());
        }
    }
    pub fn on_back(&mut self, _: &Back, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.focused_view().cloned() {
            v.update(cx, |v, _| v.back());
        }
    }
    pub fn on_forward(&mut self, _: &Forward, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.focused_view().cloned() {
            v.update(cx, |v, _| v.forward());
        }
    }
    pub fn on_new_tile(&mut self, _: &NewTile, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else { return self.open_palette(Intent::NewSpace, window, cx) };
        match self.next_tile(self.active, tab.focused) {
            Some((target, edge)) => self.split_pane(target, edge, window, cx),
            None => self.open_palette(Intent::NewSpace, window, cx),
        }
    }
    pub fn on_split_right(&mut self, _: &SplitRight, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Edge::Right, window, cx);
    }
    pub fn on_split_down(&mut self, _: &SplitDown, window: &mut Window, cx: &mut Context<Self>) {
        self.split(Edge::Bottom, window, cx);
    }
    pub fn on_toggle_sidebar(&mut self, _: &ToggleSidebar, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_sidebar(cx);
    }
    pub fn on_recent_space(&mut self, _: &RecentSpace, window: &mut Window, cx: &mut Context<Self>) {
        self.step_recent(1, window, cx);
    }
    pub fn on_recent_space_back(&mut self, _: &RecentSpaceBack, window: &mut Window, cx: &mut Context<Self>) {
        self.step_recent(-1, window, cx);
    }

    /// Ctrl+Tab: steps through the spaces by when they were last shown
    /// (the one before this first), marking where it is in the sidebar;
    /// releasing Ctrl goes there. A tap goes back to the last space.
    fn step_recent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let started = self.switching.is_none();
        let switching = self.switching.get_or_insert_with(|| {
            let exists = |id: &TabId| self.tabs.iter().any(|t| t.id == *id);
            let mut order: Vec<TabId> = self.recent.iter().copied().filter(exists).collect();
            if let Some(shown) = self.tabs.get(self.active).map(|t| t.id) {
                order.retain(|t| *t != shown);
                order.insert(0, shown);
            }
            // Spaces never shown yet come last, in sidebar order.
            order.extend(self.tabs.iter().map(|t| t.id).filter(|id| !order.contains(id)).collect::<Vec<_>>());
            Switching { order, at: 0 }
        });
        if switching.order.len() < 2 {
            self.switching = None;
            return;
        }
        let len = switching.order.len() as isize;
        switching.at = (switching.at as isize + step).rem_euclid(len) as usize;
        cx.notify();
        if started {
            // Ctrl's release, wherever the keys go (see `web::control_held`).
            cx.spawn_in(window, async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
                    let held = this.update_in(cx, |ws, window, cx| {
                        let held = web::control_held().unwrap_or(window.modifiers().control);
                        if !held {
                            ws.finish_switch(window, cx);
                        }
                        held && ws.switching.is_some()
                    });
                    if !held.unwrap_or(false) {
                        break;
                    }
                }
            })
            .detach();
        }
    }

    /// Goes to the space Ctrl+Tab stopped at.
    fn finish_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(switching) = self.switching.take() else { return };
        let target = switching.order[switching.at];
        match self.tabs.iter().position(|t| t.id == target) {
            Some(index) => self.activate(index, window, cx),
            None => cx.notify(),
        }
    }

    pub fn on_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate((self.active + 1) % self.tabs.len(), window, cx);
        }
    }
    pub fn on_prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.activate((self.active + self.tabs.len() - 1) % self.tabs.len(), window, cx);
        }
    }
    pub fn on_focus_left(&mut self, _: &FocusLeft, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Edge::Left, window, cx);
    }
    pub fn on_focus_right(&mut self, _: &FocusRight, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Edge::Right, window, cx);
    }
    pub fn on_focus_up(&mut self, _: &FocusUp, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Edge::Top, window, cx);
    }
    pub fn on_focus_down(&mut self, _: &FocusDown, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_direction(Edge::Bottom, window, cx);
    }
    pub fn on_dev_tools(&mut self, _: &DevTools, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(v) = self.focused_view().cloned() {
            v.update(cx, |v, _| v.show_dev_tools());
        }
    }
    pub fn on_rename_space(&mut self, _: &RenameSpace, window: &mut Window, cx: &mut Context<Self>) {
        self.start_rename(self.active, window, cx);
    }
    pub fn on_page_zoom_in(&mut self, _: &PageZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_page(None, web::Zoom::In, cx);
    }
    pub fn on_page_zoom_out(&mut self, _: &PageZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_page(None, web::Zoom::Out, cx);
    }
    pub fn on_page_zoom_reset(&mut self, _: &PageZoomReset, _: &mut Window, cx: &mut Context<Self>) {
        self.zoom_page(None, web::Zoom::Reset, cx);
    }

    /// Zooms a pane's page (`None`: the focused one).
    pub fn zoom_page(&mut self, pane: Option<PaneId>, zoom: web::Zoom, cx: &mut Context<Self>) {
        let Some(pane) = pane.or_else(|| self.active_tab().map(|t| t.focused)) else { return };
        if let Some(p) = self.panes.get(&pane) {
            p.view.update(cx, |v, _| v.zoom(zoom));
        }
        cx.notify();
    }

    pub fn on_zoom_pane(&mut self, _: &ZoomPane, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.active_tab().map(|t| t.focused) {
            self.toggle_zoom(pane, cx);
        }
    }

    /// Looks for a newer uBlock Origin Lite and downloads it (off the main
    /// thread); the result shows in Settings and, if ready, in Activity.
    pub fn check_ublock(&mut self, cx: &mut Context<Self>) {
        let store = &mut self.store;
        let Some(running) = store.ublock_version.clone() else { return };
        if store.ublock_update.checking {
            return;
        }
        store.ublock_update.checking = true;
        store.ublock_update.error = None;
        let paths = store.ublock_paths.clone();
        let check = cx.background_executor().spawn(async move { crate::ublock::check(&paths, &running) });
        cx.spawn(async move |this, cx| {
            let result = check.await;
            let _ = this.update(cx, |ws, cx| {
                let update = &mut ws.store.ublock_update;
                update.checking = false;
                match result {
                    Ok(crate::ublock::Checked::Current) => {}
                    Ok(crate::ublock::Checked::Ready(version)) => update.ready = Some(version),
                    Err(error) => {
                        tracing::warn!("checking for uBlock Origin Lite updates: {error:#}");
                        update.error = Some(format!("{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub fn on_open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        // A second press closes it, as herdr's does.
        if self.settings.is_some() {
            self.close_settings(window, cx);
        } else {
            self.open_settings(SettingsSection::Look, window, cx);
        }
    }

    pub fn open_settings(&mut self, section: SettingsSection, window: &mut Window, cx: &mut Context<Self>) {
        let focus = cx.focus_handle();
        // Keys go to the dialog (Esc), not the page behind it.
        window.focus(&focus, cx);
        let look = self.look_ui(window, cx);
        self.settings = Some(SettingsDialog { section, focus, look });
        cx.notify();
    }

    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_none() {
            return;
        }
        if let Some(pane) = self.active_tab().map(|t| t.focused) {
            self.focus_pane(pane, window, cx);
        }
        cx.notify();
    }

    pub fn toggle_zoom(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        let Some(index) = self.tab_of(pane) else { return };
        let tab = &mut self.tabs[index];
        tab.zoomed = if tab.zoomed == Some(pane) { None } else { Some(pane) };
        cx.notify();
    }
}

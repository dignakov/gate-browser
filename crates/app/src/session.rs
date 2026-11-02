//! Spaces survive restarts: what is open (every space, its splits, names,
//! the sidebar, the window) is saved in the database a moment after it
//! changes and restored at startup. Restored pages sleep until opened.

use crate::db::Db;
use crate::url;
use crate::workspace::{Tab, Workspace};
use gpui::{AppContext as _, Bounds, Context, Pixels, RenderImage, Window, WindowBounds, point, px, size};
use layout::{Node, PaneId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use web::WebView;

const KEY: &str = "session";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub spaces: Vec<SavedSpace>,
    pub active: usize,
    pub sidebar: SavedSidebar,
    /// x, y, width, height of the window while not maximized.
    pub window: Option<[f32; 4]>,
    pub maximized: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedSpace {
    pub name: Option<String>,
    pub root: Node,
    pub focused: PaneId,
    pub zoomed: Option<PaneId>,
    pub pages: Vec<SavedPage>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedPage {
    pub id: PaneId,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedSidebar {
    pub collapsed: bool,
    pub width: f32,
    pub split: f32,
}

impl Default for SavedSidebar {
    fn default() -> Self {
        Self { collapsed: false, width: crate::theme::SIDEBAR_WIDTH, split: 0.5 }
    }
}

impl Session {
    pub fn load(db: &Db) -> Self {
        let Some(text) = db.get_state(KEY) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!("the saved session doesn't read ({error}); starting fresh");
            Self::default()
        })
    }

    /// Where the window was, for opening it there again.
    pub fn window_bounds(&self) -> Option<WindowBounds> {
        let [x, y, w, h] = self.window?;
        let bounds = Bounds::new(point(px(x), px(y)), size(px(w.max(640.0)), px(h.max(420.0))));
        Some(if self.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) })
    }
}

/// A site icon as PNG, for the database.
pub fn favicon_png(image: &RenderImage) -> Option<Vec<u8>> {
    let size = image.size(0);
    // GPUI keeps pixels as BGRA.
    let mut rgba = image.as_bytes(0)?.to_vec();
    rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    let buffer = image::RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, rgba)?;
    let mut png = Vec::new();
    buffer.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).ok()?;
    Some(png)
}

pub fn favicon_image(png: &[u8]) -> Option<Arc<RenderImage>> {
    let mut rgba = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.into_rgba8();
    rgba.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    Some(Arc::new(RenderImage::new([image::Frame::new(rgba)])))
}

impl Workspace {
    /// What is open now, as saved.
    pub fn snapshot(&self, window: &Window, cx: &gpui::App) -> Session {
        let spaces = self
            .tabs
            .iter()
            .map(|tab| SavedSpace {
                name: tab.name.clone(),
                root: tab.root.clone(),
                focused: tab.focused,
                zoomed: tab.zoomed,
                pages: tab
                    .root
                    .panes()
                    .into_iter()
                    .filter_map(|id| {
                        let view = self.panes.get(&id)?.view.read(cx);
                        Some(SavedPage { id, url: view.url().to_string(), title: view.title().to_string() })
                    })
                    .collect(),
            })
            .collect();
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(b) => (b, false),
            WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
        };
        let f = |p: Pixels| f32::from(p);
        Session {
            spaces,
            active: self.active,
            sidebar: SavedSidebar { collapsed: self.sidebar.collapsed, width: self.sidebar.width, split: self.sidebar.split },
            window: Some([f(bounds.origin.x), f(bounds.origin.y), f(bounds.size.width), f(bounds.size.height)]),
            maximized,
        }
    }

    /// Saves the session if it changed since the last save.
    pub fn save_session(&mut self, window: &Window, cx: &gpui::App) {
        let text = match serde_json::to_string(&self.snapshot(window, cx)) {
            Ok(text) => text,
            Err(error) => return tracing::warn!("saving the session: {error}"),
        };
        if self.saved_session.as_deref() != Some(text.as_str()) {
            self.db.set_state(KEY, &text);
            self.saved_session = Some(text);
        }
    }

    /// Brings back saved spaces, every page asleep until opened.
    pub fn restore(&mut self, session: Session, window: &mut Window, cx: &mut Context<Self>) {
        let s = session.sidebar;
        self.sidebar.collapsed = s.collapsed;
        self.sidebar.width = s.width.clamp(crate::theme::SIDEBAR_MIN, crate::theme::SIDEBAR_MAX);
        self.sidebar.split = s.split.clamp(0.1, 0.9);
        for space in session.spaces {
            let pages: HashMap<PaneId, SavedPage> = space.pages.into_iter().map(|p| (p.id, p)).collect();
            let mut renamed: HashMap<PaneId, PaneId> = HashMap::new();
            let root = space.root.map_panes(&mut |old| {
                let page = pages.get(&old)?;
                let favicon = self.cached_favicon(url::host(&page.url));
                let view = cx.new(|cx| WebView::sleeping(&page.url, &page.title, favicon, window, cx));
                let id = self.add_pane(view, window, cx);
                renamed.insert(old, id);
                Some(id)
            });
            let Some(root) = root else { continue };
            let first = root.panes()[0];
            let focused = renamed.get(&space.focused).copied().unwrap_or(first);
            let zoomed = space.zoomed.and_then(|z| renamed.get(&z).copied());
            let id = self.fresh_id();
            self.tabs.push(Tab { id, name: space.name, root, focused, zoomed });
        }
        self.active = session.active.min(self.tabs.len().saturating_sub(1));
    }
}

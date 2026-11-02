//! The palette (herdr's Ctrl+Shift+P): one search box for pages already
//! open, the web, and commands. New spaces and splits come from it too:
//! nothing is created until it has somewhere to go, and Esc creates nothing.
//!
//! Search first: what is already open ranks above opening it again, so
//! typing "slack" goes to the Slack you have rather than a second one.

use crate::chrome::icon_glyph;
use crate::render::{page_title, site_tile};
use crate::text_input::{InputEvent, TextInput};
use crate::workspace::{self as ws, Workspace};
use crate::{theme, url};
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, prelude::FluentBuilder as _, px,
};
use layout::{Edge, PaneId, Rect};
use gpui::RenderImage;
use std::collections::HashSet;
use std::sync::Arc;

/// What the palette was opened for.
#[derive(Clone, Copy, PartialEq)]
pub enum Intent {
    /// Ctrl+Shift+P: anything.
    Anything,
    /// A new space: opens what is chosen in one (or goes to it if open).
    NewSpace,
    /// A new pane beside `target`: opens what is chosen there, or moves an
    /// open page there.
    Split { target: PaneId, edge: Edge },
}

impl Intent {
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Anything => None,
            Self::NewSpace => Some("New space"),
            Self::Split { edge: Edge::Bottom | Edge::Top, .. } => Some("Split down"),
            Self::Split { .. } => Some("Split right"),
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Anything => "Search open pages, the web, or commands",
            Self::NewSpace | Self::Split { .. } => "Search open pages or the web, or enter an address",
        }
    }
}

#[derive(Clone)]
pub enum Choice {
    /// A page that is already open.
    Pane(PaneId),
    /// A page from history (not open now).
    History { url: String, title: String, icon: Option<Arc<RenderImage>> },
    /// An address to load.
    Open(String),
    /// Words to search the web for.
    Search(String),
    Command(usize),
}

pub struct Palette {
    pub input: Entity<TextInput>,
    pub intent: Intent,
    pub choices: Vec<Choice>,
    pub selected: usize,
    pub scroll: ScrollHandle,
    _events: Subscription,
}

/// Something to do, by name.
struct Command {
    title: &'static str,
    /// More words it answers to.
    keywords: &'static str,
    key: Option<&'static str>,
    /// Only makes sense with a page open.
    needs_page: bool,
    run: fn(&mut Workspace, &mut Window, &mut Context<Workspace>),
}

const COMMANDS: &[Command] = &[
    Command { title: "New space", keywords: "tab open page", key: Some("Ctrl+T"), needs_page: false, run: |w, win, cx| w.on_new_tab(&ws::NewTab, win, cx) },
    Command { title: "Split right", keywords: "pane vertical beside", key: Some("Ctrl+\\"), needs_page: true, run: |w, win, cx| w.on_split_right(&ws::SplitRight, win, cx) },
    Command { title: "Split down", keywords: "pane horizontal below", key: Some("Ctrl+Shift+\\"), needs_page: true, run: |w, win, cx| w.on_split_down(&ws::SplitDown, win, cx) },
    Command { title: "Zoom pane", keywords: "maximize fill", key: Some("Ctrl+Shift+Enter"), needs_page: true, run: |w, win, cx| w.on_zoom_pane(&ws::ZoomPane, win, cx) },
    Command { title: "Close pane", keywords: "page kill", key: Some("Ctrl+W"), needs_page: true, run: |w, win, cx| w.on_close_pane(&ws::ClosePane, win, cx) },
    Command { title: "Close space", keywords: "tab kill", key: Some("Ctrl+Shift+W"), needs_page: true, run: |w, win, cx| w.on_close_tab(&ws::CloseTab, win, cx) },
    Command { title: "Rename space", keywords: "tab name", key: Some("F2"), needs_page: true, run: |w, win, cx| w.on_rename_space(&ws::RenameSpace, win, cx) },
    Command { title: "Next space", keywords: "tab", key: Some("Ctrl+Tab"), needs_page: true, run: |w, win, cx| w.on_next_tab(&ws::NextTab, win, cx) },
    Command { title: "Previous space", keywords: "tab", key: Some("Ctrl+Shift+Tab"), needs_page: true, run: |w, win, cx| w.on_prev_tab(&ws::PrevTab, win, cx) },
    Command { title: "Reload page", keywords: "refresh", key: Some("Ctrl+R"), needs_page: true, run: |w, win, cx| w.on_reload(&ws::Reload, win, cx) },
    Command { title: "Back", keywords: "history previous page", key: Some("Alt+Left"), needs_page: true, run: |w, win, cx| w.on_back(&ws::Back, win, cx) },
    Command { title: "Forward", keywords: "history next page", key: Some("Alt+Right"), needs_page: true, run: |w, win, cx| w.on_forward(&ws::Forward, win, cx) },
    Command { title: "Zoom in", keywords: "page bigger larger text size", key: Some("Ctrl+="), needs_page: true, run: |w, win, cx| w.on_page_zoom_in(&ws::PageZoomIn, win, cx) },
    Command { title: "Zoom out", keywords: "page smaller text size fit", key: Some("Ctrl+-"), needs_page: true, run: |w, win, cx| w.on_page_zoom_out(&ws::PageZoomOut, win, cx) },
    Command { title: "Actual size", keywords: "page zoom reset 100", key: Some("Ctrl+0"), needs_page: true, run: |w, win, cx| w.on_page_zoom_reset(&ws::PageZoomReset, win, cx) },
    Command { title: "Edit address", keywords: "url location", key: Some("Ctrl+L"), needs_page: true, run: |w, win, cx| w.on_focus_address(&ws::FocusAddress, win, cx) },
    Command { title: "Toggle sidebar", keywords: "collapse rail hide show", key: Some("Ctrl+B"), needs_page: false, run: |w, win, cx| w.on_toggle_sidebar(&ws::ToggleSidebar, win, cx) },
    Command { title: "Developer tools", keywords: "devtools inspect console", key: Some("F12"), needs_page: true, run: |w, win, cx| w.on_dev_tools(&ws::DevTools, win, cx) },
    Command { title: "Restart browser", keywords: "relaunch reload update apply quit", key: None, needs_page: false, run: |_, _, cx| crate::restart(cx) },
    Command { title: "Settings", keywords: "preferences options ad blocking ublock", key: Some("Ctrl+,"), needs_page: false, run: |w, win, cx| w.on_open_settings(&ws::OpenSettings, win, cx) },
];

/// The most rows shown at once; more scroll.
const SHOWN: f32 = 10.0;
const ROW_H: f32 = 32.0;
const HEAD_H: f32 = 26.0;
const WIDTH: f32 = 620.0;

/// How well `query` (words, any order, each matched) fits `text`, if at
/// all: a word found whole beats letters in order; at a word's start, more.
pub fn score(query: &str, text: &str) -> Option<i32> {
    let text = text.to_lowercase();
    let mut total = 0;
    for word in query.to_lowercase().split_whitespace() {
        total += word_score(word, &text)?;
    }
    Some(total)
}

fn word_score(word: &str, text: &str) -> Option<i32> {
    if let Some(at) = text.find(word) {
        let starts_word = text[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        return Some(1000 - at.min(200) as i32 + if starts_word { 500 } else { 0 });
    }
    // Its letters in order, fewer skipped letters ranking higher.
    let mut rest = text.chars();
    let mut skipped = 0;
    for c in word.chars() {
        loop {
            match rest.next() {
                Some(t) if t == c => break,
                Some(_) => skipped += 1,
                None => return None,
            }
        }
    }
    Some(300 - skipped.min(250))
}

impl Workspace {
    pub fn open_palette(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        self.settings = None;
        let input = cx.new(|cx| TextInput::new(intent.placeholder(), window, cx));
        let events = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Changed => this.refresh_palette(cx),
            InputEvent::Submit(_) => this.run_choice(None, window, cx),
            InputEvent::Cancel => this.cancel_palette(window, cx),
            InputEvent::Focused | InputEvent::Blurred => {}
        });
        window.focus(&input.read(cx).focus_handle(cx), cx);
        self.palette = Some(Palette { input, intent, choices: Vec::new(), selected: 0, scroll: ScrollHandle::new(), _events: events });
        self.refresh_palette(cx);
    }

    /// Esc: closes it, unless the window has nothing else to show.
    fn cancel_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            let Some(p) = self.palette.as_ref() else { return };
            p.input.update(cx, |i, cx| i.set_text("", cx));
            return self.refresh_palette(cx);
        }
        self.close_palette(window, cx);
    }

    pub fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_none() {
            return;
        }
        self.close_palette_focus(window, cx);
        cx.notify();
    }

    pub fn move_palette_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(p) = self.palette.as_mut() else { return };
        if p.choices.is_empty() {
            return;
        }
        // Stops at the ends, as herdr's does.
        p.selected = (p.selected as isize + delta).clamp(0, p.choices.len() as isize - 1) as usize;
        let row = row_index(&p.choices, p.selected, p.intent);
        p.scroll.scroll_to_item(row);
        cx.notify();
    }

    fn refresh_palette(&mut self, cx: &mut Context<Self>) {
        let Some((query, intent)) = self.palette.as_ref().map(|p| (p.input.read(cx).text().trim().to_string(), p.intent)) else {
            return;
        };
        let history = self.history_choices(&query, cx);
        let choices = self.palette_choices(&query, intent, history, cx);
        if let Some(p) = self.palette.as_mut() {
            p.choices = choices;
            p.selected = 0;
            p.scroll.scroll_to_item(0);
        }
        cx.notify();
    }

    /// Open pages first (when any match), then the address or search, then
    /// commands.
    /// History matching `query`, leaving out what is open already.
    fn history_choices(&mut self, query: &str, cx: &App) -> Vec<Choice> {
        if query.is_empty() {
            return Vec::new();
        }
        let open: HashSet<String> =
            self.panes.values().map(|p| p.view.read(cx).url().to_string()).collect();
        let pages = self.db.search(query, 12);
        pages
            .into_iter()
            .filter(|p| !open.contains(&p.url))
            .take(8)
            .map(|p| {
                let icon = self.cached_favicon(url::host(&p.url));
                Choice::History { url: p.url, title: p.title, icon }
            })
            .collect()
    }

    fn palette_choices(&self, query: &str, intent: Intent, history: Vec<Choice>, cx: &App) -> Vec<Choice> {
        // A split can bring over a page from another space, not its own.
        let skip_tab = match intent {
            Intent::Split { target, .. } => self.tab_of(target),
            _ => None,
        };
        let mut pages: Vec<(i32, usize, PaneId)> = Vec::new();
        for (t, tab) in self.tabs.iter().enumerate() {
            if Some(t) == skip_tab {
                continue;
            }
            for pane in tab.root.panes() {
                let Some(view) = self.panes.get(&pane).map(|p| p.view.read(cx)) else { continue };
                if url::display(view.url()).is_empty() {
                    continue;
                }
                let fit = if query.is_empty() {
                    Some(0)
                } else {
                    [
                        score(query, &page_title(view)),
                        score(query, url::host(view.url())).map(|s| s + 100),
                        score(query, view.url()).map(|s| s - 100),
                        tab.name.as_deref().and_then(|n| score(query, n)),
                    ]
                    .into_iter()
                    .flatten()
                    .max()
                };
                if let Some(fit) = fit {
                    pages.push((fit, t, pane));
                }
            }
        }
        // Best first; ties keep the sidebar's order.
        pages.sort_by_key(|(fit, t, _)| (-fit, *t));
        let pages = pages.into_iter().map(|(_, _, pane)| Choice::Pane(pane));

        let go = (!query.is_empty()).then(|| {
            if url::is_address(query) { Choice::Open(url::resolve(query)) } else { Choice::Search(query.to_string()) }
        });

        let mut commands: Vec<(i32, usize)> = Vec::new();
        if intent == Intent::Anything {
            for (i, c) in COMMANDS.iter().enumerate() {
                if c.needs_page && self.tabs.is_empty() {
                    continue;
                }
                let fit = if query.is_empty() { Some(0) } else { score(query, &format!("{} {}", c.title, c.keywords)) };
                if let Some(fit) = fit {
                    commands.push((fit, i));
                }
            }
            if !query.is_empty() {
                commands.sort_by_key(|(fit, i)| (-fit, *i));
            }
        }
        // Commands named by what was typed ("restart", "split"); the rest
        // follow everything else.
        let named = |i: usize| {
            let q = query.to_lowercase();
            q.len() >= 2 && COMMANDS[i].title.to_lowercase().split_whitespace().any(|w| w.starts_with(&q))
        };
        let (named, commands): (Vec<_>, Vec<_>) = commands.into_iter().map(|(_, i)| i).partition(|i| named(*i));
        let named = named.into_iter().map(Choice::Command);
        let commands = commands.into_iter().map(Choice::Command);

        // A command named outright first (typing "restart" means it, even
        // if a page about restarting was visited); then what is open, then
        // history, then the web, unless nothing known matches, when going
        // straight there comes first.
        let mut out: Vec<Choice> = named.collect();
        let pages: Vec<_> = pages.collect();
        if (pages.is_empty() && history.is_empty()) || query.is_empty() {
            out.extend(go);
            out.extend(pages);
        } else {
            out.extend(pages);
            out.extend(history);
            out.extend(go);
        }
        out.extend(commands);
        out
    }

    /// Runs the chosen row (`None`: the selected one).
    pub fn run_choice(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.palette.as_ref() else { return };
        let Some(choice) = p.choices.get(index.unwrap_or(p.selected)).cloned() else { return };
        let intent = p.intent;
        self.palette = None;
        match choice {
            Choice::Pane(pane) => match intent {
                Intent::Split { target, edge } => self.move_beside(pane, target, edge, window, cx),
                _ => self.reveal_pane(pane, window, cx),
            },
            Choice::Open(address) | Choice::History { url: address, .. } => self.open_for(intent, &address, window, cx),
            Choice::Search(words) => self.open_for(intent, &url::resolve(&words), window, cx),
            Choice::Command(i) => {
                // Back to the page first; the command may open the palette again.
                self.close_palette_focus(window, cx);
                (COMMANDS[i].run)(self, window, cx);
            }
        }
        cx.notify();
    }

    /// The keyboard back to the focused page (or the window, if none).
    fn close_palette_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_tab().map(|t| t.focused) {
            Some(pane) => self.focus_pane(pane, window, cx),
            None => window.focus(&self.focus, cx),
        }
    }

    fn open_for(&mut self, intent: Intent, address: &str, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            Intent::Split { target, edge } => self.split_with(target, edge, address, window, cx),
            Intent::Anything | Intent::NewSpace => self.open_tab(address, true, window, cx),
        }
    }

    /// The palette, over the pane area: near its top, or in its middle
    /// when the window is empty (as zeron's empty window).
    pub fn render_palette(&mut self, area: Rect, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(p) = self.palette.as_ref() else { return div() };
        let empty = self.tabs.is_empty();
        let width = WIDTH.min(area.w - 32.0).max(260.0);
        let left = area.x + (area.w - width) / 2.0;

        let mut rows: Vec<AnyElement> = Vec::new();
        let mut group = None;
        for (i, choice) in p.choices.iter().enumerate() {
            let head = group_of(choice, p.intent);
            if group != Some(head) {
                group = Some(head);
                rows.push(
                    div()
                        .flex()
                        .flex_none()
                        .items_end()
                        .h(px(HEAD_H))
                        .px(px(14.))
                        .pb(px(4.))
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_dim())
                        .child(head)
                        .into_any_element(),
                );
            }
            rows.push(self.palette_row(i, choice, i == p.selected, p.intent, cx).into_any_element());
        }
        let heads = rows.len() - p.choices.len();
        let list_h = (p.choices.len() as f32 * ROW_H + heads as f32 * HEAD_H).min(SHOWN * ROW_H + 2.0 * HEAD_H);

        let field = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(36.))
            .px(px(10.))
            .rounded(px(9.))
            .border_1()
            .border_color(theme::accent())
            .bg(theme::canvas())
            .child(icon_glyph("icons/search.svg", 15.0).text_color(theme::text_dim()))
            .when_some(p.intent.label(), |f, label| {
                f.child(
                    div()
                        .flex_none()
                        .px(px(6.))
                        .py(px(1.))
                        .rounded(px(5.))
                        .bg(theme::accent_soft())
                        .text_size(px(11.5))
                        .text_color(theme::text())
                        .child(label),
                )
            })
            .child(div().flex_1().min_w_0().text_size(px(13.)).child(p.input.clone()))
            .when(!empty, |f| f.child(div().text_size(px(11.)).text_color(theme::text_dim()).child("Esc")));

        let card = div()
            .id("palette")
            .key_context("Palette")
            .absolute()
            .left(px(left))
            .when(empty, |c| c.top(px(area.y + area.h * 0.28)))
            .when(!empty, |c| c.top(px(area.y + 52.0)))
            .w(px(width))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(14.))
            .rounded(px(10.))
            .bg(theme::popover())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &ws::PaletteUp, _, cx| this.move_palette_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &ws::PaletteDown, _, cx| this.move_palette_selection(1, cx)))
            .child(field)
            .when(!rows.is_empty(), |c| {
                c.child(
                    div()
                        .id("palette-rows")
                        .flex()
                        .flex_col()
                        .h(px(list_h))
                        .overflow_y_scroll()
                        .track_scroll(&p.scroll)
                        .children(rows),
                )
            });

        // Over the pages (not the sidebar: its window controls, dragging
        // and spaces keep working), a click outside closes it, unless
        // there is nothing behind it.
        let outside = div()
            .absolute()
            .left(px(area.x))
            .top(px(area.y))
            .w(px(area.w))
            .h(px(area.h))
            .occlude()
            .when(!empty, |d| d.on_mouse_down(MouseButton::Left, cx.listener(|this, _, w, cx| this.close_palette(w, cx))));
        div().absolute().inset_0().child(outside).child(card)
    }
}

/// The group title a row shows under.
fn group_of(choice: &Choice, intent: Intent) -> &'static str {
    match (choice, intent) {
        (Choice::Pane(_), Intent::Split { .. }) => "Move here",
        (Choice::Pane(_), _) => "Open",
        (Choice::History { .. }, _) => "History",
        (Choice::Open(_) | Choice::Search(_), _) => "Go",
        (Choice::Command(_), _) => "Commands",
    }
}

/// Where row `choice` sits among the list's children: rows plus a group
/// title wherever the group changes (as [`Workspace::render_palette`] draws).
fn row_index(choices: &[Choice], choice: usize, intent: Intent) -> usize {
    let mut heads = 0;
    let mut last = None;
    for c in &choices[..=choice] {
        let group = group_of(c, intent);
        if last != Some(group) {
            heads += 1;
            last = Some(group);
        }
    }
    choice + heads
}

impl Workspace {

    fn palette_row(&self, i: usize, choice: &Choice, on: bool, intent: Intent, cx: &mut Context<Self>) -> impl IntoElement {
        let (tile, title, detail, hint): (AnyElement, String, Option<String>, Option<AnyElement>) = match choice {
            Choice::Pane(pane) => {
                let view = self.panes[pane].view.read(cx);
                let hint = match intent {
                    Intent::Split { .. } => "Move here",
                    _ => "Switch to",
                };
                (
                    site_tile(view.favicon(), view.url(), 18.0).into_any_element(),
                    page_title(view),
                    Some(url::host(view.url()).to_string()),
                    Some(div().text_size(px(11.5)).text_color(theme::text_dim()).child(hint).into_any_element()),
                )
            }
            Choice::History { url: address, title, icon } => (
                site_tile(icon.clone(), address, 18.0).into_any_element(),
                if title.is_empty() { url::host(address).to_string() } else { title.clone() },
                Some(url::display(address)),
                None,
            ),
            Choice::Open(address) => (
                site_tile(None, address, 18.0).into_any_element(),
                format!("Open {}", url::host(address)),
                Some(address.clone()),
                None,
            ),
            Choice::Search(words) => (
                glyph_tile("icons/search.svg"),
                format!("Search for \u{201c}{words}\u{201d}"),
                Some("Google".to_string()),
                None,
            ),
            Choice::Command(c) => {
                let c = &COMMANDS[*c];
                // No picture; the space keeps titles in line with the rest.
                (div().flex_none().size(px(18.)).into_any_element(), c.title.to_string(), None, c.key.map(|k| keys(k).into_any_element()))
            }
        };
        div()
            .id(SharedString::from(format!("palette-row-{i}")))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(ROW_H))
            .mx(px(2.))
            .px(px(10.))
            .rounded(px(7.))
            .text_size(px(12.8))
            .when(on, |r| r.bg(theme::selected()))
            .hover(|r| r.bg(theme::hover()))
            .child(tile)
            .child(div().flex_none().max_w(px(300.)).truncate().text_color(if on { theme::text() } else { theme::text_muted() }).child(title))
            .child(div().flex_1().min_w_0().truncate().text_size(px(11.5)).text_color(theme::text_dim()).children(detail))
            .children(hint)
            .on_click(cx.listener(move |this, _, w, cx| this.run_choice(Some(i), w, cx)))
    }
}

fn glyph_tile(icon: &'static str) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(18.))
        .rounded(px(4.5))
        .bg(theme::wash(0.06))
        .child(icon_glyph(icon, 12.0))
        .into_any_element()
}

/// A shortcut as key caps: "Ctrl+T" → [Ctrl] [T].
fn keys(combo: &'static str) -> impl IntoElement {
    // "Ctrl+Shift+\\": split on '+' but keep a '+' key itself.
    let parts: Vec<&str> = combo.split('+').filter(|p| !p.is_empty()).collect();
    div().flex().flex_none().gap(px(3.)).children(parts.into_iter().map(|p| {
        div()
            .px(px(5.))
            .rounded(px(4.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::wash(0.04))
            .text_size(px(11.))
            .text_color(theme::text_dim())
            .child(p)
    }))
}

//! The palette (herdr's Ctrl+Shift+P): one search box for pages already
//! open, the web, and commands. New spaces and splits come from it too:
//! nothing is created until it has somewhere to go, and Esc creates nothing.
//!
//! Search first: what is already open ranks above opening it again, so
//! typing "slack" goes to the Slack you have rather than a second one.
//! A leading `?` searches the web only, `??` history only.

use crate::chrome::icon_glyph;
use crate::render::{page_title, site_tile};
use crate::text_input::{InputEvent, TextInput};
use crate::workspace::{self as ws, Workspace};
use crate::settings::Matching;
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
    /// A new pane beside `target`: opens what is chosen there, or shows an
    /// open page there too (linked: it stays in its own space).
    Split { target: PaneId, edge: Edge },
    /// `!` typed first: what is chosen takes `target`'s place, and what was
    /// there goes to a space of its own (as dragged to the sidebar).
    Here { target: PaneId },
}

impl Intent {
    fn label(self) -> Option<&'static str> {
        match self {
            Self::Anything => None,
            Self::NewSpace => Some("New space"),
            Self::Split { edge: Edge::Bottom | Edge::Top, .. } => Some("Split down"),
            Self::Split { .. } => Some("Split right"),
            Self::Here { .. } => Some("Put here"),
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Self::Anything => "Search open pages, the web, or commands  (? web, ?? history, ! here)",
            Self::NewSpace | Self::Split { .. } | Self::Here { .. } => {
                "Search open pages or the web, or enter an address  (? web, ?? history)"
            }
        }
    }
}

#[derive(Clone)]
pub enum Choice {
    /// A page that is already open.
    Pane(PaneId),
    /// A linked page in one of its spaces (→ on its row lists them).
    InSpace(PaneId, ws::TabId),
    /// A page from history (not open now).
    History { url: String, title: String, icon: Option<Arc<RenderImage>> },
    /// An address to load.
    Open(String),
    /// Words to search the web for.
    Search(String),
    Command(usize),
}

impl Palette {
    /// What the palette does now: what it was opened for, unless `!` asks
    /// for the focused pane.
    pub fn shown(&self) -> Intent {
        self.here.map_or(self.intent, |target| Intent::Here { target })
    }
}

pub struct Palette {
    pub input: Entity<TextInput>,
    pub intent: Intent,
    /// The focused pane, while the query starts with `!`.
    pub here: Option<PaneId>,
    /// What each open page's site calls itself, by host (see
    /// [`crate::db::Db::site_names`]), read as the palette opens.
    pub sites: std::collections::HashMap<String, Vec<String>>,
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
    Command { title: "Move pane to its own space", keywords: "shelve put away sidebar new space detach", key: Some("Ctrl+Alt+S"), needs_page: true, run: |w, win, cx| w.on_put_away_pane(&ws::PutAwayPane, win, cx) },
    Command { title: "Copy page address", keywords: "url link clipboard", key: Some("Ctrl+Shift+C"), needs_page: true, run: |w, win, cx| w.on_copy_address(&ws::CopyAddress, win, cx) },
    Command { title: "Close pane", keywords: "page kill", key: Some("Ctrl+W"), needs_page: true, run: |w, win, cx| w.on_close_pane(&ws::ClosePane, win, cx) },
    Command { title: "Close space", keywords: "tab kill", key: Some("Ctrl+Shift+W"), needs_page: true, run: |w, win, cx| w.on_close_tab(&ws::CloseTab, win, cx) },
    Command { title: "Rename space", keywords: "tab name", key: Some("F2"), needs_page: true, run: |w, win, cx| w.on_rename_space(&ws::RenameSpace, win, cx) },
    Command { title: "Next space", keywords: "tab", key: Some("Ctrl+PageDown"), needs_page: true, run: |w, win, cx| w.on_next_tab(&ws::NextTab, win, cx) },
    Command { title: "Previous space", keywords: "tab", key: Some("Ctrl+PageUp"), needs_page: true, run: |w, win, cx| w.on_prev_tab(&ws::PrevTab, win, cx) },
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
pub fn score(query: &str, text: &str, matching: Matching) -> Option<i32> {
    let text = text.to_lowercase();
    let mut total = 0;
    for word in query.to_lowercase().split_whitespace() {
        total += word_score(word, &text, matching)?;
    }
    Some(total)
}

fn word_score(word: &str, text: &str, matching: Matching) -> Option<i32> {
    if let Some(at) = text.find(word) {
        let starts_word = text[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        return Some(1000 - at.min(200) as i32 + if starts_word { 500 } else { 0 });
    }
    match matching {
        Matching::Exact => return None,
        // Letters in order anywhere, below everything else.
        Matching::Loose => return near_score(word, text).or_else(|| scattered(word, text)),
        Matching::Typos => {}
    }
    near_score(word, text)
}

/// The old loose match: `word`'s letters in order anywhere in `text`.
fn scattered(word: &str, text: &str) -> Option<i32> {
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
    Some(150 - skipped.min(140))
}

/// Not found whole: a word one letter off, or the letters kept together.
fn near_score(word: &str, text: &str) -> Option<i32> {
    // A word with one letter off (swapped, missing, extra or wrong):
    // typed before coffee.
    let typo = typo_score(word, text);
    // Its letters in order, close together or starting words ("gcal":
    // Google Calendar), fewer skipped letters ranking higher. Letters
    // strewn across a long title ("gmail" in "GitHub - moq: Media over
    // QUIC: Real-time…") are no match.
    typo.max(in_order(word, text))
}

/// `word`'s letters in order in `text`, kept together (see [`word_score`]).
fn in_order(word: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.chars().collect();
    let starts = |i: usize| i == 0 || !text[i - 1].is_alphanumeric();
    let mut at = 0;
    let mut skipped = 0;
    for (n, c) in word.chars().enumerate() {
        // The very next letter, else one starting a word, else any.
        let found = if text.get(at) == Some(&c) {
            at
        } else {
            (at..text.len())
                .find(|&i| text[i] == c && starts(i))
                .or_else(|| (at..text.len()).find(|&i| text[i] == c))?
        };
        // The first letter starts a word; later ones follow on, or start
        // a word of their own (what was passed over doesn't count).
        if n == 0 && !starts(found) {
            return None;
        }
        if n > 0 && !starts(found) {
            skipped += found - at;
        }
        at = found + 1;
    }
    let letters = word.chars().count();
    (skipped <= letters.max(2)).then(|| 300 - skipped as i32 * 20)
}

/// A word of `text` (or its start) one letter off `word`: 4 letters or
/// more, so short ones don't match everything.
fn typo_score(word: &str, text: &str) -> Option<i32> {
    let w: Vec<char> = word.chars().collect();
    if w.len() < 4 {
        return None;
    }
    text.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()).find_map(|t| {
        let t: Vec<char> = t.chars().collect();
        // The word whole, or as far as was typed (one letter either way).
        (w.len() - 1..=w.len() + 1).filter(|&n| n <= t.len()).any(|n| one_off(&w, &t[..n])).then_some(400)
    })
}

/// Whether `a` and `b` are one edit apart, two neighbors swapped counting
/// as one (optimal string alignment distance 1).
fn one_off(a: &[char], b: &[char]) -> bool {
    let (n, m) = (a.len(), b.len());
    if n.abs_diff(m) > 1 {
        return false;
    }
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for i in 0..=n {
        d[i][0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[n][m] <= 1
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
        let mut sites = std::collections::HashMap::new();
        for p in self.panes.values() {
            let host = url::host(p.view.read(cx).url()).to_string();
            if !host.is_empty() && !sites.contains_key(&host) {
                let names = self.db.site_names(&host);
                sites.insert(host, names);
            }
        }
        self.palette = Some(Palette { input, intent, here: None, sites, choices: Vec::new(), selected: 0, scroll: ScrollHandle::new(), _events: events });
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
        let row = row_index(&p.choices, p.selected, p.shown());
        p.scroll.scroll_to_item(row);
        cx.notify();
    }

    fn refresh_palette(&mut self, cx: &mut Context<Self>) {
        let Some((query, intent)) = self.palette.as_ref().map(|p| (p.input.read(cx).text().trim().to_string(), p.intent)) else {
            return;
        };
        // `!` first: for the focused pane; the rest reads as usual.
        let (query, here) = match query.strip_prefix('!') {
            Some(rest) => (rest.trim_start().to_string(), self.active_tab().map(|t| t.focused)),
            None => (query, None),
        };
        let intent = here.map_or(intent, |target| Intent::Here { target });
        let choices = if let Some(words) = query.strip_prefix("??") {
            self.history_only(words.trim())
        } else if let Some(words) = query.strip_prefix('?') {
            let words = words.trim();
            if words.is_empty() { Vec::new() } else { vec![Choice::Search(words.to_string())] }
        } else {
            let history = self.history_choices(&query, cx);
            self.palette_choices(&query, intent, history, cx)
        };
        if let Some(p) = self.palette.as_mut() {
            p.here = here;
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

    /// `??`: history alone, open pages included (nothing typed: the latest).
    fn history_only(&mut self, words: &str) -> Vec<Choice> {
        let pages = if words.is_empty() { self.db.recent(50) } else { self.db.search(words, 50) };
        pages
            .into_iter()
            .map(|p| {
                let icon = self.cached_favicon(url::host(&p.url));
                Choice::History { url: p.url, title: p.title, icon }
            })
            .collect()
    }

    fn palette_choices(&self, query: &str, intent: Intent, history: Vec<Choice>, cx: &App) -> Vec<Choice> {
        // A split can bring over a page from another space, not one
        // already beside it.
        let skip_tab = match intent {
            Intent::Split { target, .. } => self.tab_of(target),
            _ => None,
        };
        // Each page once, linked or not, placed by the space it goes to.
        // (match, extra from `nudge`, space, page)
        let mut pages: Vec<(i32, i32, usize, PaneId)> = Vec::new();
        let mut seen = HashSet::new();
        for tab in &self.tabs {
            for pane in tab.root.panes() {
                if !seen.insert(pane) || intent == (Intent::Here { target: pane }) {
                    continue;
                }
                if skip_tab.is_some_and(|t| self.tabs[t].root.contains(pane)) {
                    continue;
                }
                let Some(view) = self.panes.get(&pane).map(|p| p.view.read(cx)) else { continue };
                if url::display(view.url()).is_empty() {
                    continue;
                }
                let spaces = self.spaces_of(pane);
                let fit = if query.is_empty() {
                    Some((0, 0))
                } else {
                    let m = self.store.settings.matching;
                    let names = self.palette.as_ref().and_then(|p| p.sites.get(url::host(view.url())));
                    // What it's called: its title, or its site's own name
                    // ("gmail": a Workspace inbox titled by account).
                    let named = std::iter::once(score(query, &page_title(view), m))
                        .chain(names.into_iter().flatten().map(|n| score(query, n, m)))
                        .flatten()
                        .max();
                    // Where it is.
                    let address = [score(query, url::host(view.url()), m).map(|s| s + 100), score(query, view.url(), m).map(|s| s - 100)]
                        .into_iter()
                        .flatten()
                        .max();
                    let space = spaces.iter().filter_map(|&t| self.tabs[t].name.as_deref().and_then(|n| score(query, n, m))).max();
                    // The best counts whole; each other that matches adds
                    // a quarter (a title and an address beat a title alone).
                    let mut fits: Vec<i32> = [named, address, space].into_iter().flatten().collect();
                    fits.sort_unstable_by(|a, b| b.cmp(a));
                    let fit = fits.first().map(|best| best + fits[1..].iter().map(|f| f / 4).sum::<i32>());
                    let solo = spaces.iter().any(|&t| self.is_solo(t));
                    let visited = spaces.iter().map(|&t| self.visit_order(t)).min().unwrap_or(usize::MAX);
                    fit.map(|fit| (fit, nudge(space.is_some(), solo, visited)))
                };
                if let (Some((fit, extra)), Some(t)) = (fit, self.home_of(pane)) {
                    pages.push((fit, extra, t, pane));
                }
            }
        }
        // Best first; ties keep the sidebar's order.
        pages.sort_by_key(|(fit, extra, t, _)| (-(fit + extra), *t));
        // How open pages ranked (never what was typed), for a ranking
        // that surprises.
        if !query.is_empty() && tracing::enabled!(tracing::Level::DEBUG) {
            let top: Vec<String> = pages
                .iter()
                .take(5)
                .filter_map(|(fit, extra, t, pane)| {
                    let view = self.panes.get(pane)?.view.read(cx);
                    Some(format!("{fit}+{extra} {:?} ({}) in space {t}", page_title(view), url::host(view.url())))
                })
                .collect();
            tracing::debug!("palette: open pages ranked: {}", top.join("; "));
        }
        let pages = pages.into_iter().map(|(_, _, _, pane)| Choice::Pane(pane));

        let go = (!query.is_empty()).then(|| {
            if url::is_address(query) { Choice::Open(url::resolve(query)) } else { Choice::Search(query.to_string()) }
        });

        let mut commands: Vec<(i32, usize)> = Vec::new();
        if intent == Intent::Anything {
            for (i, c) in COMMANDS.iter().enumerate() {
                if c.needs_page && self.tabs.is_empty() {
                    continue;
                }
                let fit = if query.is_empty() { Some(0) } else { score(query, &format!("{} {}", c.title, c.keywords), self.store.settings.matching) };
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
        // An address typed means that address: pages open at it first, then
        // going there, then history under it; whatever only mentions it
        // (its GitHub, a page about it) after.
        if url::is_address(query) {
            let at = |c: &Choice| match c {
                Choice::Pane(pane) => self.panes.get(pane).is_some_and(|p| url::is_under(p.view.read(cx).url(), query)),
                Choice::History { url: u, .. } => url::is_under(u, query),
                _ => false,
            };
            let (open_at, pages): (Vec<_>, Vec<_>) = pages.into_iter().partition(|c| at(c));
            let (history_at, history): (Vec<_>, Vec<_>) = history.into_iter().partition(|c| at(c));
            out.extend(open_at);
            out.extend(go);
            out.extend(history_at);
            out.extend(pages);
            out.extend(history);
        } else if (pages.is_empty() && history.is_empty()) || query.is_empty() {
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
        let intent = p.shown();
        self.palette = None;
        match choice {
            Choice::Pane(pane) => match intent {
                Intent::Split { target, edge } => self.link_beside(pane, target, edge, window, cx),
                Intent::Here { target } => {
                    self.link_beside(pane, target, Edge::Right, window, cx);
                    self.put_away(target, window, cx);
                }
                _ => self.reveal_pane(pane, window, cx),
            },
            Choice::InSpace(pane, tab) => {
                if let Some(index) = self.tabs.iter().position(|t| t.id == tab && t.root.contains(pane)) {
                    self.tabs[index].focused = pane;
                    self.activate(index, window, cx);
                }
            }
            Choice::Open(address) | Choice::History { url: address, .. } => self.open_for(intent, &address, window, cx),
            Choice::Search(words) => self.open_for(intent, &url::search(&words), window, cx),
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
            Intent::Here { target } => {
                self.split_with(target, Edge::Right, address, window, cx);
                self.put_away(target, window, cx);
            }
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
            let head = group_of(choice, p.shown());
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
            rows.push(self.palette_row(i, choice, i == p.selected, p.shown(), cx).into_any_element());
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
            .when_some(p.shown().label(), |f, label| {
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
            // → at the end of what was typed: a linked page's spaces.
            .on_action(cx.listener(|this, _: &crate::text_input::Right, _, cx| this.expand_spaces(cx)))
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
        (Choice::Pane(_), Intent::Split { .. }) => "Show here",
        (Choice::Pane(_), Intent::Here { .. }) => "Put here",
        (Choice::Pane(_), _) => "Open",
        (Choice::InSpace(..), _) => "Spaces",
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
    /// → on a linked page's row: a row for each of its spaces instead, the
    /// one Enter would have gone to first.
    fn expand_spaces(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.palette.as_ref() else { return };
        let (Some(Choice::Pane(pane)), Intent::Anything | Intent::NewSpace) = (p.choices.get(p.selected), p.shown()) else { return };
        let pane = *pane;
        let mut spaces = self.spaces_of(pane);
        if spaces.len() < 2 {
            return;
        }
        let first = self.home_of(pane);
        spaces.sort_by_key(|&t| Some(t) != first);
        let choices = spaces.into_iter().map(|t| Choice::InSpace(pane, self.tabs[t].id)).collect();
        let Some(p) = self.palette.as_mut() else { return };
        p.choices = choices;
        p.selected = 0;
        p.scroll.scroll_to_item(0);
        cx.notify();
    }

    /// Where an open page is, under its title: its site, and its space
    /// when that says more (named, split, or linked: each space).
    fn where_shown(&self, pane: PaneId, cx: &App) -> String {
        let host = url::host(self.panes[&pane].view.read(cx).url()).to_string();
        let first = self.home_of(pane);
        let mut spaces = self.spaces_of(pane);
        spaces.sort_by_key(|&t| Some(t) != first);
        let Some(&home) = spaces.first() else { return host };
        let tab = &self.tabs[home];
        if spaces.len() == 1 && tab.name.is_none() && tab.root.panes().len() == 1 {
            return host;
        }
        let mut text = format!("{host} · {}", self.layout_label(home, cx));
        if spaces.len() > 1 {
            let others: Vec<String> = spaces[1..].iter().map(|&t| self.space_name(&self.tabs[t], cx)).collect();
            text.push_str(&format!(", also {}", others.join(", ")));
        }
        text
    }

    /// A space as named, with its split pages when it has several.
    fn layout_label(&self, index: usize, cx: &App) -> String {
        let tab = &self.tabs[index];
        match &tab.name {
            Some(name) if tab.root.panes().len() > 1 => format!("{name}: {}", self.layout_text(&tab.root, cx)),
            Some(name) => name.clone(),
            None if tab.root.panes().len() > 1 => self.layout_text(&tab.root, cx),
            None => self.space_name(tab, cx),
        }
    }

    /// A split as text: side by side `a | b`, stacked `a / b`.
    fn layout_text(&self, node: &layout::Node, cx: &App) -> String {
        match node {
            layout::Node::Pane { pane } => {
                let Some(p) = self.panes.get(pane) else { return String::new() };
                let title = page_title(p.view.read(cx));
                match title.char_indices().nth(18) {
                    Some((at, _)) => format!("{}…", title[..at].trim_end()),
                    None => title,
                }
            }
            layout::Node::Split { axis, children } => {
                let sep = if *axis == layout::Axis::Row { " | " } else { " / " };
                let parts: Vec<String> = children
                    .iter()
                    .map(|c| match c.node {
                        layout::Node::Split { .. } => format!("({})", self.layout_text(&c.node, cx)),
                        _ => self.layout_text(&c.node, cx),
                    })
                    .collect();
                parts.join(sep)
            }
        }
    }

    fn palette_row(&self, i: usize, choice: &Choice, on: bool, intent: Intent, cx: &mut Context<Self>) -> impl IntoElement {
        let (tile, title, detail, hint): (AnyElement, String, Option<String>, Option<AnyElement>) = match choice {
            Choice::Pane(pane) => {
                let view = self.panes[pane].view.read(cx);
                let hint = match intent {
                    Intent::Split { .. } => "Show here",
                    Intent::Here { .. } => "Put here",
                    _ if self.spaces_of(*pane).len() > 1 => "Switch to  → spaces",
                    _ => "Switch to",
                };
                (
                    site_tile(view.favicon(), view.url(), 18.0).into_any_element(),
                    page_title(view),
                    Some(self.where_shown(*pane, cx)),
                    Some(div().text_size(px(11.5)).text_color(theme::text_dim()).child(hint).into_any_element()),
                )
            }
            Choice::InSpace(pane, tab) => {
                let Some(tab) = self.tabs.iter().find(|t| t.id == *tab) else { return div().id(SharedString::from(format!("palette-row-{i}"))) };
                let view = self.panes[pane].view.read(cx);
                (
                    site_tile(view.favicon(), view.url(), 18.0).into_any_element(),
                    self.space_name(tab, cx),
                    Some(self.layout_text(&tab.root, cx)),
                    Some(div().text_size(px(11.5)).text_color(theme::text_dim()).child("Go to").into_any_element()),
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


/// Small extras that only decide between pages matching about as well
/// (a typo scores 400, a whole word 1000 and up): a space named for what
/// was typed, then a page alone in a space (Gmail itself, not the Gmail
/// in a split), then the spaces visited last (0: the current one).
fn nudge(named_space: bool, solo: bool, visited: usize) -> i32 {
    let recent = 15 - 3 * visited.min(5) as i32;
    (if named_space { 40 } else { 0 }) + (if solo { 20 } else { 0 }) + recent
}

#[cfg(test)]
mod tests {
    use super::{nudge, score};
    use crate::settings::Matching;

    #[test]
    fn nudges() {
        // Alone beats in a split, even visited longer ago.
        assert!(nudge(false, true, usize::MAX) > nudge(false, false, 0));
        // A space named for it beats both.
        assert!(nudge(true, false, usize::MAX) > nudge(false, true, 0));
        // Otherwise the more recent first.
        assert!(nudge(false, true, 1) > nudge(false, true, 3));
        // Never enough to lift a typo over a whole word.
        assert!(400 + nudge(true, true, 0) < 1000);
    }

    #[test]
    fn whole_words_beat_scattered_letters() {
        assert!(score("gmail", "Inbox (3) - someone@gmail.com - Gmail", Matching::Typos).unwrap() > 1000);
        // Letters strewn across a long title are no match.
        assert_eq!(score("gmail", "GitHub - moq-dev/moq: Media over QUIC: Real-time", Matching::Typos), None);
        assert_eq!(score("gmail", "curobo/AGENTS.md at main · NVlabs/curobo · GitHub", Matching::Typos), None);
        // Initials and near-runs still match.
        assert!(score("gcal", "Google Calendar", Matching::Typos).is_some());
        assert!(score("ghub", "github.com", Matching::Typos).is_some());
        assert!(score("yt", "YouTube", Matching::Typos).is_some());
        // One letter off, for words of four letters or more.
        for typo in ["gmial", "gmal", "gnail", "gmaill"] {
            assert!(score(typo, "Inbox (3) - Gmail", Matching::Typos).is_some(), "{typo}");
        }
        assert_eq!(score("gmial", "GitHub - moq-dev/moq: Media over QUIC", Matching::Typos), None);
        assert_eq!(score("yuo", "YouTube", Matching::Typos), None);
        // Exact takes it whole; Loose takes letters anywhere, ranked low.
        assert_eq!(score("gmial", "Gmail", Matching::Exact), None);
        assert!(score("gmail", "GitHub - moq-dev/moq: Media over QUIC: Real-time", Matching::Loose).unwrap() < 300);
    }
}

//! Drawing the workspace: the sidebar (full, or collapsed to a rail of
//! tiles) and the active tab's pane cards with their headers and dividers.

use crate::activity::{self, Kind};
use crate::chrome::{self, icon_button, icon_glyph, small_button};
use crate::palette::Intent;
use crate::workspace::{DRAG_THRESHOLD, DividerDrag, DropTarget, PaneDrag, SidebarDrag, Tab, Workspace};
use crate::theme::Ink;
use crate::{theme, url};
use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, Div, Entity, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit, ParentElement as _, Pixels, Render, RenderImage,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, StyledImage as _, Transformation, Window, div, img,
    percentage, prelude::FluentBuilder as _, px,
};
use layout::{Axis, Edge, PaneId, Rect};
use std::sync::Arc;
use std::time::Duration;
use web::{Ask, Playback, WebView};

/// How far a divider's grab area reaches into the panes on each side.
const DIVIDER_REACH: f32 = 3.0;

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let radius = chrome::corner_radius(window);
        let viewport = window.viewport_size();
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let side = self.sidebar_width();
        let g = theme::GAP;
        // Floating, the sidebar is a card like the panes, with the gap around it.
        let floating = self.store.settings.floating_sidebar;
        let inset = self.sidebar_inset();
        let edge = inset + side;
        let area = Rect::new(edge + g, g, (vw - edge - 2.0 * g).max(0.0), (vh - 2.0 * g).max(0.0));

        let root = div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .size_full()
            .relative()
            .font_family(theme::FONT)
            .text_size(px(13.))
            .text_color(theme::text())
            .on_action(cx.listener(Workspace::on_new_tab))
            .on_action(cx.listener(Workspace::on_close_pane))
            .on_action(cx.listener(Workspace::on_close_tab))
            .on_action(cx.listener(Workspace::on_focus_address))
            .on_action(cx.listener(Workspace::on_reload))
            .on_action(cx.listener(Workspace::on_back))
            .on_action(cx.listener(Workspace::on_forward))
            .on_action(cx.listener(Workspace::on_split_right))
            .on_action(cx.listener(Workspace::on_split_down))
            .on_action(cx.listener(Workspace::on_toggle_sidebar))
            .on_action(cx.listener(Workspace::on_next_tab))
            .on_action(cx.listener(Workspace::on_prev_tab))
            .on_action(cx.listener(Workspace::on_focus_left))
            .on_action(cx.listener(Workspace::on_focus_right))
            .on_action(cx.listener(Workspace::on_focus_up))
            .on_action(cx.listener(Workspace::on_focus_down))
            .on_action(cx.listener(Workspace::on_dev_tools))
            .on_action(cx.listener(Workspace::on_zoom_pane))
            .on_action(cx.listener(Workspace::on_rename_space))
            .on_action(cx.listener(Workspace::on_open_settings))
            .on_action(cx.listener(Workspace::on_open_palette))
            .on_action(cx.listener(Workspace::on_page_zoom_in))
            .on_action(cx.listener(Workspace::on_page_zoom_out))
            .on_action(cx.listener(Workspace::on_page_zoom_reset));

        self.sync_backdrop(vw, vh, window.scale_factor(), cx);
        let settings = &self.store.settings;
        let canvas = settings.background.solid().map(gpui::rgb).unwrap_or_else(theme::canvas);
        let backdrop = self.backdrop.as_ref();
        let sharp = backdrop.map(|b| b.sharp.clone());
        let blurred = backdrop.and_then(|b| b.blurred.clone());
        // The sidebar's tint over the background: opaque, or glass.
        let opacity = settings.chrome_opacity.clamp(0.3, 1.0);
        let tint = gpui::Rgba { a: opacity, ..theme::chrome() };
        // Its text, readable on what actually shows through: the tint over
        // the background's average under it.
        let under = match (settings.background.solid(), backdrop) {
            (Some(color), _) => gpui::rgb(color),
            (None, Some(b)) => crate::backdrop::average(&b.sharp, [inset / vw, inset / vh, (inset + side) / vw, (vh - inset) / vh])
                .map(gpui::rgb)
                .unwrap_or_else(theme::canvas),
            _ => theme::canvas(),
        };
        // Solid backgrounds hide nothing in an average; pictures do.
        let picture = if backdrop.is_some() { 1.0 - opacity } else { 0.0 };
        self.set_ink(theme::Ink::over(theme::mix(under, theme::chrome(), opacity), picture), cx);
        let ink = self.ink;
        let sidebar = div()
            .absolute()
            .left(px(inset))
            .top(px(inset))
            .bottom(px(inset))
            .w(px(side))
            .overflow_hidden()
            // The background, blurred, lined up with the window behind it.
            .when_some(blurred.filter(|_| opacity < 1.0), |s, image| {
                s.child(img(image).absolute().left(px(-inset)).top(px(-inset)).w(px(vw)).h(px(vh)).object_fit(ObjectFit::Fill))
            })
            .child(div().absolute().inset_0().bg(tint))
            .text_color(ink.text)
            .border_color(theme::border())
            .when(floating, |s| s.border_1().rounded(px(theme::PANE_RADIUS)))
            .when(!floating, |s| s.border_r_1())
            .child(if self.sidebar.collapsed {
                self.rail(vh - 2.0 * inset, cx).into_any_element()
            } else {
                self.sidebar(vh - 2.0 * inset, cx).into_any_element()
            });

        // The window: rounded corners while floating.
        let frame = div()
            .absolute()
            .inset_0()
            .rounded(px(radius))
            .bg(canvas)
            .overflow_hidden()
            // Sized outright: an image's own size is its device pixels, which
            // would win over insets (and overflow at scales above 1).
            .when_some(sharp, |f, image| f.child(img(image).absolute().left_0().top_0().w(px(vw)).h(px(vh)).object_fit(ObjectFit::Fill)))
            .child(sidebar)
            .children(self.render_panes(area, cx));

        let dragging = self.divider_drag.is_some() || self.pane_drag.is_some() || self.sidebar.drag.is_some();
        root.child(frame)
            .child(self.sidebar_resize_handle(edge, cx))
            .when(dragging, |r| r.child(self.drag_overlay(area, cx)))
            .when(self.palette.is_some(), |r| r.child(self.render_palette(area, cx)))
            .when(self.settings.is_some(), |r| r.child(self.settings_dialog(vw, vh, cx)))
            .children(chrome::resize_borders(window))
    }
}

/// What a page shows as its name: its title, else its host.
pub fn page_title(view: &WebView) -> String {
    if !view.title().is_empty() && view.title() != view.url() {
        view.title().to_string()
    } else if url::display(view.url()).is_empty() {
        "New Tab".to_string()
    } else {
        url::host(view.url()).to_string()
    }
}

fn tile_base(size: f32) -> Div {
    div().flex().flex_none().items_center().justify_center().size(px(size)).rounded(px(size / 4.0))
}

/// `text`'s initial on a color of its own (herdr's avatars), if it has one.
fn letter_tile(text: &str, size: f32) -> Option<Div> {
    let initial = text.chars().find(|c| c.is_alphanumeric())?;
    Some(
        tile_base(size)
            .bg(theme::tile(text))
            .text_size(px(size * 0.55))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(gpui::white())
            .child(initial.to_uppercase().to_string()),
    )
}

/// A site's tile: its favicon, or its initial on a color of its own.
pub(crate) fn site_tile(favicon: Option<Arc<RenderImage>>, url: &str, size: f32) -> Div {
    if let Some(favicon) = favicon {
        return tile_base(size).child(img(favicon).size(px((size * 0.8).round())));
    }
    let host = url::host(url);
    let host = host.strip_prefix("www.").unwrap_or(host);
    letter_tile(host, size).unwrap_or_else(|| tile_base(size).bg(theme::wash(0.06)).child(icon_glyph("icons/globe.svg", size * 0.65)))
}

/// A space's tile: a named space is its own thing (its name's initial, as
/// herdr's spaces); otherwise it shows its focused page's site.
fn space_tile(tab: &Tab, view: &WebView, size: f32) -> Div {
    tab.name.as_deref().and_then(|name| letter_tile(name, size)).unwrap_or_else(|| site_tile(view.favicon(), view.url(), size))
}

/// The row a dragged pane would become, shown while it is over the sidebar.
fn new_space_row() -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(px(theme::ROW_HEIGHT))
        .px(px(8.))
        .rounded(px(theme::CONTROL_RADIUS))
        .bg(theme::accent_soft())
        .border_1()
        .border_color(theme::accent())
        .child(tile_base(20.0).bg(theme::wash(0.08)).child(icon_glyph("icons/plus.svg", 13.0).text_color(theme::text())))
        .child("New space")
}

fn spinner(id: impl Into<gpui::ElementId>, size: f32, color: gpui::Rgba) -> impl IntoElement {
    icon_glyph("icons/loader.svg", size)
        .text_color(color)
        .with_animation(id, Animation::new(Duration::from_millis(900)).repeat(), |svg, delta| {
            svg.with_transformation(Transformation::rotate(percentage(delta)))
        })
}

impl Workspace {
    // ---- the full sidebar

    fn sidebar(&mut self, vh: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = self.ink;
        let view = self.focused_view().cloned();
        let (can_back, can_fwd, loading) = view
            .as_ref()
            .map(|v| {
                let v = v.read(cx);
                (v.can_go_back(), v.can_go_forward(), v.is_loading())
            })
            .unwrap_or_default();

        // One row: window controls, navigation, collapse.
        let header = chrome::window_drag(
            div().id("sidebar-header").flex().flex_none().items_center().h(px(theme::HEADER_HEIGHT)).pl(px(14.)).pr(px(8.)).gap(px(2.)),
        )
        .child(chrome::traffic_lights(false))
        .child(div().w(px(10.)))
        .child(
            icon_button("nav-back", "icons/arrow-left.svg", ink)
                .when(!can_back, |b| b.opacity(0.35))
                .on_click(cx.listener(|this, _, w, cx| this.on_back(&crate::workspace::Back, w, cx))),
        )
        .child(
            icon_button("nav-forward", "icons/arrow-right.svg", ink)
                .when(!can_fwd, |b| b.opacity(0.35))
                .on_click(cx.listener(|this, _, w, cx| this.on_forward(&crate::workspace::Forward, w, cx))),
        )
        .child(icon_button("nav-reload", if loading { "icons/x.svg" } else { "icons/rotate-cw.svg" }, ink).on_click(cx.listener(
            |this, _, _, cx| {
                if let Some(v) = this.focused_view().cloned() {
                    v.update(cx, |v, _| if v.is_loading() { v.stop() } else { v.reload() });
                }
            },
        )))
        .child(div().flex_1())
        .child(icon_button("sidebar-collapse", "icons/panel-left.svg", ink).on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))));

        let secure = view.as_ref().is_some_and(|v| v.read(cx).url().starts_with("https://"));
        let address = div()
            .id("address")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .mx(px(10.))
            .mb(px(6.))
            .h(px(32.))
            .px(px(10.))
            .rounded(px(8.))
            .bg(theme::wash(if self.address_focused { 0.07 } else { 0.04 }))
            .border_1()
            .border_color(if self.address_focused { theme::accent().into() } else { theme::border() })
            .child(icon_glyph(if secure { "icons/lock.svg" } else { "icons/search.svg" }, 13.0).text_color(ink.muted))
            .child(self.address.clone());

        // Spaces takes its share of the lists; Activity the rest.
        let lists = vh - theme::HEADER_HEIGHT - theme::ADDRESS_BLOCK - theme::FOOTER_HEIGHT;
        let spaces = div()
            .flex()
            .flex_col()
            .flex_none()
            .h(px(self.spaces_height(lists)))
            .child(section_header(
                "Spaces",
                Some(
                    icon_button("new-tab", "icons/plus.svg", ink)
                        .on_click(cx.listener(|this, _, w, cx| this.on_new_tab(&crate::workspace::NewTab, w, cx)))
                        .into_any_element(),
                ),
                ink,
            ))
            .child(
                div()
                    .id("spaces")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .gap(px(1.))
                    .children(self.space_rows(cx)),
            );

        let activity = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(theme::LIST_MIN))
            .border_t_1()
            .border_color(theme::border())
            .child(section_header("Activity", None, ink))
            .child(
                div()
                    .id("activity")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .children(self.activity_rows(cx)),
            )
            .child(self.lists_split(lists, cx));

        // At the bottom, as herdr has it.
        let footer = div()
            .flex_none()
            .h(px(theme::FOOTER_HEIGHT))
            .px(px(8.))
            .pt(px(4.))
            .pb(px(8.))
            .border_t_1()
            .border_color(theme::border())
            .child(
                div()
                    .id("open-settings")
                    .group("open-settings")
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .h(px(28.))
                    .px(px(8.))
                    .rounded(px(theme::CONTROL_RADIUS))
                    .text_color(ink.muted)
                    .hover(move |s| s.bg(ink.hover).text_color(ink.text))
                    .child(icon_glyph("icons/settings.svg", 14.0).text_color(ink.muted).group_hover("open-settings", move |s| s.text_color(ink.text)))
                    .child("Settings")
                    .on_click(cx.listener(|this, _, w, cx| this.on_open_settings(&crate::workspace::OpenSettings, w, cx))),
            );

        div().size_full().flex().flex_col().child(header).child(address).child(spaces).child(activity).child(footer)
    }

    fn space_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ink = self.ink;
        let mut rows = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            let count = tab.root.panes().len();
            let Some(view) = self.panes.get(&tab.focused).map(|p| p.view.read(cx)) else { continue };
            let renaming = self.renaming.as_ref().filter(|r| r.tab == tab.id).map(|r| r.input.clone());
            let name = self.space_name(tab, cx);
            let group: SharedString = format!("space-{}", tab.id).into();
            let row = div()
                .id(("space", tab.id))
                .group(group.clone())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .h(px(theme::ROW_HEIGHT))
                .pl(px(8.))
                .pr(px(4.))
                .rounded(px(theme::CONTROL_RADIUS))
                .when(active, |r| r.bg(ink.selected))
                .when(!active, |r| r.hover(move |s| s.bg(ink.hover)))
                .child(space_tile(tab, view, 20.0))
                .child(match renaming {
                    Some(input) => div()
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .h(px(24.))
                        .items_center()
                        .px(px(6.))
                        .ml(px(-6.))
                        .rounded(px(5.))
                        .bg(theme::wash(0.05))
                        .border_1()
                        .border_color(theme::accent())
                        .child(input),
                    None => div().flex_1().min_w_0().truncate().child(name),
                })
                .when(view.is_loading(), |r| r.child(spinner(("space-loading", tab.id), 12.0, theme::working())))
                .when(count > 1, |r| {
                    r.child(
                        div()
                            .flex_none()
                            .text_size(px(12.))
                            .text_color(ink.dim)
                            .group_hover(group.clone(), |s| s.invisible())
                            .child(format!("{count} panes")),
                    )
                })
                .child(
                    small_button(("close-space", tab.id), format!("close-space-{}", tab.id), "icons/x.svg", ink)
                        .invisible()
                        .group_hover(group.clone(), |s| s.visible())
                        .on_click(cx.listener(move |this, _, w, cx| this.close_tab(i, w, cx))),
                )
                .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, w, cx| this.close_tab(i, w, cx)))
                // Double-click (or F2) renames.
                .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, w, cx| {
                    if ev.click_count() >= 2 {
                        this.start_rename(i, w, cx);
                    } else {
                        this.activate(i, w, cx);
                    }
                }));
            rows.push(row.into_any_element());
        }
        if self.dropping_new_space() {
            rows.push(new_space_row().into_any_element());
        }
        rows
    }

    /// A pane is being dragged over the sidebar, to become a space.
    fn dropping_new_space(&self) -> bool {
        self.pane_drag.as_ref().is_some_and(|d| d.moved && d.target == Some(DropTarget::NewSpace))
    }

    fn activity_rows(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ink = self.ink;
        let items = activity::items(self, cx);
        // Not a page's: the browser's own (a downloaded update).
        let update = self.store.ublock_update.ready.clone();
        if items.is_empty() && update.is_none() {
            return vec![
                div()
                    .px(px(8.))
                    .pt(px(2.))
                    .text_size(px(12.))
                    .text_color(ink.dim)
                    .child("Pages that need you show up here")
                    .into_any_element(),
            ];
        }
        let mut rows = Vec::new();
        for kind in Kind::ALL {
            let group: Vec<_> = items.iter().filter(|i| i.kind == kind).collect();
            let notices = if kind == Kind::NeedsYou { update.iter().count() } else { 0 };
            if group.is_empty() && notices == 0 {
                continue;
            }
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(26.))
                    .px(px(8.))
                    .mt(px(4.))
                    .text_size(px(12.))
                    .child(div().text_color(kind_color(kind)).child(kind.label()))
                    .child(div().text_color(ink.dim).child((group.len() + notices).to_string()))
                    .into_any_element(),
            );
            if update.is_some() && kind == Kind::NeedsYou {
                rows.push(
                    div()
                        .id("activity-ublock-update")
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(10.))
                        .h(px(theme::ROW_HEIGHT))
                        .px(px(8.))
                        .rounded(px(theme::CONTROL_RADIUS))
                        .hover(move |s| s.bg(ink.hover))
                        .child(update_tile(20.0))
                        .child(div().flex_1().min_w_0().truncate().child("Ad blocker update"))
                        .child(
                            text_button("activity-ublock-restart", "Restart", false)
                                .h(px(22.))
                                .px(px(8.))
                                .on_click(|_, _, cx| crate::restart(cx)),
                        )
                        .on_click(cx.listener(|this, _, w, cx| {
                            this.open_settings(crate::workspace::SettingsSection::AdBlocking, w, cx)
                        }))
                        .into_any_element(),
                );
            }
            for item in group {
                let pane = item.pane;
                let Some(view) = self.panes.get(&pane).map(|p| p.view.read(cx)) else { continue };
                let meta: AnyElement = match kind {
                    Kind::NeedsYou => badge("!", theme::needs_you()).into_any_element(),
                    Kind::Live => div()
                        .flex()
                        .gap(px(4.))
                        .when(item.camera, |d| d.child(icon_glyph("icons/video.svg", 13.0).text_color(theme::live())))
                        .when(item.microphone, |d| d.child(icon_glyph("icons/mic.svg", 13.0).text_color(theme::live())))
                        .into_any_element(),
                    Kind::Media => media_controls("activity", pane, view.playback(), view.muted(), ink, cx).into_any_element(),
                    Kind::Unread => badge(&item.count.unwrap_or(0).to_string(), theme::unread()).into_any_element(),
                };
                rows.push(
                    div()
                        .id(("activity", pane))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(10.))
                        .h(px(theme::ROW_HEIGHT))
                        .pl(px(8.))
                        .pr(px(8.))
                        .rounded(px(theme::CONTROL_RADIUS))
                        .hover(move |s| s.bg(ink.hover))
                        .child(site_tile(view.favicon(), view.url(), 20.0))
                        .child(div().flex_1().min_w_0().truncate().child(page_title(view)))
                        .child(meta)
                        .on_click(cx.listener(move |this, _, w, cx| this.reveal_pane(pane, w, cx)))
                        .into_any_element(),
                );
            }
        }
        rows
    }

    /// The line between Spaces and Activity (in the sidebar and the rail) is a
    /// handle: drag it to share their height, `lists`, between them.
    fn lists_split(&mut self, lists: f32, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("lists-split")
            .absolute()
            .top(px(-4.))
            .left_0()
            .right_0()
            .h(px(7.))
            .cursor_row_resize()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.sidebar.drag = Some(SidebarDrag::Split { last: f32::from(ev.position.y), lists });
                    cx.notify();
                }),
            )
    }

    // ---- the rail (collapsed sidebar)

    fn rail(&mut self, vh: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let ink = self.ink;
        let top = chrome::window_drag(
            div().id("rail-top").flex().flex_col().flex_none().h(px(theme::RAIL_HEADER_HEIGHT)).items_center().pt(px(14.)).pb(px(8.)).gap(px(8.)),
        )
            .child(chrome::traffic_lights(true))
            .child(icon_button("rail-expand", "icons/panel-left.svg", ink).on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))));

        let mut spaces = Vec::new();
        for (i, tab) in self.tabs.iter().enumerate() {
            let Some(view) = self.panes.get(&tab.focused).map(|p| p.view.read(cx)) else { continue };
            spaces.push(
                rail_item(("rail-space", tab.id), i == self.active, space_tile(tab, view, 20.0), ink)
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, w, cx| this.close_tab(i, w, cx)))
                    .on_click(cx.listener(move |this, _, w, cx| this.activate(i, w, cx)))
                    .into_any_element(),
            );
        }

        let lists = vh - theme::RAIL_HEADER_HEIGHT - theme::FOOTER_HEIGHT;
        let mut activity = Vec::new();
        if self.store.ublock_update.ready.is_some() {
            activity.push(
                rail_item("rail-ublock-update", false, update_tile(20.0).relative().child(status_dot(Kind::NeedsYou)), ink)
                    .on_click(cx.listener(|this, _, w, cx| this.open_settings(crate::workspace::SettingsSection::AdBlocking, w, cx)))
                    .into_any_element(),
            );
        }
        for item in activity::items(self, cx) {
            let pane = item.pane;
            let Some(view) = self.panes.get(&pane).map(|p| p.view.read(cx)) else { continue };
            let tile = site_tile(view.favicon(), view.url(), 20.0).relative().child(status_dot(item.kind));
            activity.push(
                rail_item(("rail-activity", pane), false, tile, ink)
                    .on_click(cx.listener(move |this, _, w, cx| this.reveal_pane(pane, w, cx)))
                    .into_any_element(),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .child(top)
            .child(
                div()
                    .id("rail-spaces")
                    .flex()
                    .flex_col()
                    .items_center()
                    .flex_none()
                    .h(px(self.spaces_height(lists)))
                    .overflow_y_scroll()
                    .gap(px(2.))
                    .child(
                        icon_button("rail-new-tab", "icons/plus.svg", ink)
                            .on_click(cx.listener(|this, _, w, cx| this.on_new_tab(&crate::workspace::NewTab, w, cx))),
                    )
                    .children(spaces)
                    .when(self.dropping_new_space(), |list| {
                        list.child(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .justify_center()
                                .size(px(32.))
                                .rounded(px(theme::CONTROL_RADIUS))
                                .bg(theme::accent_soft())
                                .border_1()
                                .border_color(theme::accent())
                                .child(icon_glyph("icons/plus.svg", 14.0).text_color(ink.text)),
                        )
                    }),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .border_t_1()
                    .border_color(theme::border())
                    .child(
                        div()
                            .id("rail-activity")
                            .flex()
                            .flex_col()
                            .items_center()
                            .size_full()
                            .pt(px(6.))
                            .overflow_y_scroll()
                            .gap(px(2.))
                            .children(activity),
                    )
                    .child(self.lists_split(lists, cx)),
            )
            .child(
                div().flex_none().h(px(theme::FOOTER_HEIGHT)).w_full().flex().justify_center().pt(px(4.)).pb(px(8.)).border_t_1().border_color(theme::border()).child(
                    icon_button("rail-settings", "icons/settings.svg", ink)
                        .on_click(cx.listener(|this, _, w, cx| this.on_open_settings(&crate::workspace::OpenSettings, w, cx))),
                ),
            )
    }

    /// The sidebar's right edge: drag to resize; narrow enough, it
    /// collapses to the rail, and the rail widens back out.
    fn sidebar_resize_handle(&mut self, side: f32, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("sidebar-resize")
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(side - 3.0))
            .w(px(6.))
            .cursor_col_resize()
            // Over the header's drag area (see `chrome::window_drag`), the
            // handle, not the title bar, takes the press.
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.sidebar.drag = Some(SidebarDrag::Width { offset: f32::from(ev.position.x) - side });
                    cx.notify();
                }),
            )
    }

    // ---- panes

    fn render_panes(&mut self, area: Rect, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(tab) = self.tabs.get(self.active) else { return Vec::new() };
        let geometry = match tab.zoomed {
            Some(pane) => layout::Geometry { panes: vec![(pane, area)], dividers: Vec::new() },
            None => tab.root.geometry(area, theme::GAP),
        };
        let lone = geometry.panes.len() == 1;
        let (focused, zoomed) = (tab.focused, tab.zoomed);
        let mut out = Vec::new();
        for (pane, rect) in &geometry.panes {
            let Some(view) = self.panes.get(pane).map(|p| p.view.clone()) else { continue };
            out.push(self.pane_card(*pane, *rect, view, *pane == focused, lone, zoomed == Some(*pane), cx));
        }
        for (i, d) in geometry.dividers.iter().enumerate() {
            let divider = d.clone();
            let row = d.axis == Axis::Row;
            // The grab area: the gap, reaching a little into both panes.
            let (x, y, w, h) = if row {
                (d.rect.x - DIVIDER_REACH, d.rect.y, d.rect.w + 2.0 * DIVIDER_REACH, d.rect.h)
            } else {
                (d.rect.x, d.rect.y - DIVIDER_REACH, d.rect.w, d.rect.h + 2.0 * DIVIDER_REACH)
            };
            let el = div()
                .id(("divider", i))
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(w))
                .h(px(h))
                .when(row, |el| el.cursor_col_resize())
                .when(!row, |el| el.cursor_row_resize())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if ev.click_count >= 2 {
                            if let Some(tab) = this.tabs.get_mut(this.active) {
                                tab.root.equalize(&divider.path);
                            }
                        } else {
                            this.divider_drag = Some(DividerDrag {
                                path: divider.path.clone(),
                                index: divider.index,
                                axis: divider.axis,
                                span: divider.span,
                                last: ev.position,
                            });
                        }
                        cx.notify();
                    }),
                );
            out.push(el.into_any_element());
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn pane_card(
        &mut self,
        pane: PaneId,
        rect: Rect,
        view: Entity<WebView>,
        focused: bool,
        lone: bool,
        zoomed: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let v = view.read(cx);
        let title = page_title(v);
        let host = url::host(v.url()).to_string();
        let favicon = v.favicon();
        let page_url = v.url().to_string();
        let loading = v.is_loading();
        let (camera, microphone) = v.media();
        let (playback, muted) = (v.playback(), v.muted());
        let asks = v.ask().is_some() || v.crashed();
        let zoom = v.zoom_percent();
        let header_group: SharedString = format!("pane-header-{pane}").into();
        let button_group = |name: &str| format!("pane-{pane}-{name}");

        let controls = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .invisible()
            .group_hover(header_group.clone(), |s| s.visible())
            .child(
                small_button(("pane-split-right", pane), button_group("split-right"), "icons/columns-2.svg", Ink::standard())
                    .on_click(cx.listener(move |this, _, w, cx| this.split_pane(pane, Edge::Right, w, cx))),
            )
            .child(
                small_button(("pane-split-down", pane), button_group("split-down"), "icons/rows-2.svg", Ink::standard())
                    .on_click(cx.listener(move |this, _, w, cx| this.split_pane(pane, Edge::Bottom, w, cx))),
            )
            .child(
                small_button(("pane-zoom", pane), button_group("zoom"), if zoomed { "icons/minimize-2.svg" } else { "icons/maximize-2.svg" }, Ink::standard())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_zoom(pane, cx))),
            )
            .child(
                small_button(("pane-close", pane), button_group("close"), "icons/x.svg", Ink::standard())
                    .on_click(cx.listener(move |this, _, w, cx| this.close_pane(pane, w, cx))),
            );

        let header = div()
            .id(("pane-header", pane))
            .group(header_group.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(theme::PANE_HEADER_HEIGHT))
            .pl(px(10.))
            .pr(px(5.))
            .border_b_1()
            .border_color(theme::border())
            .text_size(px(12.))
            .child(site_tile(favicon, &page_url, 14.0))
            .child(div().flex_none().max_w(px(260.)).truncate().font_weight(FontWeight::MEDIUM).child(title))
            .child(div().flex_1().min_w_0().truncate().text_color(theme::text_dim()).child(host))
            .when(loading, |h| h.child(spinner(("pane-loading", pane), 12.0, theme::working())))
            .when(camera, |h| h.child(icon_glyph("icons/video.svg", 13.0).text_color(theme::live())))
            .when(microphone, |h| h.child(icon_glyph("icons/mic.svg", 13.0).text_color(theme::live())))
            .when(playback != Playback::None, |h| h.child(media_controls("pane", pane, playback, muted, Ink::standard(), cx)))
            .when(asks, |h| h.child(pill("Needs you", theme::needs_you())))
            // Zoomed: the size shows; a click goes back to 100%.
            .when(zoom != 100, |h| {
                h.child(
                    div()
                        .id(("pane-zoom-level", pane))
                        .flex_none()
                        .px(px(5.))
                        .rounded(px(4.))
                        .text_size(px(11.))
                        .text_color(theme::text_muted())
                        .bg(theme::wash(0.06))
                        .hover(|s| s.bg(theme::wash(0.1)).text_color(theme::text()))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| this.zoom_page(Some(pane), web::Zoom::Reset, cx)))
                        .child(format!("{zoom}%")),
                )
            })
            .child(controls)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, w, cx| {
                    this.focus_pane(pane, w, cx);
                    if ev.click_count >= 2 {
                        this.toggle_zoom(pane, cx);
                    } else if !lone {
                        // A drag once it moves far enough; a click otherwise.
                        this.pane_drag = Some(PaneDrag { pane, origin: ev.position, moved: false, target: None });
                    }
                }),
            );

        // Drawn over the page so the page's square pixels can't cover it.
        let ring = div()
            .absolute()
            .inset_0()
            .rounded(px(theme::PANE_RADIUS))
            .border_1()
            .border_color(if focused && !lone { theme::accent().into() } else { theme::border() });

        let drop = self.pane_drag.as_ref().filter(|d| d.moved && d.pane != pane).and_then(|d| match d.target {
            Some(DropTarget::Pane(target, edge)) if target == pane => Some(edge),
            _ => None,
        });
        // Where the split the palette asks about would go.
        let drop = drop.or_else(|| match self.palette.as_ref().map(|p| p.intent) {
            Some(Intent::Split { target, edge }) if target == pane => Some(Some(edge)),
            _ => None,
        });

        div()
            .id(("pane", pane))
            .absolute()
            .left(px(rect.x))
            .top(px(rect.y))
            .w(px(rect.w))
            .h(px(rect.h))
            .flex()
            .flex_col()
            .rounded(px(theme::PANE_RADIUS))
            .bg(theme::surface())
            .overflow_hidden()
            .child(header)
            // Inside the card's border, which frames the page, not covers it.
            .child(div().flex_1().min_h_0().mx(px(1.)).mb(px(1.)).child(view))
            .when(asks, |card| card.child(self.question(pane, rect, cx)))
            .child(ring)
            .when_some(drop, |card, edge| card.child(drop_highlight(rect, edge)))
            .into_any_element()
    }

    /// The question a pane's page asks (or its crash), over the page.
    fn question(&mut self, pane: PaneId, rect: Rect, cx: &mut Context<Self>) -> impl IntoElement {
        let p = &self.panes[&pane];
        let view = p.view.read(cx);
        let host = match url::host(view.url()) {
            "" => "This page".to_string(),
            host => host.to_string(),
        };
        let crashed = view.crashed() && view.ask().is_none();
        let (title, message, cancel, accept): (String, Option<String>, Option<&str>, &str) = match view.ask() {
            Some(Ask::Alert { message }) => (format!("{host} says"), Some(message.clone()), None, "OK"),
            Some(Ask::Confirm { message }) => (format!("{host} says"), Some(message.clone()), Some("Cancel"), "OK"),
            Some(Ask::Prompt { message, .. }) => (format!("{host} asks"), Some(message.clone()), Some("Cancel"), "OK"),
            Some(Ask::Leave { reload }) => (
                if *reload { "Reload site?" } else { "Leave site?" }.to_string(),
                Some("Changes you made may not be saved.".to_string()),
                Some("Stay"),
                if *reload { "Reload" } else { "Leave" },
            ),
            Some(Ask::Permission { origin, what }) => {
                (format!("{} wants to use", url::host(origin)), Some(join_words(what)), Some("Block"), "Allow")
            }
            None => ("This page crashed".to_string(), Some(format!("{host} stopped unexpectedly.")), None, "Reload"),
        };
        let prompt = p.prompt.as_ref().map(|(input, _)| input.clone());
        // Dialogs hold the page; a permission request leaves it usable.
        let modal = !matches!(view.ask(), Some(Ask::Permission { .. }));

        let card = div()
            .id(("question", pane))
            .w(px(380f32.min(rect.w - 32.0).max(160.0)))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(16.))
            .rounded(px(10.))
            .bg(theme::popover())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .occlude()
            .child(div().font_weight(FontWeight::MEDIUM).child(title))
            .when_some(message.filter(|m| !m.is_empty()), |c, m| c.child(div().text_color(theme::text_muted()).child(m)))
            .when_some(prompt, |c, input| {
                c.child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(30.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .bg(theme::wash(0.05))
                        .border_1()
                        .border_color(theme::accent())
                        .child(input),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .mt(px(4.))
                    .when_some(cancel, |row, label| {
                        row.child(
                            text_button(("question-cancel", pane), label, false)
                                .on_click(cx.listener(move |this, _, w, cx| this.answer(pane, false, None, w, cx))),
                        )
                    })
                    .child(text_button(("question-accept", pane), accept, true).on_click(cx.listener(move |this, _, w, cx| {
                        if crashed {
                            if let Some(p) = this.panes.get(&pane) {
                                p.view.update(cx, |v, _| v.reload());
                            }
                        } else {
                            this.answer(pane, true, None, w, cx);
                        }
                    }))),
            );

        div()
            .absolute()
            .top(px(theme::PANE_HEADER_HEIGHT))
            .left_0()
            .right_0()
            .bottom_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(24.))
            .when(modal, |d| d.bg(gpui::black().opacity(0.35)).occlude())
            .child(card)
    }

    /// While dragging, a layer over everything takes the pointer so pages
    /// don't see the drag.
    fn drag_overlay(&mut self, area: Rect, cx: &mut Context<Self>) -> impl IntoElement {
        let cursor_row = matches!(self.divider_drag, Some(DividerDrag { axis: Axis::Column, .. }));
        let (cursor_row, cursor_col) = match &self.sidebar.drag {
            Some(SidebarDrag::Split { .. }) => (true, false),
            Some(SidebarDrag::Width { .. }) => (false, true),
            None => (self.divider_drag.is_some() && cursor_row, self.divider_drag.is_some() && !cursor_row),
        };
        div()
            .id("drag-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .when(cursor_row, |el| el.cursor_row_resize())
            .when(cursor_col, |el| el.cursor_col_resize())
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| this.drag_move(ev.position, area, cx)))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, w, cx| this.drag_end(ev.position, w, cx)))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, w, cx| this.drag_end(ev.position, w, cx)))
    }

    fn drag_move(&mut self, pos: gpui::Point<Pixels>, area: Rect, cx: &mut Context<Self>) {
        match self.sidebar.drag {
            Some(SidebarDrag::Width { offset }) => {
                return self.drag_sidebar_edge(f32::from(pos.x) - offset - self.sidebar_inset(), cx);
            }
            Some(SidebarDrag::Split { last, lists }) => {
                let y = f32::from(pos.y);
                self.sidebar.drag = Some(SidebarDrag::Split { last: y, lists });
                return self.drag_sidebar_split(y - last, lists, cx);
            }
            None => {}
        }
        if let Some(drag) = self.divider_drag.as_mut() {
            let delta = match drag.axis {
                Axis::Row => f32::from(pos.x - drag.last.x),
                Axis::Column => f32::from(pos.y - drag.last.y),
            };
            drag.last = pos;
            let (path, index, span) = (drag.path.clone(), drag.index, drag.span);
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.root.resize(&path, index, delta, span);
            }
            cx.notify();
            return;
        }
        if let Some(drag) = self.pane_drag.as_mut() {
            let (dx, dy) = (f32::from(pos.x - drag.origin.x), f32::from(pos.y - drag.origin.y));
            if !drag.moved && dx.hypot(dy) < DRAG_THRESHOLD {
                return;
            }
            drag.moved = true;
            let (x, y) = (f32::from(pos.x), f32::from(pos.y));
            let target = if x < self.sidebar_inset() + self.sidebar_width() {
                Some(DropTarget::NewSpace)
            } else {
                self.tabs.get(self.active).and_then(|tab| {
                    tab.root
                        .geometry(area, theme::GAP)
                        .panes
                        .iter()
                        .find_map(|(p, r)| layout::drop_zone(*r, x, y).map(|zone| DropTarget::Pane(*p, zone)))
                })
            };
            if let Some(drag) = self.pane_drag.as_mut() {
                drag.target = target;
            }
            cx.notify();
        }
    }

    fn drag_end(&mut self, _pos: gpui::Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.drag = None;
        self.divider_drag = None;
        if let Some(drag) = self.pane_drag.take().filter(|d| d.moved) {
            match drag.target {
                Some(DropTarget::Pane(target, edge)) if target != drag.pane => {
                    self.move_pane(drag.pane, target, edge, cx);
                    self.focus_pane(drag.pane, window, cx);
                }
                Some(DropTarget::NewSpace) => self.pane_to_new_space(drag.pane, window, cx),
                _ => {}
            }
        }
        cx.notify();
    }
}

fn section_header(label: &'static str, action: Option<AnyElement>, ink: Ink) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .h(px(32.))
        .pl(px(16.))
        .pr(px(8.))
        .text_size(px(12.5))
        .text_color(ink.dim)
        .child(label)
        .children(action)
}

fn rail_item(id: impl Into<gpui::ElementId>, active: bool, tile: Div, ink: Ink) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(32.))
        .rounded(px(theme::CONTROL_RADIUS))
        .when(active, |r| r.bg(ink.selected))
        .when(!active, |r| r.hover(move |s| s.bg(ink.hover)))
        .child(tile)
}


/// Play/pause and mute for a page's media. `place` tells apart where
/// they show (Activity, the pane's header).
fn media_controls(place: &'static str, pane: PaneId, playback: Playback, muted: bool, ink: Ink, cx: &Context<Workspace>) -> impl IntoElement {
    let id = |what: &str| SharedString::from(format!("{place}-{what}-{pane}"));
    let playing = playback == Playback::Playing;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .child(
            small_button(id("play"), id("play"), if playing { "icons/pause.svg" } else { "icons/play.svg" }, ink)
                .on_click(cx.listener(move |this, _, _, cx| this.play_pause(pane, cx))),
        )
        .child(
            small_button(id("mute"), id("mute"), if muted { "icons/volume-x.svg" } else { "icons/volume-2.svg" }, ink)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_mute(pane, cx))),
        )
}

/// The corner dot on a rail tile: why it is listed.
fn status_dot(kind: Kind) -> Div {
    div()
        .absolute()
        .right(px(-3.))
        .bottom(px(-3.))
        .size(px(9.))
        .rounded_full()
        .border_2()
        .border_color(theme::chrome())
        .bg(kind_color(kind))
}

/// The ad blocker's tile (its updates in Activity).
fn update_tile(size: f32) -> Div {
    tile_base(size).bg(theme::wash(0.06)).child(icon_glyph("icons/shield.svg", size * 0.65).text_color(theme::needs_you()))
}

fn kind_color(kind: Kind) -> gpui::Rgba {
    match kind {
        Kind::NeedsYou => theme::needs_you(),
        Kind::Live => theme::live(),
        Kind::Media => theme::playing(),
        Kind::Unread => theme::unread(),
    }
}

/// A small count or mark on a tinted square, as herdr marks agents.
fn badge(text: &str, color: gpui::Rgba) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(16.))
        .h(px(16.))
        .px(px(4.))
        .rounded(px(4.))
        .bg(gpui::Rgba { a: 0.18, ..color })
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(text.to_string())
}

fn pill(text: &'static str, color: gpui::Rgba) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(18.))
        .px(px(6.))
        .rounded(px(5.))
        .bg(gpui::Rgba { a: 0.15, ..color })
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(text)
}

pub(crate) fn text_button(id: impl Into<gpui::ElementId>, label: &str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(28.))
        .px(px(12.))
        .rounded(px(6.))
        .text_size(px(12.5))
        .font_weight(FontWeight::MEDIUM)
        .when(primary, |b| b.bg(theme::accent()).text_color(gpui::white()).hover(|s| s.opacity(0.9)))
        .when(!primary, |b| b.bg(theme::wash(0.06)).hover(|s| s.bg(theme::wash(0.1))))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(label.to_string())
}

/// "camera", "camera and microphone", "a, b and c".
fn join_words(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Where a dropped pane would land: half the target on an edge, all of it
/// (a swap) in the middle.
fn drop_highlight(rect: Rect, edge: Option<Edge>) -> impl IntoElement {
    let (w, h) = (rect.w, rect.h);
    let (x, y, dw, dh) = match edge {
        Some(Edge::Left) => (0.0, 0.0, w / 2.0, h),
        Some(Edge::Right) => (w / 2.0, 0.0, w / 2.0, h),
        Some(Edge::Top) => (0.0, 0.0, w, h / 2.0),
        Some(Edge::Bottom) => (0.0, h / 2.0, w, h / 2.0),
        None => (0.0, 0.0, w, h),
    };
    div()
        .absolute()
        .left(px(x + 4.0))
        .top(px(y + 4.0))
        .w(px((dw - 8.0).max(0.0)))
        .h(px((dh - 8.0).max(0.0)))
        .rounded(px(theme::PANE_RADIUS - 2.0))
        .bg(theme::accent_soft())
        .border_2()
        .border_color(theme::accent())
}

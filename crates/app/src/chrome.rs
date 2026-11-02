//! Window chrome we draw ourselves: the traffic lights (the only window
//! controls), icon buttons, window dragging and edge resizing.

use crate::theme::{self, Ink};
use gpui::{prelude::FluentBuilder as _,
    AnyElement, App, ClickEvent, Decorations, Div, ElementId, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, ResizeEdge, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, px, svg,
};

/// Which edges are free to resize from and whether the window floats
/// (nothing tiled or maximized), which is when it gets rounded corners.
pub fn free_edges(window: &Window) -> (bool, bool, bool, bool) {
    match window.window_decorations() {
        Decorations::Client { tiling } => (!tiling.top, !tiling.bottom, !tiling.left, !tiling.right),
        Decorations::Server => (false, false, false, false),
    }
}

pub fn corner_radius(window: &Window) -> f32 {
    let (t, b, l, r) = free_edges(window);
    if t && b && l && r && !window.is_maximized() { theme::WINDOW_RADIUS } else { 0.0 }
}

/// Invisible strips along the free edges that start a compositor resize.
pub fn resize_borders(window: &Window) -> Vec<AnyElement> {
    if window.is_maximized() {
        return Vec::new();
    }
    let (top, bottom, left, right) = free_edges(window);
    const EDGE: f32 = 6.0;
    const CORNER: f32 = 14.0;
    let strip = |el: Div, edge: ResizeEdge| -> AnyElement {
        el.occlude()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                cx.stop_propagation();
                window.start_window_resize(edge);
            })
            .into_any_element()
    };
    let mut out = Vec::new();
    if top {
        out.push(strip(div().absolute().top_0().left(px(CORNER)).right(px(CORNER)).h(px(EDGE)).cursor_ns_resize(), ResizeEdge::Top));
    }
    if bottom {
        out.push(strip(
            div().absolute().bottom_0().left(px(CORNER)).right(px(CORNER)).h(px(EDGE)).cursor_ns_resize(),
            ResizeEdge::Bottom,
        ));
    }
    if left {
        out.push(strip(div().absolute().left_0().top(px(CORNER)).bottom(px(CORNER)).w(px(EDGE)).cursor_ew_resize(), ResizeEdge::Left));
    }
    if right {
        out.push(strip(
            div().absolute().right_0().top(px(CORNER)).bottom(px(CORNER)).w(px(EDGE)).cursor_ew_resize(),
            ResizeEdge::Right,
        ));
    }
    if top && left {
        out.push(strip(div().absolute().top_0().left_0().size(px(CORNER)).cursor_nwse_resize(), ResizeEdge::TopLeft));
    }
    if top && right {
        out.push(strip(div().absolute().top_0().right_0().size(px(CORNER)).cursor_nesw_resize(), ResizeEdge::TopRight));
    }
    if bottom && left {
        out.push(strip(div().absolute().bottom_0().left_0().size(px(CORNER)).cursor_nesw_resize(), ResizeEdge::BottomLeft));
    }
    if bottom && right {
        out.push(strip(div().absolute().bottom_0().right_0().size(px(CORNER)).cursor_nwse_resize(), ResizeEdge::BottomRight));
    }
    out
}

/// Makes empty chrome move the window when dragged and maximize it on a
/// double click. Controls inside stop the press themselves.
pub fn window_drag(el: gpui::Stateful<Div>) -> gpui::Stateful<Div> {
    // Windows: the system moves it (and snaps it, and maximizes it on a
    // double click), as a title bar. Buttons inside must occlude it.
    el.window_control_area(gpui::WindowControlArea::Drag)
        // Elsewhere, the app starts the move.
        .on_mouse_down(MouseButton::Left, |ev, window, _cx| {
            if ev.click_count >= 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
}

/// Close, minimize, maximize: macOS-style lights that show their glyphs
/// when the group is hovered. Stacked in the collapsed sidebar's rail.
pub fn traffic_lights(vertical: bool) -> impl IntoElement {
    let light = |id: &'static str, color: gpui::Rgba, glyph: &'static str, action: fn(&mut Window, &mut App)| {
        div()
            .id(id)
            .size(px(12.))
            .rounded_full()
            .bg(color)
            .flex()
            .items_center()
            .justify_center()
            .child(
                svg()
                    .path(glyph)
                    .size(px(8.))
                    .text_color(gpui::black().opacity(0.6))
                    .invisible()
                    .group_hover("lights", |s| s.visible()),
            )
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(move |_: &ClickEvent, window, cx| action(window, cx))
    };
    div()
        .id("traffic-lights")
        .group("lights")
        // Not part of a drag area around it (see `window_drag`).
        .occlude()
        .flex()
        .when(vertical, |d| d.flex_col())
        .items_center()
        .gap(px(8.))
        .when(!vertical, |d| d.px(px(4.)))
        .child(light("close", theme::close_red(), "icons/x.svg", |window, _| window.remove_window()))
        .child(light("minimize", theme::minimize_yellow(), "icons/minus.svg", |window, _| window.minimize_window()))
        .child(light("maximize", theme::maximize_green(), "icons/plus.svg", |window, _| window.zoom_window()))
}

/// A square icon button in the chrome's style. `id` also names its hover
/// group: the glyph brightens with the button.
pub fn icon_button(id: &'static str, icon: &'static str, ink: Ink) -> gpui::Stateful<Div> {
    button(id.into(), id.into(), icon, 28.0, 16.0, ink)
}

/// The small buttons in a pane's header (and on sidebar rows).
pub fn small_button(id: impl Into<ElementId>, group: impl Into<SharedString>, icon: &'static str, ink: Ink) -> gpui::Stateful<Div> {
    button(id.into(), group.into(), icon, 20.0, 14.0, ink)
}

fn button(id: ElementId, group: SharedString, icon: &'static str, size: f32, glyph: f32, ink: Ink) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .group(group.clone())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(px(if size > 24.0 { theme::CONTROL_RADIUS } else { 4.0 }))
        // Header buttons sit in drag areas (see `window_drag`).
        .when(size > 24.0, |b| b.occlude())
        .hover(move |s| s.bg(ink.hover))
        .active(move |s| s.bg(ink.selected))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(icon_glyph(icon, glyph).text_color(ink.muted).group_hover(group, move |s| s.text_color(ink.text)))
}

pub fn icon_glyph(icon: &'static str, size: f32) -> gpui::Svg {
    svg().path(icon).flex_none().size(px(size)).text_color(theme::text_muted())
}

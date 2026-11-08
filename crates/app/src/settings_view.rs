//! The settings dialog (herdr's: sections on the left, a page of settings
//! on the right, each applied the moment it changes).

use crate::chrome::icon_glyph;
use crate::render::text_button;
use crate::theme;
use crate::workspace::{SettingsSection, Workspace};
use gpui::{
    AnyElement, Context, FontWeight, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};

impl Workspace {
    /// The dialog over a dimmed window; a click outside or Esc closes it.
    pub fn settings_dialog(&mut self, vw: f32, vh: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(dialog) = self.settings.as_ref() else { return div() };
        let (section, focus) = (dialog.section, dialog.focus.clone());

        let nav = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(190.))
            .h_full()
            .p(px(10.))
            .gap(px(2.))
            .border_r_1()
            .border_color(theme::border())
            .child(div().px(px(10.)).pt(px(6.)).pb(px(12.)).text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .children(SettingsSection::ALL.map(|s| {
                let on = s == section;
                div()
                    .id(SharedString::from(format!("settings-section-{}", s.title())))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(7.))
                    .when(on, |d| d.bg(theme::selected()))
                    .hover(|d| d.bg(theme::selected()))
                    .child(icon_glyph(s.icon(), 15.0).text_color(if on { theme::accent() } else { theme::text_dim() }))
                    .child(
                        div().flex_1().text_size(px(13.5)).text_color(if on { theme::text() } else { theme::text_muted() }).child(s.title()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(d) = this.settings.as_mut() {
                            d.section = s;
                        }
                        cx.notify();
                    }))
            }));

        let page = match section {
            SettingsSection::Look => self.look_page(cx),
            SettingsSection::Tiling => self.tiling_page(cx),
            SettingsSection::AdBlocking => self.ad_blocking_page(cx),
        };

        let card = div()
            .id("settings")
            .track_focus(&focus)
            .flex()
            .w(px(theme::SETTINGS_WIDTH.min(vw - 48.0).max(320.0)))
            .h(px(theme::SETTINGS_HEIGHT.min(vh - 48.0).max(240.0)))
            .bg(theme::popover())
            .border_1()
            .border_color(theme::border())
            .rounded(px(8.))
            .shadow_lg()
            .overflow_hidden()
            .occlude()
            // Clicks inside stay inside; outside, they close it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                if ev.keystroke.key == "escape" {
                    cx.stop_propagation();
                    this.close_settings(w, cx);
                }
            }))
            .child(nav)
            .child(div().id("settings-page").flex_1().min_w_0().h_full().overflow_y_scroll().px(px(28.)).py(px(22.)).child(page));

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::black().opacity(0.35))
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, w, cx| this.close_settings(w, cx)))
            .child(card)
            .children(self.slider_overlay(cx))
    }

    fn tiling_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let spiral = self.store.settings.tiling_spiral;
        let spiral_toggle = toggle("settings-tiling-spiral", spiral, true).on_click(cx.listener(|this, _, _, cx| {
            this.store.set(|s| s.tiling_spiral = !s.tiling_spiral);
            cx.notify();
        }));
        div()
            .flex()
            .flex_col()
            .child(heading("Tiling"))
            .child(row(
                "Open links as tiles",
                Some(
                    "Middle-click and Ctrl+click open a link beside the page, in a spiral: right, then below, then left, \
                     then above, turning inward. Hold Shift for a space instead. Off, they open as spaces, and Shift makes \
                     a tile."
                        .to_string(),
                ),
                false,
                spiral_toggle.into_any_element(),
            ))
            .child(row(
                "New tile",
                Some(
                    "Ctrl+Alt+Enter opens a page beside this one: in the spiral when it's on, otherwise across the \
                     pane's longer side. Too small to split, it opens as a space."
                        .to_string(),
                ),
                false,
                div().into_any_element(),
            ))
            .child(row(
                "Swap panes",
                Some("Hold Shift while dragging a pane by its header to swap it with the one it's dropped on.".to_string()),
                false,
                div().into_any_element(),
            ))
            .into_any_element()
    }

    fn ad_blocking_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let store = &self.store;
        let on = store.settings.ad_blocking;
        let about = if store.ublock_missing {
            "uBlock Origin Lite isn't installed (extensions/ubol is missing from this build).".to_string()
        } else if store.ad_blocking_pending() {
            format!("Restart Gate to turn it {}.", if on { "on" } else { "off" })
        } else {
            "uBlock Origin Lite blocks ads, trackers and annoyances, YouTube's included.".to_string()
        };
        let blocking = toggle("settings-ad-blocking", on, !store.ublock_missing).on_click(cx.listener(|this, _, _, cx| {
            if !this.store.ublock_missing {
                this.store.set(|s| s.ad_blocking = !s.ad_blocking);
                cx.notify();
            }
        }));
        let dashboard = store.ublock_dashboard();

        // The installed version, and whether a newer one waits.
        let update = &store.ublock_update;
        let version_row = store.ublock_version.clone().map(|version| {
            let (about, notice) = if let Some(next) = &update.ready {
                (format!("Version {next} is downloaded. Restart Gate to use it."), true)
            } else if update.checking {
                ("Checking for a newer version\u{2026}".to_string(), false)
            } else if let Some(error) = &update.error {
                (format!("Couldn't check for updates: {error}"), true)
            } else {
                (checked_ago(store.ublock_paths.last_checked()), false)
            };
            let control = if update.ready.is_some() {
                text_button("settings-ublock-restart", "Restart now", true).on_click(|_, _, cx| crate::restart(cx))
            } else {
                text_button("settings-ublock-check", "Check now", false)
                    .when(update.checking, |b| b.opacity(0.5))
                    .on_click(cx.listener(|this, _, _, cx| this.check_ublock(cx)))
            };
            row(format!("uBlock Origin Lite {version}"), Some(about), notice, control.into_any_element())
        });
        let auto = store.settings.ublock_auto_update;
        let auto_toggle = toggle("settings-ublock-auto", auto, !store.ublock_missing).on_click(cx.listener(|this, _, _, cx| {
            this.store.set(|s| s.ublock_auto_update = !s.ublock_auto_update);
            cx.notify();
        }));

        div()
            .flex()
            .flex_col()
            .child(heading("Ad blocking"))
            .child(row("Block ads and trackers", Some(about), store.ad_blocking_pending(), blocking.into_any_element()))
            .children(version_row)
            .when(!store.ublock_missing, |page| {
                page.child(row(
                    "Update automatically",
                    Some("Looks for a new release once a day; it applies the next time Gate starts.".to_string()),
                    false,
                    auto_toggle.into_any_element(),
                ))
            })
            .when_some(dashboard, |page, url| {
                let open = text_button("settings-ublock-dashboard", "Open", false)
                    .gap(px(6.))
                    .child(icon_glyph("icons/external-link.svg", 12.0))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.close_settings(w, cx);
                        this.open_tab(&url, true, w, cx);
                    }));
                page.child(row(
                    "Filters and exceptions",
                    Some("Filtering mode, filter lists, and sites to leave alone, in uBlock Origin Lite's dashboard.".to_string()),
                    false,
                    open.into_any_element(),
                ))
            })
            .into_any_element()
    }
}

/// "Checked 3 hours ago", from the last check's time.
fn checked_ago(at: Option<std::time::SystemTime>) -> String {
    let Some(age) = at.and_then(|t| t.elapsed().ok()) else { return "Not checked for updates yet.".to_string() };
    let minutes = age.as_secs() / 60;
    let ago = match minutes {
        0 => "just now".to_string(),
        1..60 => format!("{minutes} min ago"),
        60..1440 => format!("{} h ago", minutes / 60),
        _ => format!("{} days ago", minutes / 1440),
    };
    format!("Up to date. Checked {ago}.")
}

pub(crate) fn heading(text: &str) -> impl IntoElement {
    div().pt(px(6.)).pb(px(8.)).text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::text_dim()).child(text.to_uppercase())
}

/// One setting: what it is on the left, its control on the right.
/// `notice` draws the explanation in the accent (something to act on).
pub(crate) fn row(title: impl Into<SharedString>, about: Option<String>, notice: bool, control: AnyElement) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .w_full()
        .gap(px(16.))
        .py(px(12.))
        .border_b_1()
        .border_color(theme::border())
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(3.))
                .child(div().text_size(px(14.)).child(title.into()))
                .when_some(about, |d, about| {
                    d.child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(18.))
                            .text_color(if notice { theme::accent() } else { theme::text_dim() })
                            .child(about),
                    )
                }),
        )
        .child(div().flex_none().child(control))
}

/// A switch; `enabled` false greys it out.
pub(crate) fn toggle(id: &'static str, on: bool, enabled: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded(px(10.))
        .bg(if on { theme::accent().into() } else { theme::hover() })
        .when(on, |d| d.justify_end())
        .when(!enabled, |d| d.opacity(0.4))
        .child(div().size(px(16.)).rounded(px(8.)).bg(if on { gpui::white() } else { theme::text_muted().into() }))
}

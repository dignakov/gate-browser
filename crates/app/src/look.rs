//! Settings › Look: the background (solid, gradient, wallpaper), how see-
//! through the sidebar is, and the glass's blur.

use crate::backdrop::{self, Background, PRESETS, SWATCHES};
use crate::settings::Settings;
use crate::settings_view::{heading, row, toggle};
use crate::text_input::{InputEvent, TextInput};
use crate::theme;
use crate::workspace::Workspace;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, Entity, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    MouseDownEvent, MouseMoveEvent, ParentElement as _, Pixels, RenderImage, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, Window, canvas, div, img, prelude::FluentBuilder as _, px,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

/// A setting set by dragging.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slider {
    Opacity,
    Blur,
    Angle,
}

impl Slider {
    fn range(self) -> (f32, f32) {
        match self {
            Self::Opacity => (0.3, 1.0),
            Self::Blur => (0.0, 80.0),
            Self::Angle => (0.0, 360.0),
        }
    }

    fn get(self, s: &Settings) -> f32 {
        match self {
            Self::Opacity => s.chrome_opacity,
            Self::Blur => s.blur,
            Self::Angle => match &s.background {
                Background::Gradient { angle, .. } => *angle,
                _ => 0.0,
            },
        }
    }

    fn set(self, s: &mut Settings, value: f32) {
        match self {
            Self::Opacity => s.chrome_opacity = value,
            Self::Blur => s.blur = value.round(),
            Self::Angle => {
                if let Background::Gradient { angle, .. } = &mut s.background {
                    *angle = value.round();
                }
            }
        }
    }

    fn label(self, value: f32) -> String {
        match self {
            Self::Opacity => format!("{:.0}%", value * 100.0),
            Self::Blur => format!("{value:.0} px"),
            Self::Angle => format!("{value:.0}°"),
        }
    }
}

/// The Look page's fields and the sliders' places, while Settings is open.
pub struct LookUi {
    colors: [Entity<TextInput>; 3],
    folder: Entity<TextInput>,
    sliders: Rc<RefCell<HashMap<Slider, Bounds<Pixels>>>>,
    _events: Vec<Subscription>,
}

const SLIDER_W: f32 = 180.0;
/// The slider knob's radius.
const KNOB_R: f32 = 7.0;
const TILE_W: f32 = 150.0;
const TILE_H: f32 = 84.0;

fn hex(color: u32) -> String {
    format!("#{color:06x}")
}

fn parse_hex(text: &str) -> Option<u32> {
    let t = text.trim().trim_start_matches('#');
    (t.len() == 6).then(|| u32::from_str_radix(t, 16).ok()).flatten()
}

/// Where wallpapers come from: the chosen folder, else ~/Pictures/Wallpapers.
pub fn wallpaper_dir(s: &Settings) -> PathBuf {
    s.wallpaper_dir.clone().unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Pictures/Wallpapers")
    })
}

fn field(placeholder: &str, text: String, window: &mut Window, cx: &mut Context<Workspace>) -> Entity<TextInput> {
    cx.new(|cx| {
        let mut input = TextInput::new(placeholder.to_string(), window, cx);
        input.text_color = theme::text().into();
        input.placeholder_color = theme::text_dim().into();
        input.selection_color = theme::accent_soft().into();
        input.cursor_color = theme::accent().into();
        input.set_text(text, cx);
        input
    })
}

impl Workspace {
    pub fn look_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) -> LookUi {
        let colors: Vec<u32> = match &self.store.settings.background {
            Background::Gradient { colors, .. } => colors.clone(),
            _ => PRESETS[0].colors.to_vec(),
        };
        let colors = [0, 1, 2].map(|i| field(if i == 2 { "optional" } else { "#rrggbb" }, colors.get(i).map(|c| hex(*c)).unwrap_or_default(), window, cx));
        let folder = field("Folder of images", wallpaper_dir(&self.store.settings).display().to_string(), window, cx);
        let mut events = Vec::new();
        for input in &colors {
            events.push(cx.subscribe_in(input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Changed | InputEvent::Submit(_) => this.apply_custom_colors(cx),
                InputEvent::Cancel => this.close_settings(window, cx),
                _ => {}
            }));
        }
        events.push(cx.subscribe_in(&folder, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit(text) => {
                let dir = PathBuf::from(text.trim());
                this.store.set(|s| s.wallpaper_dir = Some(dir));
                cx.notify();
            }
            InputEvent::Cancel => this.close_settings(window, cx),
            _ => {}
        }));
        LookUi { colors, folder, sliders: Rc::default(), _events: events }
    }

    /// The custom colors, once they read as 2 or 3 colors.
    fn apply_custom_colors(&mut self, cx: &mut Context<Self>) {
        let Some(look) = self.settings.as_ref().map(|d| &d.look) else { return };
        let texts: Vec<String> = look.colors.iter().map(|i| i.read(cx).text().to_string()).collect();
        let (first, second, third) = (parse_hex(&texts[0]), parse_hex(&texts[1]), parse_hex(&texts[2]));
        let (Some(a), Some(b)) = (first, second) else { return };
        if !texts[2].trim().is_empty() && third.is_none() {
            return;
        }
        let colors: Vec<u32> = [Some(a), Some(b), third].into_iter().flatten().collect();
        // The angle stays as it is (a gradient is showing: these fields are on its page).
        let angle = Slider::Angle.get(&self.store.settings);
        self.store.set(|s| s.background = Background::Gradient { colors, angle });
        cx.notify();
    }

    fn set_background(&mut self, background: Background, cx: &mut Context<Self>) {
        if let (Background::Gradient { colors, .. }, Some(dialog)) = (&background, self.settings.as_ref()) {
            for (i, input) in dialog.look.colors.iter().enumerate() {
                let text = colors.get(i).map(|c| hex(*c)).unwrap_or_default();
                input.update(cx, |input, cx| input.set_text(text, cx));
            }
        }
        self.store.set(|s| s.background = background);
        cx.notify();
    }

    /// A background's small picture, made off the main thread the first
    /// time it is asked for.
    fn thumb(&mut self, key: String, background: Background, cx: &mut Context<Self>) -> Option<Arc<RenderImage>> {
        if let Some(thumb) = self.thumbs.get(&key) {
            return thumb.clone();
        }
        self.thumbs.insert(key.clone(), None);
        let job = cx.background_executor().spawn(async move { backdrop::thumbnail(&background, TILE_W as u32 * 2, TILE_H as u32 * 2) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |ws, cx| {
                match result {
                    Ok(thumb) => {
                        ws.thumbs.insert(key, Some(thumb));
                    }
                    Err(error) => tracing::warn!("thumbnail for {key}: {error:#}"),
                }
                cx.notify();
            });
        })
        .detach();
        None
    }

    pub fn slide_to(&mut self, which: Slider, x: Pixels, cx: &mut Context<Self>) {
        let Some(bounds) = self.settings.as_ref().and_then(|d| d.look.sliders.borrow().get(&which).copied()) else { return };
        let f = (f32::from(x - bounds.origin.x) / f32::from(bounds.size.width)).clamp(0.0, 1.0);
        let (lo, hi) = which.range();
        // Live while dragging; saved when let go.
        which.set(&mut self.store.settings, lo + f * (hi - lo));
        cx.notify();
    }

    pub fn end_slide(&mut self, cx: &mut Context<Self>) {
        if self.slider_drag.take().is_some() {
            self.store.set(|_| {});
            cx.notify();
        }
    }

    /// While a slider is held: a layer over everything follows the pointer.
    pub fn slider_overlay(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let which = self.slider_drag?;
        Some(
            div()
                .id("slider-drag")
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| this.slide_to(which, ev.position.x, cx)))
                .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_slide(cx)))
                .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| this.end_slide(cx)))
                .into_any_element(),
        )
    }

    fn slider(&self, which: Slider, cx: &mut Context<Self>) -> AnyElement {
        let (lo, hi) = which.range();
        let value = which.get(&self.store.settings);
        let f = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
        let places = self.settings.as_ref().map(|d| d.look.sliders.clone()).unwrap_or_default();
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                // The knob's half-width of grab room past both ends of the
                // track: a knob at either end is grabbed on its whole face.
                div()
                    .id(SharedString::from(format!("slider-{which:?}")))
                    .relative()
                    .w(px(SLIDER_W + 2.0 * KNOB_R))
                    .h(px(20.))
                    .child(
                        div()
                            .absolute()
                            .left(px(KNOB_R))
                            .w(px(SLIDER_W))
                            .top_0()
                            .bottom_0()
                            .child(
                                canvas(move |bounds, _, _| { places.borrow_mut().insert(which, bounds); }, |_, _, _, _| {})
                                    .absolute()
                                    .inset_0(),
                            )
                            .child(div().absolute().left_0().right_0().top(px(8.)).h(px(4.)).rounded(px(2.)).bg(theme::wash(0.1)))
                            .child(div().absolute().left_0().top(px(8.)).w(px(f * SLIDER_W)).h(px(4.)).rounded(px(2.)).bg(theme::accent())),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(f * SLIDER_W))
                            .top(px(10.0 - KNOB_R))
                            .size(px(2.0 * KNOB_R))
                            .rounded_full()
                            .bg(gpui::white())
                            .shadow_sm(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.slider_drag = Some(which);
                            this.slide_to(which, ev.position.x, cx);
                        }),
                    ),
            )
            .child(div().w(px(48.)).text_size(px(12.)).text_color(theme::text_muted()).child(which.label(value)))
            .into_any_element()
    }

    pub fn look_page(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let settings = self.store.settings.clone();
        let kind = match settings.background {
            Background::Solid { .. } => 0,
            Background::Gradient { .. } => 1,
            Background::Wallpaper { .. } => 2,
        };
        let dir = wallpaper_dir(&settings);
        let walls = backdrop::wallpapers(&dir);

        // Solid / Gradient / Wallpaper.
        let segments = div().flex().p(px(2.)).gap(px(2.)).rounded(px(8.)).bg(theme::wash(0.05)).children(
            ["Solid", "Gradient", "Wallpaper"].into_iter().enumerate().map(|(i, label)| {
                let first_wall = walls.first().cloned();
                div()
                    .id(SharedString::from(format!("bg-kind-{label}")))
                    .px(px(12.))
                    .py(px(4.))
                    .rounded(px(6.))
                    .text_size(px(12.5))
                    .when(i == kind, |d| d.bg(theme::selected()).text_color(theme::text()))
                    .when(i != kind, |d| d.text_color(theme::text_muted()).hover(|d| d.text_color(theme::text())))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let background = match i {
                            0 => Background::default(),
                            1 => Background::Gradient { colors: PRESETS[0].colors.to_vec(), angle: PRESETS[0].angle },
                            _ => match first_wall.clone() {
                                Some(path) => Background::Wallpaper { path },
                                None => return,
                            },
                        };
                        if i != kind {
                            this.set_background(background, cx);
                        }
                    }))
            }),
        );

        let mut page = div().flex().flex_col().child(heading("Look")).child(row(
            "Background",
            Some("Behind the sidebar and between the panes.".to_string()),
            false,
            segments.into_any_element(),
        ));

        match &settings.background {
            Background::Solid { color } => {
                let current = *color;
                page = page.child(div().flex().flex_wrap().gap(px(10.)).py(px(12.)).children(SWATCHES.iter().map(|&c| {
                    div()
                        .id(SharedString::from(format!("swatch-{c:06x}")))
                        .size(px(28.))
                        .rounded_full()
                        .bg(gpui::rgb(c))
                        .border_2()
                        .border_color(if c == current { theme::accent().into() } else { theme::border() })
                        .on_click(cx.listener(move |this, _, _, cx| this.set_background(Background::Solid { color: c }, cx)))
                })));
            }
            Background::Gradient { colors, .. } => {
                let tiles: Vec<AnyElement> = PRESETS
                    .iter()
                    .map(|p| {
                        let background = Background::Gradient { colors: p.colors.to_vec(), angle: p.angle };
                        let on = p.colors == colors.as_slice();
                        let thumb = self.thumb(format!("preset:{}", p.name), background.clone(), cx);
                        self.tile(SharedString::from(format!("preset-{}", p.name)), thumb, Some(p.name), on, cx, move |this, cx| {
                            this.set_background(background.clone(), cx)
                        })
                    })
                    .collect();
                page = page.child(div().flex().flex_wrap().gap(px(10.)).py(px(12.)).children(tiles));
                let look = self.settings.as_ref().map(|d| d.look.colors.clone());
                if let Some(inputs) = look {
                    page = page.child(row(
                        "Colors",
                        Some("Two or three, as #rrggbb.".to_string()),
                        false,
                        div().flex().gap(px(6.)).children(inputs.into_iter().map(hex_field)).into_any_element(),
                    ));
                }
                page = page.child(row("Angle", None, false, self.slider(Slider::Angle, cx)));
            }
            Background::Wallpaper { path } => {
                let current = path.clone();
                if let Some(folder) = self.settings.as_ref().map(|d| d.look.folder.clone()) {
                    page = page.child(row(
                        "Folder",
                        Some(if walls.is_empty() { "No images here (.jpg, .png). Enter applies.".to_string() } else { format!("{} images. Enter applies.", walls.len()) }),
                        walls.is_empty(),
                        div()
                            .flex()
                            .items_center()
                            .w(px(240.))
                            .h(px(28.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .bg(theme::wash(0.05))
                            .border_1()
                            .border_color(theme::border())
                            .text_size(px(12.))
                            .child(folder)
                            .into_any_element(),
                    ));
                }
                let tiles: Vec<AnyElement> = walls
                    .iter()
                    .map(|wall| {
                        let background = Background::Wallpaper { path: wall.clone() };
                        let thumb = self.thumb(format!("wall:{}", wall.display()), background.clone(), cx);
                        self.tile(SharedString::from(format!("wall-{}", wall.display())), thumb, None, *wall == current, cx, move |this, cx| {
                            this.set_background(background.clone(), cx)
                        })
                    })
                    .collect();
                page = page.child(div().flex().flex_wrap().gap(px(10.)).py(px(12.)).children(tiles));
            }
        }

        page = page.child(row(
            "Sidebar opacity",
            Some("Lower lets the background through.".to_string()),
            false,
            self.slider(Slider::Opacity, cx),
        ));
        if matches!(settings.background, Background::Wallpaper { .. }) {
            page = page.child(row("Blur", Some("How frosted the sidebar's glass is.".to_string()), false, self.slider(Slider::Blur, cx)));
        }
        let floating = settings.floating_sidebar;
        page = page.child(row(
            "Floating sidebar",
            Some("The sidebar as a card of its own, floating like the panes.".to_string()),
            false,
            toggle("settings-floating-sidebar", floating, true)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.store.set(|s| s.floating_sidebar = !s.floating_sidebar);
                    cx.notify();
                }))
                .into_any_element(),
        ));
        page.into_any_element()
    }

    /// A picture to pick: a preset or a wallpaper.
    fn tile(
        &self,
        id: SharedString,
        thumb: Option<Arc<RenderImage>>,
        name: Option<&'static str>,
        on: bool,
        cx: &mut Context<Self>,
        pick: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
    ) -> AnyElement {
        div()
            .id(id)
            .flex()
            .flex_col()
            .gap(px(5.))
            .child(
                div()
                    .w(px(TILE_W))
                    .h(px(TILE_H))
                    .rounded(px(8.))
                    .overflow_hidden()
                    .bg(theme::wash(0.05))
                    .border_2()
                    .border_color(if on { theme::accent().into() } else { theme::border() })
                    .when_some(thumb, |d, t| d.child(img(t).size_full().object_fit(gpui::ObjectFit::Cover))),
            )
            .when_some(name, |d, name| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(if on { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                        .text_color(if on { theme::text() } else { theme::text_muted() })
                        .child(name),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| pick(this, cx)))
            .into_any_element()
    }
}

fn hex_field(input: Entity<TextInput>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .w(px(84.))
        .h(px(28.))
        .px(px(8.))
        .rounded(px(6.))
        .bg(theme::wash(0.05))
        .border_1()
        .border_color(theme::border())
        .text_size(px(12.))
        .child(input)
}

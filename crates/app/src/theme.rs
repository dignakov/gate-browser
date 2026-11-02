//! The look: herdr's dark panels and proportions, with zeron's accent.
//! Colors were taken from herdr's running window (its derived palette).

use gpui::{Hsla, Rgba, hsla, rgb, rgba};

pub const FONT: &str = "Geist";

/// Behind the pane cards (the work area's gaps).
pub fn canvas() -> Rgba {
    rgb(0x131518)
}
/// The sidebar.
pub fn chrome() -> Rgba {
    rgb(0x101114)
}
/// Pane cards and their headers.
pub fn surface() -> Rgba {
    rgb(0x17191d)
}
/// Cards floating over a page (questions it asks).
pub fn popover() -> Rgba {
    rgb(0x202227)
}
/// White ink at `alpha`: hovers, washes, hairlines.
pub fn wash(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}
pub fn border() -> Hsla {
    wash(0.094)
}
pub fn hover() -> Hsla {
    wash(0.055)
}
pub fn selected() -> Hsla {
    wash(0.09)
}
pub fn text() -> Rgba {
    rgb(0xeeeeef)
}
pub fn text_muted() -> Rgba {
    rgb(0x9b9c9f)
}
pub fn text_dim() -> Rgba {
    rgb(0x66676a)
}
pub fn accent() -> Rgba {
    rgb(0x8b7cf6)
}
pub fn accent_soft() -> Rgba {
    rgba(0x8b7cf633)
}
/// Something waits on the user.
pub fn needs_you() -> Rgba {
    rgb(0xe06c75)
}
/// Camera or microphone in use.
pub fn live() -> Rgba {
    rgb(0xe3814f)
}
pub fn playing() -> Rgba {
    rgb(0x3fae6a)
}
pub fn unread() -> Rgba {
    rgb(0x61afef)
}
pub fn working() -> Rgba {
    rgb(0xe5c07b)
}
pub fn close_red() -> Rgba {
    rgb(0xff5f57)
}
pub fn minimize_yellow() -> Rgba {
    rgb(0xfebc2e)
}
pub fn maximize_green() -> Rgba {
    rgb(0x28c840)
}

/// One strong color per hue for letter tiles (herdr's space avatars).
const TILES: [u32; 6] = [0x22a99a, 0xe3814f, 0x5b8def, 0x8b7cf6, 0x3fae6a, 0xd9649a];

/// A stable tile color for `key` (a site's host).
pub fn tile(key: &str) -> Rgba {
    // FNV-1a: stable across runs, unlike std's hasher.
    let hash = key.bytes().fold(0x811c9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x01000193));
    rgb(TILES[hash as usize % TILES.len()])
}

/// Window corners while floating (not maximized or tiled).
pub const WINDOW_RADIUS: f32 = 10.0;
pub const PANE_RADIUS: f32 = 10.0;
pub const CONTROL_RADIUS: f32 = 6.0;

pub const SIDEBAR_WIDTH: f32 = 268.0;
pub const SIDEBAR_MIN: f32 = 200.0;
/// Dragged narrower than this, the sidebar collapses to its rail.
pub const SIDEBAR_COLLAPSE: f32 = 140.0;
pub const SIDEBAR_MAX: f32 = 480.0;
/// The collapsed sidebar: a rail of tiles.
pub const RAIL_WIDTH: f32 = 44.0;
/// The sidebar's top row (window controls and navigation).
pub const HEADER_HEIGHT: f32 = 46.0;
/// The address field and the space below it.
pub const ADDRESS_BLOCK: f32 = 38.0;
/// The sidebar's bottom row (Settings), with its padding.
pub const FOOTER_HEIGHT: f32 = 40.0;
/// The settings dialog, at most (smaller windows shrink it).
pub const SETTINGS_WIDTH: f32 = 760.0;
pub const SETTINGS_HEIGHT: f32 = 500.0;
/// The least a sidebar list (Spaces, Activity) is given.
pub const LIST_MIN: f32 = 90.0;
pub const ROW_HEIGHT: f32 = 30.0;
pub const PANE_HEADER_HEIGHT: f32 = 30.0;
/// Space around and between panes.
pub const GAP: f32 = 8.0;

/// Text colors for one surface, chosen for what is behind it: the sidebar
/// over a see-through background gets brighter grays (or dark ones, over a
/// light background) so labels keep their contrast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    pub text: Rgba,
    pub muted: Rgba,
    pub dim: Rgba,
    pub hover: Hsla,
    pub selected: Hsla,
}

/// WCAG contrast the grays keep at least (text gets the most it can).
const MUTED_CONTRAST: f32 = 4.5;
const DIM_CONTRAST: f32 = 3.0;

impl Ink {
    /// The palette on the opaque sidebar.
    pub fn standard() -> Self {
        Self { text: text(), muted: text_muted(), dim: text_dim(), hover: hover(), selected: selected() }
    }

    /// Ink for text on `background` (an opaque color). `see_through` (0–1)
    /// is how much of a picture shows through: the average color hides its
    /// bright and dark patches, so the grays keep a margin above the
    /// minimum contrast, the more the more shows through.
    pub fn over(background: Rgba, see_through: f32) -> Self {
        let margin = 1.0 + see_through.clamp(0.0, 1.0) * 1.5;
        let (muted_goal, dim_goal) = (MUTED_CONTRAST * margin, DIM_CONTRAST * margin);
        let light = contrast(rgb(0xffffff), background) >= contrast(rgb(0x000000), background);
        if light {
            let text = text();
            Self {
                text,
                muted: toward(text_muted(), text, background, muted_goal),
                dim: toward(text_dim(), text, background, dim_goal),
                hover: hover(),
                selected: selected(),
            }
        } else {
            let text = rgb(0x141518);
            Self {
                text,
                muted: toward(rgb(0x4a4b50), text, background, muted_goal),
                dim: toward(rgb(0x6b6c70), text, background, dim_goal),
                hover: hsla(0.0, 0.0, 0.0, 0.06),
                selected: hsla(0.0, 0.0, 0.0, 0.1),
            }
        }
    }
}

/// `from`, moved toward `to` just until it reaches `goal` contrast on
/// `background` (unchanged if it already does).
fn toward(from: Rgba, to: Rgba, background: Rgba, goal: f32) -> Rgba {
    (0..=20)
        .map(|i| mix(from, to, i as f32 / 20.0))
        .find(|c| contrast(*c, background) >= goal)
        .unwrap_or(to)
}

/// `a` with `t` of `b` mixed in.
pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba { r: a.r + (b.r - a.r) * t, g: a.g + (b.g - a.g) * t, b: a.b + (b.b - a.b) * t, a: 1.0 }
}

fn luminance(c: Rgba) -> f32 {
    let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_sidebar_keeps_its_colors() {
        assert_eq!(Ink::over(chrome(), 0.0), Ink::standard());
    }

    #[test]
    fn grays_brighten_over_a_lighter_background_and_flip_over_a_light_one() {
        let mid = Ink::over(rgb(0x5a4a6a), 0.0);
        assert!(contrast(mid.muted, rgb(0x5a4a6a)) >= MUTED_CONTRAST || mid.muted == text());
        assert!(contrast(mid.dim, rgb(0x5a4a6a)) >= DIM_CONTRAST || mid.dim == text());
        let light = Ink::over(rgb(0xe8e0f0), 0.0);
        assert!(luminance(light.text) < 0.05);
        assert!(contrast(light.muted, rgb(0xe8e0f0)) >= MUTED_CONTRAST);
    }

    #[test]
    fn more_see_through_means_brighter_grays() {
        let bg = rgb(0x2a2030);
        assert!(luminance(Ink::over(bg, 0.4).dim) > luminance(Ink::over(bg, 0.0).dim));
    }
}

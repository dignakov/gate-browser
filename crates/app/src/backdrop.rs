//! The window's background: a solid color, a gradient, or a wallpaper,
//! drawn behind the sidebar and the pane cards; and its blurred twin, which
//! the sidebar shows through its tint (frosted glass).
//!
//! Gradients and wallpapers become one image the window's size, made off
//! the main thread when the setting or the window's size changes. The
//! glass is that image blurred once, not a live blur: the sidebar only ever
//! sits on the background, so drawing it costs what a picture costs. Pages
//! stay opaque, so their text keeps its subpixel smoothing.

use gpui::RenderImage;
use image::{Rgba, RgbaImage, imageops};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Background {
    /// One color per theme (`None`: the theme's own background).
    Solid {
        #[serde(alias = "color", default, skip_serializing_if = "Option::is_none")]
        dark: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        light: Option<u32>,
    },
    /// Two or three colors (0xRRGGBB) along `angle` degrees, as CSS's
    /// `linear-gradient` (0: bottom to top, 90: left to right, 180: top to
    /// bottom).
    Gradient { colors: Vec<u32>, angle: f32 },
    Wallpaper { path: PathBuf },
}

impl Default for Background {
    fn default() -> Self {
        Self::Solid { dark: None, light: None }
    }
}

impl Background {
    /// A solid background's color (0xRRGGBB) in the theme in use.
    pub fn solid(&self) -> Option<u32> {
        let Self::Solid { dark, light } = self else { return None };
        Some(if crate::theme::dark() {
            dark.unwrap_or(crate::theme::DARK_PALETTE.canvas)
        } else {
            light.unwrap_or(crate::theme::LIGHT_PALETTE.canvas)
        })
    }
}

pub struct Preset {
    pub name: &'static str,
    pub colors: &'static [u32],
    pub angle: f32,
}

pub const PRESETS: &[Preset] = &[
    Preset { name: "Miami Night", colors: &[0x1b0b3a, 0xff2a6d, 0x05d9e8], angle: 135.0 },
    Preset { name: "Tokyo", colors: &[0x0f0c29, 0xd4145a, 0x00b4ff], angle: 120.0 },
    Preset { name: "Purple–Orange", colors: &[0x4a1d7a, 0xff7e3f], angle: 135.0 },
    Preset { name: "Purple–Orange–Green", colors: &[0x5b247a, 0xff8a00, 0x1fa57a], angle: 135.0 },
    Preset { name: "Synthwave", colors: &[0x241734, 0xb8336a, 0xf9a03f], angle: 180.0 },
    Preset { name: "Aurora", colors: &[0x0b2e33, 0x1fb48f, 0x6b4fd8], angle: 160.0 },
    Preset { name: "Deep Sea", colors: &[0x0f2027, 0x203a43, 0x2c5364], angle: 135.0 },
    Preset { name: "Ember", colors: &[0x1c0a04, 0xa8321e, 0xf0a020], angle: 160.0 },
    Preset { name: "Dusk", colors: &[0x232b45, 0x8a4f7d, 0xf1916d], angle: 170.0 },
];

/// Solid colors offered as swatches, in the dark theme and the light one.
pub const DARK_SWATCHES: &[u32] = &[0x131518, 0x0e0f12, 0x1b1d23, 0x1a1626, 0x10201f, 0x221a17, 0x1c2433, 0x2a2a2e];
pub const LIGHT_SWATCHES: &[u32] = &[0xe9eaee, 0xf6f6f8, 0xdedfe4, 0xe7e3f0, 0xdfebe7, 0xf0e7e0, 0xdce4ef, 0xd4d4d8];

/// What a render was made from: a new one is needed when it changes.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub background: Background,
    /// Device pixels.
    pub width: u32,
    pub height: u32,
    /// Blur radius in device pixels (0: no glass image).
    pub blur: u32,
}

pub struct Rendered {
    pub key: Key,
    pub sharp: Arc<RenderImage>,
    /// Lower resolution (the blur hides it); drawn stretched.
    pub blurred: Option<Arc<RenderImage>>,
}

/// The last wallpaper decoded, so resizing the window doesn't decode it again.
static DECODED: Mutex<Option<(PathBuf, Arc<RgbaImage>)>> = Mutex::new(None);

/// Makes the images for `key`; `None` for a solid color (painted as one).
/// Blocking: run it off the main thread.
pub fn render(key: &Key) -> anyhow::Result<Option<Rendered>> {
    let (w, h) = (key.width.max(1), key.height.max(1));
    let sharp = match &key.background {
        Background::Solid { .. } => return Ok(None),
        Background::Gradient { colors, angle } => gradient(colors, *angle, w, h),
        Background::Wallpaper { path } => cover(decode(path)?.as_ref(), w, h),
    };
    // Blurred at a quarter size: four times fewer pixels, a quarter radius.
    let blurred = (key.blur > 0).then(|| {
        let small = imageops::resize(&sharp, (w / 4).max(1), (h / 4).max(1), imageops::FilterType::Triangle);
        bgra(imageops::fast_blur(&small, key.blur as f32 / 4.0))
    });
    Ok(Some(Rendered { key: key.clone(), sharp: bgra(sharp), blurred }))
}

/// A small picture of a background, for Settings.
pub fn thumbnail(background: &Background, w: u32, h: u32) -> anyhow::Result<Arc<RenderImage>> {
    Ok(bgra(match background {
        Background::Solid { .. } => RgbaImage::from_pixel(w, h, rgba(background.solid().unwrap_or_default())),
        Background::Gradient { colors, angle } => gradient(colors, *angle, w, h),
        Background::Wallpaper { path } => {
            let image = image::open(path)?;
            cover(&image.thumbnail(w * 2, h * 2).into_rgba8(), w, h)
        }
    }))
}

/// The average color (0xRRGGBB) of `image` over `region`, given as
/// fractions of its size (left, top, right, bottom), from a grid of samples.
pub fn average(image: &RenderImage, region: [f32; 4]) -> Option<u32> {
    let size = image.size(0);
    let (w, h) = (size.width.0.max(1) as usize, size.height.0.max(1) as usize);
    let bytes = image.as_bytes(0)?;
    let [l, t, r, b] = region.map(|f| f.clamp(0.0, 1.0));
    let (x0, x1) = ((l * w as f32) as usize, ((r * w as f32) as usize).max((l * w as f32) as usize + 1).min(w));
    let (y0, y1) = ((t * h as f32) as usize, ((b * h as f32) as usize).max((t * h as f32) as usize + 1).min(h));
    const GRID: usize = 48;
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for gy in 0..GRID {
        let y = y0 + (y1 - y0) * gy / GRID;
        for gx in 0..GRID {
            let x = x0 + (x1 - x0) * gx / GRID;
            let i = (y * w + x) * 4;
            let Some(p) = bytes.get(i..i + 3) else { continue };
            // BGRA.
            sum[0] += p[2] as u64;
            sum[1] += p[1] as u64;
            sum[2] += p[0] as u64;
            n += 1;
        }
    }
    (n > 0).then(|| ((sum[0] / n) << 16 | (sum[1] / n) << 8 | (sum[2] / n)) as u32)
}

/// The wallpapers in `dir`: images, sorted by name.
pub fn wallpapers(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg" | "png"))
        })
        .collect();
    out.sort();
    out
}

fn decode(path: &Path) -> anyhow::Result<Arc<RgbaImage>> {
    let mut cache = DECODED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((cached, image)) = cache.as_ref()
        && cached == path
    {
        return Ok(image.clone());
    }
    let image = Arc::new(image::open(path)?.into_rgba8());
    *cache = Some((path.to_path_buf(), image.clone()));
    Ok(image)
}

/// Scaled to cover `w`×`h`, centered, the overflow cropped.
fn cover(image: &RgbaImage, w: u32, h: u32) -> RgbaImage {
    let (iw, ih) = image.dimensions();
    let scale = (w as f32 / iw as f32).max(h as f32 / ih as f32);
    let (sw, sh) = (((iw as f32 * scale).ceil() as u32).max(w), ((ih as f32 * scale).ceil() as u32).max(h));
    let scaled = imageops::resize(image, sw, sh, imageops::FilterType::Triangle);
    imageops::crop_imm(&scaled, (sw - w) / 2, (sh - h) / 2, w, h).to_image()
}

fn rgba(color: u32) -> Rgba<u8> {
    Rgba([(color >> 16) as u8, (color >> 8) as u8, color as u8, 255])
}

/// Colors spread evenly along `angle`, mixed in Oklab (no muddy middles),
/// with a little noise so wide gradients don't band.
fn gradient(colors: &[u32], angle: f32, w: u32, h: u32) -> RgbaImage {
    let stops: Vec<[f32; 3]> = match colors {
        [] => vec![[0.0; 3]],
        colors => colors.iter().map(|c| oklab(*c)).collect(),
    };
    // CSS: 0deg points up, angles turn clockwise.
    let (dx, dy) = (angle.to_radians().sin(), -angle.to_radians().cos());
    // The span the direction covers across the window's corners.
    let corners = [(0.0, 0.0), (w as f32, 0.0), (0.0, h as f32), (w as f32, h as f32)];
    let along: Vec<f32> = corners.iter().map(|(x, y)| x * dx + y * dy).collect();
    let (lo, hi) = (along.iter().copied().fold(f32::MAX, f32::min), along.iter().copied().fold(f32::MIN, f32::max));
    let span = (hi - lo).max(1.0);
    let mut seed = 0x9e37_79b9u32;
    RgbaImage::from_fn(w, h, |x, y| {
        // At pixel centers.
        let t = (((x as f32 + 0.5) * dx + (y as f32 + 0.5) * dy) - lo) / span;
        let pos = t.clamp(0.0, 1.0) * (stops.len() - 1) as f32;
        let i = (pos.floor() as usize).min(stops.len().saturating_sub(2));
        let f = if stops.len() == 1 { 0.0 } else { pos - i as f32 };
        let (a, b) = (stops[i], stops[(i + 1).min(stops.len() - 1)]);
        let lab = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f];
        let [r, g, bl] = srgb(lab);
        // xorshift: ±0.5 of a level of noise.
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = (seed as f32 / u32::MAX as f32) - 0.5;
        let px = |v: f32| (v * 255.0 + noise).round().clamp(0.0, 255.0) as u8;
        Rgba([px(r), px(g), px(bl), 255])
    })
}

fn oklab(color: u32) -> [f32; 3] {
    let lin = |c: u32| {
        let v = (c & 0xff) as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(color >> 16), lin(color >> 8), lin(color));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn srgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    let lin = [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ];
    lin.map(|v| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
    })
}

/// GPUI keeps image pixels as BGRA.
fn bgra(mut image: RgbaImage) -> Arc<RenderImage> {
    image.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    Arc::new(RenderImage::new([image::Frame::new(image)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_round_trips() {
        for c in [0x000000, 0xffffff, 0xff2a6d, 0x05d9e8, 0x5b247a] {
            let [r, g, b] = srgb(oklab(c));
            let back = ((r * 255.0).round() as u32) << 16 | ((g * 255.0).round() as u32) << 8 | (b * 255.0).round() as u32;
            assert_eq!(back, c, "{c:06x}");
        }
    }

    #[test]
    fn gradient_runs_from_first_to_last_color() {
        // Pixel centers sit half a pixel in from the ends: 0.5/64 and 63.5/64.
        let g = gradient(&[0x000000, 0xffffff], 90.0, 64, 4);
        assert!(g.get_pixel(0, 0)[0] <= 3);
        assert!(g.get_pixel(63, 0)[0] >= 250);
        assert!(g.get_pixel(0, 0)[0] < g.get_pixel(32, 0)[0] && g.get_pixel(32, 0)[0] < g.get_pixel(63, 0)[0]);
    }
}

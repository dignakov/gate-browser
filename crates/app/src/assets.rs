//! Icons and fonts built into the binary.

use gpui::{App, AssetSource, Result, SharedString};
use std::borrow::Cow;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("icons/", $name, ".svg"), include_bytes!(concat!("../../../assets/icons/", $name, ".svg")))),*]
    };
}

static ICONS: &[(&str, &[u8])] = icons![
    "arrow-left", "arrow-right", "rotate-cw", "x", "plus", "minus", "panel-left", "globe", "search", "lock",
    "columns-2", "rows-2", "maximize-2", "minimize-2", "mic", "video", "triangle-alert", "loader", "volume-2", "volume-x", "play", "pause",
    "settings", "shield", "external-link", "palette",
];

static FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/Geist.ttf"),
    include_bytes!("../../../assets/fonts/Geist-Medium.ttf"),
    include_bytes!("../../../assets/fonts/Geist-SemiBold.ttf"),
    include_bytes!("../../../assets/fonts/GeistMono.ttf"),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}

pub fn load_fonts(cx: &App) {
    let fonts = FONTS.iter().map(|f| Cow::Borrowed(*f)).collect();
    if let Err(error) = cx.text_system().add_fonts(fonts) {
        tracing::warn!("bundled fonts: {error:#}");
    }
}

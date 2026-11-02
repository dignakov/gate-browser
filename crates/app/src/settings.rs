//! What the user set in Settings, kept in `settings.json` in the profile
//! folder and written as soon as something changes.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Load uBlock Origin Lite (read at startup).
    pub ad_blocking: bool,
    /// Download new uBlock Origin Lite releases (applied at the next start).
    pub ublock_auto_update: bool,
    /// Light or dark, for Gate and the pages' `prefers-color-scheme`.
    pub theme: Theme,
    /// The sidebar as a floating card (as the panes), not an edge panel.
    pub floating_sidebar: bool,
    /// Behind the sidebar and the panes.
    pub background: crate::backdrop::Background,
    /// Where wallpapers are picked from.
    pub wallpaper_dir: Option<PathBuf>,
    /// How opaque the sidebar's tint is over the background (0.3–1).
    pub chrome_opacity: f32,
    /// The glass's blur radius over a wallpaper, in pixels.
    pub blur: f32,
    /// Pages hand frames over as GPU buffers (no copies through the CPU),
    /// where the platform can (read at startup). Off until verified: see
    /// `web`'s gpu_frames module.
    pub gpu_frames: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ad_blocking: true,
            ublock_auto_update: true,
            theme: Theme::System,
            floating_sidebar: false,
            background: Default::default(),
            wallpaper_dir: None,
            chrome_opacity: 1.0,
            blur: 30.0,
            gpu_frames: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// As the system's appearance.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    /// Dark or not, given the system's appearance.
    pub fn dark(self, appearance: gpui::WindowAppearance) -> bool {
        match self {
            Self::System => matches!(appearance, gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark),
            Self::Light => false,
            Self::Dark => true,
        }
    }
}

impl Settings {
    /// The saved settings; defaults for anything missing or unreadable.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        serde_json::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!("{}: {error}; using defaults", path.display());
            Self::default()
        })
    }

    pub fn save(&self, path: &Path) {
        let text = serde_json::to_string_pretty(self).expect("settings serialize");
        // Through a temporary file: a crash mid-write leaves the old file.
        let tmp = path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&tmp, text + "\n").and_then(|()| std::fs::rename(&tmp, path)) {
            tracing::warn!("saving {}: {error}", path.display());
        }
    }
}

/// The settings, where they live, and what this run started with (some
/// settings only apply at startup).
pub struct Store {
    pub path: PathBuf,
    pub settings: Settings,
    /// uBlock Origin Lite's id while it runs; `None` if off or missing.
    pub ublock: Option<String>,
    pub ublock_missing: bool,
    /// The installed version (running or not).
    pub ublock_version: Option<String>,
    pub ublock_paths: crate::ublock::Paths,
    pub ublock_update: UpdateState,
}

/// Looking for, and downloading, uBlock Origin Lite releases.
#[derive(Default)]
pub struct UpdateState {
    pub checking: bool,
    /// Downloaded; the next start uses it.
    pub ready: Option<String>,
    pub error: Option<String>,
}

impl Store {
    pub fn set(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings);
        self.settings.save(&self.path);
    }

    /// Ad blocking was switched since startup; a restart applies it.
    pub fn ad_blocking_pending(&self) -> bool {
        !self.ublock_missing && self.settings.ad_blocking != self.ublock.is_some()
    }

    /// uBlock Origin Lite's own settings page, while it runs.
    pub fn ublock_dashboard(&self) -> Option<String> {
        self.ublock.as_ref().map(|id| format!("chrome-extension://{id}/dashboard.html"))
    }
}

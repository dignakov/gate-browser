//! uBlock Origin Lite: where it runs from, and keeping it up to date.
//!
//! Its filter rules ship inside each release, so staying current means
//! replacing the extension. It runs from `<profile>/extensions/ubol`, a path
//! that never changes: Chromium names an unpacked extension by its path, and
//! its settings (filtering mode, sites left alone) live under that name.
//!
//! A newer release is downloaded (checked against GitHub's SHA-256) and
//! unpacked beside it as `ubol.next`; the next launch swaps it in before
//! Chromium starts. The copy in the source tree only seeds a new profile
//! (or replaces an older install).

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const RELEASES: &str = "https://api.github.com/repos/uBlockOrigin/uBOL-home/releases/latest";
/// How often to look for a new release.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct Paths {
    /// What runs.
    pub live: PathBuf,
    /// A downloaded update, waiting for the next launch.
    pub next: PathBuf,
    /// When we last looked (its modification time).
    stamp: PathBuf,
}

impl Paths {
    pub fn new(profile: &Path) -> Self {
        let dir = profile.join("extensions");
        Self { live: dir.join("ubol"), next: dir.join("ubol.next"), stamp: dir.join("ubol.checked") }
    }

    /// When the last check ran, if one has.
    pub fn last_checked(&self) -> Option<SystemTime> {
        fs::metadata(&self.stamp).and_then(|m| m.modified()).ok()
    }

    pub fn check_due(&self) -> bool {
        self.last_checked().and_then(|t| t.elapsed().ok()).is_none_or(|age| age >= CHECK_EVERY)
    }
}

/// The bundled copy in the source tree, if present.
fn bundled() -> Option<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/ubol");
    dir.join("manifest.json").is_file().then(|| dir.canonicalize().ok()).flatten()
}

/// The version in an unpacked extension's manifest.
pub fn version(dir: &Path) -> Option<String> {
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
    }
    let text = fs::read_to_string(dir.join("manifest.json")).ok()?;
    serde_json::from_str::<Manifest>(&text).ok().map(|m| m.version)
}

/// "2026.1026.1812" as numbers, to compare.
fn parts(version: &str) -> Vec<u64> {
    version.split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

fn newer(a: &str, b: &str) -> bool {
    parts(a) > parts(b)
}

/// Before Chromium starts: swaps in a waiting update, or installs the
/// bundled copy when there is none yet (or it is newer). Returns the folder
/// to load, if there is one.
pub fn prepare(paths: &Paths) -> Option<PathBuf> {
    if version(&paths.next).is_some() {
        if let Err(error) = replace_dir(&paths.next, &paths.live) {
            tracing::warn!("installing the uBlock Origin Lite update: {error}");
        }
    }
    if let Some(seed) = bundled() {
        let stale = match (version(&paths.live), version(&seed)) {
            (None, _) => true,
            (Some(live), Some(seed)) => newer(&seed, &live),
            _ => false,
        };
        if stale {
            let staged = paths.live.with_extension("seed");
            let copied = remove_dir(&staged).and_then(|()| copy_dir(&seed, &staged)).and_then(|()| replace_dir(&staged, &paths.live));
            if let Err(error) = copied {
                tracing::warn!("installing uBlock Origin Lite: {error}");
            }
        }
    }
    version(&paths.live).is_some().then(|| paths.live.canonicalize().unwrap_or_else(|_| paths.live.clone()))
}

/// What a check found.
pub enum Checked {
    /// Nothing newer than `version`.
    Current,
    /// `version` is downloaded and waits for a restart.
    Ready(String),
}

/// Looks for a newer release than `running` (or the one already waiting)
/// and, if there is one, downloads and unpacks it as the next version.
/// Blocking: run it off the main thread.
pub fn check(paths: &Paths, running: &str) -> anyhow::Result<Checked> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        digest: Option<String>,
    }

    let agent = ureq::Agent::new_with_defaults();
    let release: Release =
        agent.get(RELEASES).header("User-Agent", "Gate").call()?.body_mut().read_json()?;
    touch(&paths.stamp);

    let have = version(&paths.next).filter(|n| newer(n, running)).unwrap_or_else(|| running.to_string());
    if !newer(&release.tag_name, &have) {
        return Ok(if have == running { Checked::Current } else { Checked::Ready(have) });
    }
    let asset = release
        .assets
        .iter()
        .find(|a| a.name.ends_with(".chromium.zip"))
        .ok_or_else(|| anyhow::anyhow!("release {} has no Chromium build", release.tag_name))?;
    let want = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .ok_or_else(|| anyhow::anyhow!("release {} lists no SHA-256", release.tag_name))?;

    let mut zip = Vec::new();
    agent
        .get(&asset.browser_download_url)
        .header("User-Agent", "Gate")
        .call()?
        .body_mut()
        .with_config()
        .limit(200 * 1024 * 1024)
        .reader()
        .read_to_end(&mut zip)?;
    let got: String = Sha256::digest(&zip).iter().map(|b| format!("{b:02x}")).collect();
    anyhow::ensure!(got == want, "download of {} doesn't match its SHA-256", asset.name);

    // Unpacked beside, then renamed: a half-written folder never looks ready.
    let staged = paths.next.with_extension("part");
    remove_dir(&staged)?;
    zip::ZipArchive::new(std::io::Cursor::new(zip))?.extract(&staged)?;
    let unpacked = version(&staged);
    anyhow::ensure!(unpacked.as_deref() == Some(release.tag_name.as_str()), "{} unpacked as version {unpacked:?}", asset.name);
    replace_dir(&staged, &paths.next)?;
    Ok(Checked::Ready(release.tag_name))
}

fn touch(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(error) = fs::write(path, b"") {
        tracing::warn!("{}: {error}", path.display());
    }
}

fn remove_dir(dir: &Path) -> std::io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Puts `from` where `to` is, the old `to` removed.
fn replace_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir)?;
    }
    let old = to.with_extension("old");
    remove_dir(&old)?;
    if to.exists() {
        fs::rename(to, &old)?;
    }
    fs::rename(from, to)?;
    remove_dir(&old)
}

/// Copies an extension folder, leaving out what Chromium generated in it.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "_metadata" {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            copy_dir(&path, &to.join(&name))?;
        } else {
            fs::copy(&path, to.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        assert!(newer("2026.1101.900", "2026.1026.1812"));
        assert!(newer("2026.1026.1812", "2026.1026.912"));
        assert!(!newer("2026.1026.1812", "2026.1026.1812"));
    }
}

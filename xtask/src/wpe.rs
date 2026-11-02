//! WPE WebKit, the Linux engine: `cargo xtask build-wpe`. CI caches the
//! install by this file and xtask/patches/, so what the build depends on
//! lives here.

use crate::{root, run};
use anyhow::{Context as _, Result, ensure};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The release Gate builds against, unpacked into `third_party/wpe/` and
/// installed to `third_party/wpe/install`.
const WPE_VERSION: &str = "2.54.2";
const WPE_SHA256: &str = "f3284e19e47fcb5ac8c63c2e1c113dbd65b2423eb8a545f5b345707829004f56";

/// Builds WPE WebKit with only what Gate uses: the WPEPlatform API (Gate
/// implements its own platform, so WPE's Wayland and DRM ones are off),
/// media through GStreamer, the PDF viewer.
pub(crate) fn wpe_install(root: &Path) -> PathBuf {
    root.join("third_party/wpe/install")
}

/// WebKit's license notices (in Source/WebCore), installed with it to
/// `share/doc/wpe-webkit/`, and their names in a package's licenses/.
pub(crate) const NOTICES: [(&str, &str); 3] = [
    ("LICENSE-LGPL-2.1", "webkit-LICENSE-LGPL-2.1.txt"),
    ("LICENSE-LGPL-2", "webkit-LICENSE-LGPL-2.txt"),
    ("LICENSE-APPLE", "webkit-LICENSE-APPLE.txt"),
];

pub(crate) fn build_wpe() -> Result<()> {
    let dir = root().join("third_party/wpe");
    fs::create_dir_all(&dir)?;
    let tarball = dir.join(format!("wpewebkit-{WPE_VERSION}.tar.xz"));
    if !tarball.is_file() {
        run(Command::new("curl")
            .current_dir(&dir)
            .args(["-fL", "-o"])
            .arg(&tarball)
            .arg(format!("https://wpewebkit.org/releases/wpewebkit-{WPE_VERSION}.tar.xz")))?;
    }
    let sum = Command::new("sha256sum").arg(&tarball).output().context("running sha256sum")?;
    let sum = String::from_utf8_lossy(&sum.stdout);
    ensure!(sum.split_whitespace().next() == Some(WPE_SHA256), "{} doesn't match its SHA-256", tarball.display());
    let source = dir.join(format!("wpewebkit-{WPE_VERSION}"));
    if !source.is_dir() {
        run(Command::new("tar").current_dir(&dir).arg("-xJf").arg(&tarball))?;
    }
    patch_wpe(&source)?;
    let build = dir.join("build");
    let install = wpe_install(&root());
    let off = [
        "ENABLE_WPE_LEGACY_API",
        "ENABLE_WPE_PLATFORM_DRM",
        "ENABLE_WPE_PLATFORM_WAYLAND",
        "ENABLE_WPE_QT_API",
        "ENABLE_DOCUMENTATION",
        "ENABLE_INTROSPECTION",
        "ENABLE_JOURNALD_LOG",
        "ENABLE_BUBBLEWRAP_SANDBOX",
        "ENABLE_SPEECH_SYNTHESIS",
        "ENABLE_SPELLCHECK",
        "ENABLE_GAMEPAD",
        "ENABLE_WEBDRIVER",
        "USE_ATK",
        "USE_FLITE",
        "USE_JPEGXL",
        "USE_LIBBACKTRACE",
        "USE_LIBHYPHEN",
        "USE_SYSPROF_CAPTURE",
    ];
    let mut configure = Command::new("cmake");
    configure
        .arg("-S")
        .arg(&source)
        .arg("-B")
        .arg(&build)
        .args(["-G", "Ninja", "-DPORT=WPE", "-DCMAKE_BUILD_TYPE=Release", "-DENABLE_WPE_PLATFORM=ON", "-DENABLE_WPE_PLATFORM_HEADLESS=ON"])
        .arg(format!("-DCMAKE_INSTALL_PREFIX={}", install.display()))
        // Its processes (libexec/wpe-webkit-2.0) and libraries find each
        // other wherever the install goes.
        .arg("-DCMAKE_INSTALL_RPATH=$ORIGIN/../../lib;$ORIGIN/../lib;$ORIGIN")
        .args(off.iter().map(|o| format!("-D{o}=OFF")));
    run(&mut configure).context("configuring WPE WebKit (missing -dev packages show above)")?;
    // WebKit builds for an hour or two; GATE_JOBS caps the parallelism.
    let jobs = std::env::var("GATE_JOBS").unwrap_or_else(|_| "4".into());
    run(Command::new("cmake").arg("--build").arg(&build).args(["-j", &jobs])).context("building WPE WebKit")?;
    run(Command::new("cmake").arg("--install").arg(&build)).context("installing WPE WebKit")?;
    let notices = install.join("share/doc/wpe-webkit");
    fs::create_dir_all(&notices)?;
    for (notice, _) in NOTICES {
        fs::copy(source.join("Source/WebCore").join(notice), notices.join(notice))?;
    }
    println!("{}", install.display());
    Ok(())
}

/// Applies Gate's patches (`xtask/patches/wpe-*.patch`) to the WPE WebKit
/// source, each once.
fn patch_wpe(source: &Path) -> Result<()> {
    let mut patches: Vec<PathBuf> = fs::read_dir(root().join("xtask/patches"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("wpe-") && n.ends_with(".patch")))
        .collect();
    patches.sort();
    for patch in patches {
        let applied = Command::new("patch")
            .current_dir(source)
            .args(["-p1", "-R", "-s", "-f", "--dry-run", "-i"])
            .arg(&patch)
            .status()
            .context("running patch")?
            .success();
        if !applied {
            run(Command::new("patch").current_dir(source).args(["-p1", "-N", "-i"]).arg(&patch)).with_context(|| format!("applying {}", patch.display()))?;
        }
    }
    Ok(())
}


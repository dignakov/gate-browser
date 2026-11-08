//! WPE WebKit, the Linux engine, and the GStreamer it runs media and
//! WebRTC on: `cargo xtask build-wpe`. CI caches the install by this file and
//! xtask/patches/, so what the build depends on lives here.

use crate::{root, run};
use anyhow::{Context as _, Result, ensure};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The release Gate builds against, unpacked into `third_party/wpe/` and
/// installed to `third_party/wpe/install`.
const WPE_VERSION: &str = "2.54.2";
const WPE_SHA256: &str = "f3284e19e47fcb5ac8c63c2e1c113dbd65b2423eb8a545f5b345707829004f56";

/// GStreamer, cloned into `third_party/gstreamer/src` and installed with
/// WebKit, which builds against it; Gate ships it. GStreamer 1.24 (Ubuntu
/// 24.04's) breaks WebRTC renegotiation; 1.26 fixes it.
const GST_VERSION: &str = "1.26.8";
const GST_COMMIT: &str = "16d77e12ad213ef24e76a8cc34d347b8221c9975";

/// What Gate takes from its own GStreamer: the libraries WebKit links and
/// the WebRTC plugins. Every other plugin (FFmpeg's decoders, sinks, …)
/// comes from the system's GStreamer, which Gate's loads too.
const GST_FEATURES: [&str; 18] = [
    "base",
    "good",
    "bad",
    "orc",
    "webrtc",
    // The plugin scanner is one of the tools.
    "tools",
    "gst-plugins-base:app",
    "gst-plugins-base:audioconvert",
    "gst-plugins-base:audioresample",
    "gst-plugins-base:videoconvertscale",
    "gst-plugins-base:opus",
    "gst-plugins-base:gl",
    "gst-plugins-good:rtp",
    "gst-plugins-good:rtpmanager",
    "gst-plugins-good:vpx",
    "gst-plugins-bad:dtls",
    "gst-plugins-bad:srtp",
    "gst-plugins-bad:sctp",
];

/// GStreamer's license, installed to `share/doc/gstreamer/`, and its name in
/// a package's licenses/.
pub(crate) const GST_NOTICE: (&str, &str) = ("COPYING", "gstreamer-COPYING.txt");

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
    apply_patches(&source, "wpe-")?;
    let build = dir.join("build");
    let install = wpe_install(&root());
    build_gstreamer(&install)?;
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
        .env("PKG_CONFIG_PATH", pkg_config_path(&install))
        .arg("-S")
        .arg(&source)
        .arg("-B")
        .arg(&build)
        .args(["-G", "Ninja", "-DPORT=WPE", "-DCMAKE_BUILD_TYPE=Release", "-DENABLE_WPE_PLATFORM=ON", "-DENABLE_WPE_PLATFORM_HEADLESS=ON"])
        // WebRTC (calls, Meet) is off in WPE unless asked for; it runs on
        // GStreamer's webrtcbin.
        .args(["-DENABLE_WEB_RTC=ON", "-DUSE_GSTREAMER_WEBRTC=ON"])
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

/// Builds GStreamer into `install`, where WebKit finds it by pkg-config.
/// Needs meson 1.4 or newer (`pip install meson`).
fn build_gstreamer(install: &Path) -> Result<()> {
    let dir = root().join("third_party/gstreamer");
    fs::create_dir_all(&dir)?;
    let source = dir.join("src");
    if !source.is_dir() {
        run(Command::new("git")
            .args(["clone", "-q", "--depth", "1", "-b", GST_VERSION, "https://gitlab.freedesktop.org/gstreamer/gstreamer.git"])
            .arg(&source))?;
    }
    let head = Command::new("git").current_dir(&source).args(["rev-parse", "HEAD"]).output().context("running git")?;
    ensure!(String::from_utf8_lossy(&head.stdout).trim() == GST_COMMIT, "{} isn't GStreamer {GST_VERSION}", source.display());
    apply_patches(&source, "gst-")?;
    let build = dir.join("build");
    let mut setup = Command::new("meson");
    setup.arg("setup");
    if build.join("meson-private").is_dir() {
        setup.arg("--reconfigure");
    }
    setup
        .arg(&build)
        .arg(&source)
        .arg(format!("--prefix={}", install.display()))
        .args(["--libdir=lib", "--buildtype=release", "--wrap-mode=nodownload", "-Dauto_features=disabled", "-Dorc-source=system"])
        .args(GST_FEATURES.iter().map(|f| format!("-D{f}=enabled")))
        // Libraries in lib/, plugins in lib/gstreamer-1.0/ and the plugin
        // scanner in libexec/gstreamer-1.0/ find this GStreamer's libraries.
        .arg("-Dc_link_args=-Wl,-rpath,$ORIGIN:$ORIGIN/..:$ORIGIN/../../lib");
    run(&mut setup).context("configuring GStreamer (missing -dev packages show above)")?;
    let jobs = std::env::var("GATE_JOBS").unwrap_or_else(|_| "4".into());
    run(Command::new("ninja").arg("-C").arg(&build).args(["-j", &jobs])).context("building GStreamer")?;
    run(Command::new("meson").arg("install").arg("-C").arg(&build)).context("installing GStreamer")?;
    let notices = install.join("share/doc/gstreamer");
    fs::create_dir_all(&notices)?;
    fs::copy(source.join("subprojects/gstreamer").join(GST_NOTICE.0), notices.join(GST_NOTICE.0))?;
    Ok(())
}

/// pkg-config's search path with `install` first.
fn pkg_config_path(install: &Path) -> String {
    let ours = install.join("lib/pkgconfig").display().to_string();
    match std::env::var("PKG_CONFIG_PATH") {
        Ok(theirs) if !theirs.is_empty() => format!("{ours}:{theirs}"),
        _ => ours,
    }
}

/// Applies Gate's patches (`xtask/patches/<prefix>*.patch`) to a source
/// tree, each once.
fn apply_patches(source: &Path, prefix: &str) -> Result<()> {
    let mut patches: Vec<PathBuf> = fs::read_dir(root().join("xtask/patches"))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(prefix) && n.ends_with(".patch")))
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


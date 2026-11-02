//! Project tasks.
//!
//! `cargo xtask package [--target <triple>]` builds Gate for distribution:
//!
//! - Linux: `dist/gate_<version>_<arch>.deb`, Gate on WPE WebKit (built by
//!   `cargo xtask build-wpe`), installed to /usr/lib/gate.
//! - Windows: `dist/gate-windows-<arch>.zip`, one folder that runs from
//!   anywhere: gate.exe, Chromium (CEF), uBlock Origin Lite, licenses/.
//!
//! Windows builds natively, or from Linux with cargo-xwin (the MSVC runtime
//! and Windows SDK) and LLVM's clang-cl and lld-link: GPUI compiles its
//! Windows shaders at startup (`runtime_shaders`), so no fxc.exe is needed.
//! `cargo xtask check-windows` is the quick debug build of the same.

use anyhow::{Context as _, Result, bail, ensure};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

mod wpe;
use wpe::{build_wpe, wpe_install};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("package") => {
            let target = match args.get(1).map(String::as_str) {
                Some("--target") => args.get(2).cloned().context("--target needs a triple")?,
                None => host_target(),
                Some(other) => bail!("unknown argument {other}"),
            };
            package(&target)
        }
        Some("check-windows") => check_windows(),
        Some("build-wpe") => build_wpe(),
        _ => bail!("usage: cargo xtask package [--target <triple>] | check-windows | build-wpe"),
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask sits in the workspace").to_path_buf()
}

fn host_target() -> String {
    if cfg!(windows) { "x86_64-pc-windows-msvc" } else { "x86_64-unknown-linux-gnu" }.to_string()
}

fn package(target: &str) -> Result<()> {
    let windows = target.contains("windows");
    ensure!(windows || target.contains("linux"), "no packaging for {target} yet");
    let arch = target.split('-').next().unwrap_or("x86_64");
    let root = root();
    if !windows {
        ensure!(wpe_install(&root).join("lib/libWPEWebKit-2.0.so.1").exists(), "WPE WebKit isn't built (cargo xtask build-wpe)");
    }

    let mut cargo = if windows && !cfg!(windows) {
        xwin()?
    } else {
        let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        cargo.current_dir(&root);
        cargo
    };
    cargo.args(["build", "--package", "gate", "--profile", "dist", "--target", target]);
    run(&mut cargo).context("building Gate")?;

    let archive = if windows { package_windows(&root, target, arch)? } else { package_deb(&root, target, arch)? };
    println!("{}", archive.display());
    Ok(())
}

/// Notices for what every package ships: Gate's fonts, icons and GPUI.
const LICENSES: [(&str, &str); 3] = [
    ("assets/fonts/LICENSE-Geist.txt", "geist-LICENSE.txt"),
    ("assets/icons/LICENSE-Lucide.txt", "lucide-LICENSE.txt"),
    ("vendor/zui/LICENSE-APACHE", "zui-gpui-LICENSE-APACHE.txt"),
];

fn copy_licenses(licenses: &Path, files: impl IntoIterator<Item = (PathBuf, &'static str)>) -> Result<()> {
    fs::create_dir_all(licenses)?;
    let root = root();
    for (from, to) in LICENSES.iter().map(|&(from, to)| (root.join(from), to)).chain(files) {
        if from.is_file() {
            fs::copy(&from, licenses.join(to))?;
        } else {
            eprintln!("note: {} is missing; not in licenses/", from.display());
        }
    }
    Ok(())
}

/// Windows: Gate on Chromium (CEF), as one folder that runs from anywhere,
/// zipped (`dist/gate-windows-<arch>.zip`).
fn package_windows(root: &Path, target: &str, arch: &str) -> Result<PathBuf> {
    let name = format!("gate-windows-{arch}");
    let out = root.join("dist").join(&name);
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;
    fs::copy(root.join("target").join(target).join("dist/gate.exe"), out.join("gate.exe")).context("copying the binary")?;

    let cef = cef_dir(root, "windows", arch)?;
    copy_cef(&cef, &out)?;
    copy_tree(&root.join("extensions/ubol"), &out.join("extensions/ubol"), &|p| p.file_name().is_some_and(|n| n == "_metadata"))?;
    copy_licenses(
        &out.join("licenses"),
        [
            (cef.join("CREDITS.html"), "chromium-CREDITS.html"),
            // CEF's binary downloads leave its license out; kept in the repo.
            (root.join("licenses/cef-LICENSE.txt"), "cef-LICENSE.txt"),
            (root.join("extensions/ubol/LICENSE.txt"), "ublock-origin-lite-LICENSE.txt"),
        ],
    )?;

    let zip = root.join("dist").join(format!("{name}.zip"));
    zip_dir(&out, &name, &zip)?;
    Ok(zip)
}

/// Linux: Gate on WPE WebKit as a Debian package
/// (`dist/gate_<version>_<arch>.deb`). WebKit finds its processes and data
/// relative to its library (xtask/patches/wpe-relocatable.patch), so it
/// lives privately in /usr/lib/gate; the system provides the rest.
fn package_deb(root: &Path, target: &str, arch: &str) -> Result<PathBuf> {
    let deb_arch = match arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => bail!("no Debian architecture for {other}"),
    };
    let version = gate_version(root)?;
    let name = format!("gate_{version}_{deb_arch}");
    let stage = root.join("dist").join(&name);
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    let app = stage.join("usr/lib/gate");
    fs::create_dir_all(app.join("lib"))?;

    let dist = root.join("target").join(target).join("dist");
    fs::copy(dist.join("gate"), app.join("gate")).context("copying the binary")?;
    fs::copy(built_helper(&dist)?, app.join("gate-wpe")).context("copying the WPE helper")?;
    let wpe = wpe_install(root);
    fs::copy(wpe.join("lib/libWPEWebKit-2.0.so.1"), app.join("lib/libWPEWebKit-2.0.so.1"))?;
    for dir in ["lib/wpe-webkit-2.0", "libexec/wpe-webkit-2.0", "share/wpe-webkit-2.0"] {
        copy_tree(&wpe.join(dir), &app.join(dir), &|_| false)?;
    }
    let elves = ["gate-wpe", "lib/libWPEWebKit-2.0.so.1", "lib/wpe-webkit-2.0/injected-bundle/libWPEInjectedBundle.so"]
        .into_iter()
        .map(|f| app.join(f))
        .chain(["WPEWebProcess", "WPENetworkProcess", "WPEGPUProcess"].into_iter().map(|p| app.join("libexec/wpe-webkit-2.0").join(p)))
        .collect::<Vec<_>>();
    for elf in &elves {
        strip(elf)?;
    }

    fs::create_dir_all(stage.join("usr/bin"))?;
    std::os::unix::fs::symlink("../lib/gate/gate", stage.join("usr/bin/gate"))?;
    let applications = stage.join("usr/share/applications");
    fs::create_dir_all(&applications)?;
    fs::write(applications.join("gate.desktop"), DESKTOP_ENTRY)?;
    copy_licenses(
        &stage.join("usr/share/doc/gate/licenses"),
        wpe::NOTICES.map(|(notice, name)| (wpe.join("share/doc/wpe-webkit").join(notice), name)),
    )?;

    let mut binaries = elves;
    binaries.push(app.join("gate"));
    // dpkg-shlibdeps resolves $ORIGIN within a tree with DEBIAN/.
    fs::create_dir_all(stage.join("DEBIAN"))?;
    let depends = shlib_depends(root, &app.join("lib"), &binaries)?;
    let size_kb = tree_size(&stage)? / 1024;
    fs::write(
        stage.join("DEBIAN/control"),
        format!(
            "Package: gate\n\
             Version: {version}\n\
             Architecture: {deb_arch}\n\
             Maintainer: Gate <https://github.com/dignakov/gate-browser>\n\
             Installed-Size: {size_kb}\n\
             Depends: {depends}, glib-networking\n\
             Recommends: gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-gl, gstreamer1.0-libav, xdg-desktop-portal\n\
             Suggests: gstreamer1.0-plugins-bad\n\
             Section: web\n\
             Priority: optional\n\
             Homepage: https://github.com/dignakov/gate-browser\n\
             Description: Web browser on WebKit\n \
             Gate is a keyboard-first web browser built with GPUI on WPE WebKit.\n"
        ),
    )?;

    let deb = root.join("dist").join(format!("{name}.deb"));
    run(Command::new("dpkg-deb").args(["--root-owner-group", "-Zxz", "--build"]).arg(&stage).arg(&deb)).context("building the .deb")?;
    Ok(deb)
}

const DESKTOP_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Gate
GenericName=Web Browser
Comment=Browse the web
Exec=gate %U
Icon=web-browser
Terminal=false
Categories=Network;WebBrowser;
MimeType=text/html;application/xhtml+xml;x-scheme-handler/http;x-scheme-handler/https;
StartupWMClass=gate
StartupNotify=true
";

/// The app's version (crates/app/Cargo.toml).
fn gate_version(root: &Path) -> Result<String> {
    let manifest = fs::read_to_string(root.join("crates/app/Cargo.toml"))?;
    manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .map(str::to_string)
        .context("crates/app/Cargo.toml has no version")
}

/// The `gate-wpe` the web crate's build script made for this build (the
/// newest, should older builds be around).
fn built_helper(dist: &Path) -> Result<PathBuf> {
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = fs::read_dir(dist.join("build"))?
        .filter_map(|e| e.ok().map(|e| e.path().join("out/gate-wpe")))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p).context("the build made no gate-wpe")
}

/// The packages `binaries` need from the system, as Debian's dependency list
/// (dpkg-shlibdeps; `private` holds the libraries the package brings).
fn shlib_depends(root: &Path, private: &Path, binaries: &[PathBuf]) -> Result<String> {
    // dpkg-shlibdeps runs in a source package; a stub stands in for one.
    let work = root.join("dist/.shlibdeps");
    fs::create_dir_all(work.join("debian"))?;
    fs::write(work.join("debian/control"), "Source: gate\n\nPackage: gate\nArchitecture: any\n")?;
    let out = Command::new("dpkg-shlibdeps")
        .current_dir(&work)
        .arg("-O")
        .arg(format!("-l{}", private.display()))
        .args(binaries.iter().map(|b| {
            let mut arg = std::ffi::OsString::from("-e");
            arg.push(b);
            arg
        }))
        .output()
        .context("running dpkg-shlibdeps (apt install dpkg-dev)")?;
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    ensure!(out.status.success(), "dpkg-shlibdeps failed");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().find_map(|l| l.strip_prefix("shlibs:Depends=")).map(str::to_string).context("dpkg-shlibdeps gave no Depends")
}

fn tree_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            total += tree_size(&entry.path())?;
        } else if kind.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// Cross-builds a debug Gate for Windows (quicker than a package).
fn check_windows() -> Result<()> {
    let mut cargo = xwin()?;
    cargo.args(["build", "--package", "gate", "--target", "x86_64-pc-windows-msvc"]);
    run(&mut cargo)
}

/// `cargo xwin`, with the newest LLVM first on the PATH (MSVC's current C++
/// library wants Clang 19 or newer).
fn xwin() -> Result<Command> {
    let llvm = newest_llvm().context("no LLVM in /usr/lib/llvm-* (apt install clang-tools-20 lld-20 llvm-20)")?;
    let path = std::env::join_paths(std::iter::once(llvm.join("bin")).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())))?;
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(root()).env("PATH", path).env("XWIN_ACCEPT_LICENSE", "1").arg("xwin");
    Ok(cargo)
}

/// The highest-numbered /usr/lib/llvm-N that has clang-cl and lld-link.
fn newest_llvm() -> Option<PathBuf> {
    let mut found: Vec<(u32, PathBuf)> = fs::read_dir("/usr/lib")
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter_map(|p| {
            let version = p.file_name()?.to_str()?.strip_prefix("llvm-")?.parse().ok()?;
            (p.join("bin/clang-cl").exists() && p.join("bin/lld-link").exists()).then_some((version, p))
        })
        .collect();
    found.sort();
    found.pop().map(|(_, p)| p)
}

/// The CEF distribution cef-dll-sys downloaded for this platform
/// (`third_party/cef/<version>/cef_<os>_<arch>`).
fn cef_dir(root: &Path, os: &str, arch: &str) -> Result<PathBuf> {
    let want = format!("cef_{os}_{arch}");
    let mut found = Vec::new();
    for version in fs::read_dir(root.join("third_party/cef")).context("third_party/cef: build once first")? {
        let dir = version?.path().join(&want);
        if dir.is_dir() {
            found.push(dir);
        }
    }
    found.sort();
    found.pop().with_context(|| format!("no {want} under third_party/cef (the build downloads it)"))
}

/// CEF's runtime files: everything in the distribution but what is only for
/// building against it (headers, the C++ wrapper, CMake, import libraries)
/// and its sample launchers.
fn copy_cef(cef: &Path, out: &Path) -> Result<()> {
    let build_only = |p: &Path| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        matches!(name, "include" | "cmake" | "libcef_dll" | "CMakeLists.txt" | "archive.json" | "chrome-sandbox" | "README.txt" | "LICENSE.txt" | "CREDITS.html")
            // CEF's sample launchers.
            || name.starts_with("bootstrap")
            || name.ends_with(".lib")
            || name.ends_with(".pdb")
    };
    copy_tree(cef, out, &build_only)
}

fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let path = entry?.path();
        if skip(&path) {
            continue;
        }
        let dest = to.join(path.file_name().expect("entries have names"));
        if path.is_dir() {
            copy_tree(&path, &dest, skip)?;
        } else {
            fs::copy(&path, &dest).with_context(|| format!("copying {}", path.display()))?;
        }
    }
    Ok(())
}

fn strip(file: &Path) -> Result<()> {
    run(Command::new("strip").arg("--strip-unneeded").arg(file)).with_context(|| format!("stripping {}", file.display()))
}

fn zip_dir(dir: &Path, prefix: &str, zip: &Path) -> Result<()> {
    let mut writer = zip::ZipWriter::new(fs::File::create(zip)?);
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    fn add(writer: &mut zip::ZipWriter<fs::File>, dir: &Path, at: &str, options: zip::write::SimpleFileOptions) -> Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = format!("{at}/{}", entry.file_name().to_string_lossy());
            if path.is_dir() {
                writer.add_directory(name.clone(), options)?;
                add(writer, &path, &name, options)?;
            } else {
                writer.start_file(name, options)?;
                writer.write_all(&fs::read(&path)?)?;
            }
        }
        Ok(())
    }
    add(&mut writer, dir, prefix, options)?;
    writer.finish()?;
    Ok(())
}

fn run(command: &mut Command) -> Result<()> {
    let status = command.status().with_context(|| format!("running {command:?}"))?;
    ensure!(status.success(), "{command:?} failed ({status})");
    Ok(())
}

//! Project tasks. `cargo xtask package [--target <triple>]` builds Gate for
//! distribution and lays it out as one folder that runs from anywhere:
//!
//! ```text
//! dist/gate-<os>-<arch>/
//!     gate(.exe)              the browser
//!     libcef.so | libcef.dll  Chromium, and the rest of CEF's runtime files
//!     locales/  *.pak  icudtl.dat  v8_context_snapshot.bin  ...
//!     extensions/ubol/        uBlock Origin Lite, the first copy installed
//!     licenses/               notices for what ships inside
//! ```
//!
//! plus `dist/gate-<os>-<arch>.tar.gz` (Linux) or `.zip` (Windows).
//!
//! Windows packages are built on Windows (CI does): GPUI compiles its
//! shaders with fxc.exe, which only exists there. From Linux,
//! `cargo xtask check-windows` cross-builds a debug Gate with cargo-xwin
//! (debug GPUI compiles shaders at startup instead) to catch errors early.

use anyhow::{Context as _, Result, bail, ensure};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

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
        _ => bail!("usage: cargo xtask package [--target <triple>] | cargo xtask check-windows"),
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
    let os = if windows { "windows" } else if target.contains("linux") { "linux" } else { bail!("no packaging for {target} yet") };
    let arch = target.split('-').next().unwrap_or("x86_64");
    let root = root();

    ensure!(
        !windows || cfg!(windows),
        "Windows packages are built on Windows (GPUI's shaders need fxc.exe); \
         from here, `cargo xtask check-windows` checks the Windows build"
    );
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo.current_dir(&root);
    cargo.args(["build", "--package", "gate", "--profile", "dist", "--target", target]);
    run(&mut cargo).context("building Gate")?;

    let name = format!("gate-{os}-{arch}");
    let out = root.join("dist").join(&name);
    if out.exists() {
        fs::remove_dir_all(&out)?;
    }
    fs::create_dir_all(&out)?;

    let exe = if windows { "gate.exe" } else { "gate" };
    fs::copy(root.join("target").join(target).join("dist").join(exe), out.join(exe)).context("copying the binary")?;

    let cef = cef_dir(&root, os, arch)?;
    copy_cef(&cef, &out)?;
    if !windows {
        // The distribution's libcef.so carries 1.3 GB of debug info.
        strip(&out.join("libcef.so"))?;
    }

    copy_tree(&root.join("extensions/ubol"), &out.join("extensions/ubol"), &|p| p.file_name().is_some_and(|n| n == "_metadata"))?;

    let licenses = out.join("licenses");
    fs::create_dir_all(&licenses)?;
    for (from, to) in [
        (cef.join("CREDITS.html"), "chromium-CREDITS.html"),
        // CEF's binary downloads leave its license out; kept in the repo.
        (root.join("licenses/cef-LICENSE.txt"), "cef-LICENSE.txt"),
        (root.join("extensions/ubol/LICENSE.txt"), "ublock-origin-lite-LICENSE.txt"),
        (root.join("assets/fonts/LICENSE-Geist.txt"), "geist-LICENSE.txt"),
        (root.join("assets/icons/LICENSE-Lucide.txt"), "lucide-LICENSE.txt"),
        (root.join("vendor/zui/LICENSE-APACHE"), "zui-gpui-LICENSE-APACHE.txt"),
    ] {
        if from.is_file() {
            fs::copy(&from, licenses.join(to))?;
        } else {
            eprintln!("note: {} is missing; not in licenses/", from.display());
        }
    }

    let archive = if windows {
        let zip = root.join("dist").join(format!("{name}.zip"));
        zip_dir(&out, &name, &zip)?;
        zip
    } else {
        let tgz = root.join("dist").join(format!("{name}.tar.gz"));
        run(Command::new("tar").current_dir(root.join("dist")).args(["-czf"]).arg(&tgz).arg(&name))?;
        tgz
    };
    println!("{}\n{}", out.display(), archive.display());
    Ok(())
}

/// Cross-builds a debug Gate for Windows with cargo-xwin, which brings the
/// MSVC runtime and Windows SDK; clang-cl and lld-link come from the newest
/// LLVM installed (MSVC's current C++ library wants Clang 19 or newer).
fn check_windows() -> Result<()> {
    let llvm = newest_llvm().context("no LLVM in /usr/lib/llvm-* (apt install clang-tools-20 lld-20 llvm-20)")?;
    let path = std::env::join_paths(std::iter::once(llvm.join("bin")).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())))?;
    let mut cargo = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cargo
        .current_dir(root())
        .env("PATH", path)
        .env("XWIN_ACCEPT_LICENSE", "1")
        .args(["xwin", "build", "--package", "gate", "--target", "x86_64-pc-windows-msvc"]);
    run(&mut cargo)
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
/// building against it (headers, the C++ wrapper, CMake, import libraries).
fn copy_cef(cef: &Path, out: &Path) -> Result<()> {
    let build_only = |p: &Path| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        matches!(name, "include" | "cmake" | "libcef_dll" | "CMakeLists.txt" | "archive.json" | "chrome-sandbox" | "README.txt" | "LICENSE.txt" | "CREDITS.html")
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

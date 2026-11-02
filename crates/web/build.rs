use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(gate_wpe)");
    let cef = std::env::var_os("CARGO_FEATURE_CEF").is_some();
    if cef {
        let dir = std::env::var("DEP_CEF_DLL_WRAPPER_CEF_DIR").expect("cef-dll-sys reports where the CEF distribution is");
        println!("cargo::rustc-env=OPUS_CEF_DIR={dir}");
        println!("cargo::metadata=CEF_DIR={dir}");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") && !build_wpe_helper() && !cef {
        panic!("Linux runs on WPE WebKit: build it first (cargo xtask build-wpe), or add Chromium (--features web/cef)");
    }
}

/// Builds the WPE WebKit helper (`gate-wpe`) against the WPE WebKit that
/// `cargo xtask build-wpe` installs (or `GATE_WPE_PREFIX`): whether it did.
/// `GATE_WPE=0` leaves it out (a Linux build with Chromium only).
fn build_wpe_helper() -> bool {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest.join("src/engine/wpe/helper.c");
    let media = manifest.join("src/js/media.js");
    println!("cargo::rerun-if-changed={}", source.display());
    println!("cargo::rerun-if-changed={}", media.display());
    println!("cargo::rerun-if-env-changed=GATE_WPE");
    println!("cargo::rerun-if-env-changed=GATE_WPE_PREFIX");
    if std::env::var("GATE_WPE").as_deref() == Ok("0") {
        return false;
    }
    let prefix = std::env::var_os("GATE_WPE_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../third_party/wpe/install"));
    let pkgconfig: Vec<PathBuf> = ["lib/pkgconfig", "lib64/pkgconfig", "lib/x86_64-linux-gnu/pkgconfig", "lib/aarch64-linux-gnu/pkgconfig"]
        .iter()
        .map(|d| prefix.join(d))
        .filter(|d| d.is_dir())
        .collect();
    if pkgconfig.is_empty() {
        println!("cargo::warning=WPE WebKit not found at {} (cargo xtask build-wpe)", prefix.display());
        return false;
    }
    println!("cargo::rerun-if-changed={}", pkgconfig[0].display());
    let mut search = std::env::join_paths(&pkgconfig).unwrap();
    if let Some(system) = std::env::var_os("PKG_CONFIG_PATH") {
        search = std::env::join_paths(std::env::split_paths(&search).chain(std::env::split_paths(&system))).unwrap();
    }
    let pkg = |args: &[&str]| -> Option<String> {
        let out = Command::new("pkg-config").env("PKG_CONFIG_PATH", &search).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let packages = ["wpe-webkit-2.0 >= 2.50", "wpe-platform-2.0", "json-glib-1.0", "gbm", "egl", "glesv2", "xkbcommon"];
    let mut args = vec!["--cflags", "--libs"];
    args.extend(packages);
    let Some(flags) = pkg(&args) else {
        println!("cargo::warning=WPE WebKit's development files are incomplete (pkg-config failed)");
        return false;
    };
    let Some(libdir) = pkg(&["--variable=libdir", "wpe-webkit-2.0"]) else {
        println!("cargo::warning=pkg-config gave no libdir for wpe-webkit-2.0");
        return false;
    };

    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    write_c_string(&out.join("gate_media_js.h"), "MEDIA_JS", &std::fs::read(&media).unwrap());
    let helper = out.join("gate-wpe");
    let status = Command::new(std::env::var("CC").unwrap_or_else(|_| "cc".into()))
        .args(["-std=gnu11", "-O2", "-Wall", "-Wextra", "-Wno-unused-parameter", "-Wno-missing-field-initializers"])
        .arg("-I")
        .arg(&out)
        .arg(&source)
        .arg("-o")
        .arg(&helper)
        .args(flags.split_whitespace())
        // WebKit is in lib/ beside the helper: in a package, and here, where
        // lib/ is the install it built against.
        .arg("-Wl,-rpath,$ORIGIN/lib")
        .arg("-lm")
        .status()
        .expect("a C compiler builds the WPE helper");
    if !status.success() {
        panic!("the WPE helper did not compile (GATE_WPE=0 builds without it)");
    }
    let lib = out.join("lib");
    let _ = std::fs::remove_file(&lib);
    std::os::unix::fs::symlink(&libdir, &lib).expect("linking lib/ beside the helper");
    println!("cargo::rustc-env=GATE_WPE_HELPER={}", helper.display());
    println!("cargo::rustc-cfg=gate_wpe");
    true
}

/// `source` as a C string constant `name`.
fn write_c_string(path: &Path, name: &str, source: &[u8]) {
    let mut text = format!("static const char {name}[] = {{");
    for (i, byte) in source.iter().enumerate() {
        if i % 24 == 0 {
            text.push('\n');
        }
        text.push_str(&format!("{byte},"));
    }
    text.push_str("0};\n");
    std::fs::write(path, text).unwrap();
}

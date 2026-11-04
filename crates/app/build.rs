use std::path::{Path, PathBuf};

fn main() {
    // Linux finds libcef.so beside the binary (a packaged Gate), else in the
    // CEF distribution it was built with. Windows looks beside the .exe
    // for DLLs by itself.
    // Only Linux builds with Chromium (the web crate's `cef` feature) have it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && let Ok(dir) = std::env::var("DEP_OPUS_CEF_CEF_DIR")
    {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,{dir}");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_icon();
    }
}

/// The app icon, as resource 1 of the .exe: GPUI loads it for its windows
/// (title bar, taskbar) and Explorer shows it for the file.
fn embed_icon() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/app-icon/light.png");
    println!("cargo::rerun-if-changed={}", source.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let icon = out.join("gate.ico");
    let image = image::open(&source).expect("reading the app icon");
    let file = std::fs::File::create(&icon).expect("writing gate.ico");
    let frames: Vec<_> = [16u32, 20, 24, 32, 40, 48, 64, 128, 256]
        .iter()
        .map(|&size| {
            let frame = image.resize_exact(size, size, image::imageops::FilterType::Lanczos3).into_rgba8();
            image::codecs::ico::IcoFrame::as_png(frame.as_raw(), size, size, image::ExtendedColorType::Rgba8).expect("encoding an icon size")
        })
        .collect();
    image::codecs::ico::IcoEncoder::new(file).encode_images(&frames).expect("writing gate.ico");
    let rc = out.join("gate.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", icon.display().to_string().replace('\\', "\\\\"))).expect("writing gate.rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_required().expect("embedding the app icon");
}

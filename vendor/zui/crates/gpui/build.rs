#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

fn main() {
    println!("cargo::rustc-check-cfg=cfg(gles)");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if target_os == "windows" {
        #[cfg(feature = "windows-manifest")]
        embed_resource();
    }
}

#[cfg(feature = "windows-manifest")]
fn embed_resource() {
    // The resource script is written with the manifest's absolute path:
    // resource compilers resolve relative paths differently (rc.exe from
    // the working directory, llvm-rc from the preprocessed script's folder
    // in OUT_DIR, where a crate-relative path names nothing).
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifest = dir.join("resources/windows/gpui.manifest.xml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc_file = out.join("gpui.rc");
    // Forward slashes: Windows takes them, and they need no escaping here.
    let path = manifest.display().to_string().replace('\\', "/");
    std::fs::write(&rc_file, format!("#define RT_MANIFEST 24\n1 RT_MANIFEST \"{path}\"\n")).unwrap();
    embed_resource::compile(&rc_file, embed_resource::NONE)
        .manifest_required()
        .unwrap();
}

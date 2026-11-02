fn main() {
    let dir = std::env::var("DEP_CEF_DLL_WRAPPER_CEF_DIR")
        .expect("cef-dll-sys reports where the CEF distribution is");
    println!("cargo::rustc-env=OPUS_CEF_DIR={dir}");
    println!("cargo::metadata=CEF_DIR={dir}");
}

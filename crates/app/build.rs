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
}

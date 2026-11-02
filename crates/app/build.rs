fn main() {
    // Linux finds libcef.so beside the binary (a packaged Gate), else in the
    // CEF distribution it was built with. Windows looks beside the .exe
    // for DLLs by itself.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        let dir = std::env::var("DEP_OPUS_CEF_CEF_DIR").expect("the web crate reports the CEF directory");
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,{dir}");
    }
}

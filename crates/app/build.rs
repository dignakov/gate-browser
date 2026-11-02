fn main() {
    // libcef.so stays in the CEF distribution; point the binary at it.
    let dir = std::env::var("DEP_OPUS_CEF_CEF_DIR").expect("the web crate reports the CEF directory");
    println!("cargo::rustc-link-arg-bins=-Wl,-rpath,{dir}");
}

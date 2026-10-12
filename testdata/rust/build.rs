// Links with link.x when building for the bare-metal Cortex-M target.
fn main() {
    println!("cargo:rerun-if-changed=link.x");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("none") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        println!("cargo:rustc-link-search={dir}");
        println!("cargo:rustc-link-arg-bins=-Tlink.x");
    }
}

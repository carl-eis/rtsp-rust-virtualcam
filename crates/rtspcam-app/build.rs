//! Embeds the application manifest (Common Controls v6 visual styles, per-monitor DPI,
//! `asInvoker`) with the MSVC linker, so no resource compiler is needed.

fn main() {
    println!("cargo:rerun-if-changed=res/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}/res/app.manifest");
    }
}

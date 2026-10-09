//! Compiles the Slint UI (`ui/app.slint`) with the same style on every OS. With the MSVC
//! toolchain it also embeds the application manifest (per-monitor DPI, `asInvoker`) and the
//! version information shown in Explorer's Properties dialog.

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("the Slint UI should compile");

    println!("cargo:rerun-if-changed=res/app.manifest");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}/res/app.manifest");

        let mut res = winresource::WindowsResource::new();
        res.set("FileDescription", "RTSP Cam");
        res.set("ProductName", "RTSP Cam");
        res.set("OriginalFilename", "rtspcam.exe");
        res.set("InternalName", "rtspcam");
        res.set("LegalCopyright", "Copyright (c) RTSP Cam contributors");
        res.compile().expect("could not embed version information");
    }
}

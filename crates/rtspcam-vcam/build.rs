//! Links `exports.def` so the COM entry points are exported PRIVATE (silences LNK4104), and
//! embeds the version information shown in Explorer's Properties dialog.

fn main() {
    let def = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("exports.def");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());

        let mut res = winresource::WindowsResource::new();
        res.set("FileDescription", "RTSP Cam virtual camera media source");
        res.set("ProductName", "RTSP Cam");
        res.set("OriginalFilename", "rtspcam_vcam.dll");
        res.set("InternalName", "rtspcam_vcam");
        res.set("LegalCopyright", "Copyright (c) RTSP Cam contributors");
        res.compile().expect("could not embed version information");
    }
    println!("cargo:rerun-if-changed=exports.def");
}

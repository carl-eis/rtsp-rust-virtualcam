//! Links `exports.def` so the COM entry points are exported PRIVATE (silences LNK4104).

fn main() {
    let def = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("exports.def");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def.display());
    }
    println!("cargo:rerun-if-changed=exports.def");
}

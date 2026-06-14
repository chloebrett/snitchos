use std::path::Path;

/// Own the canonical userspace linker script for the whole `user/` workspace.
///
/// Copy `user.ld` into `OUT_DIR` and add that directory to the link search
/// path. Unlike `rustc-link-arg` (which a dependency *cannot* pass on behalf of
/// a downstream binary), a `rustc-link-search` from a dependency build script
/// **does** propagate to the final binary link — so every userspace crate that
/// depends on `snitchos-user` can link with `-Tuser.ld` (name only) and let the
/// linker find it via this path, instead of carrying its own copy of the
/// script. (The cortex-m-rt `link.x` pattern.)
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    let src = Path::new(&manifest).join("user.ld");
    let dst = Path::new(&out).join("user.ld");
    std::fs::copy(&src, &dst).expect("copy user.ld into OUT_DIR");

    println!("cargo:rustc-link-search={out}");
    println!("cargo:rerun-if-changed={}", src.display());
}

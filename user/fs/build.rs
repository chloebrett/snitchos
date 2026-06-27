use std::path::Path;

fn main() {
    // Link with the shared `user.ld` — owned by `snitchos-user` and found via
    // the link-search path its build script publishes (see user/runtime/build.rs),
    // so this crate carries no copy of the script.
    println!("cargo:rustc-link-arg=-Tuser.ld");
    // Strip symbols + debug at link time (keeps the embedded ELF small).
    println!("cargo:rustc-link-arg=-s");

    generate_fs_image_seed();
}

/// Bake every file under `fs-image/` (workspace root) into a `SEED` manifest the
/// seeded server (`fs-server-seeded`) embeds with `include_bytes!`. This is the
/// build-time fs image: drop a file in `fs-image/`, it appears in the seeded
/// filesystem. General — not Stitch-specific.
fn generate_fs_image_seed() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let out = std::env::var("OUT_DIR").unwrap();
    // user/fs → ../../fs-image at the workspace root.
    let image_dir = Path::new(&manifest).join("../../fs-image");
    println!("cargo:rerun-if-changed={}", image_dir.display());

    let mut files: Vec<(String, String)> = match std::fs::read_dir(&image_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter_map(|p| {
                let name = p.file_name()?.to_str()?.to_string();
                let abs = p.canonicalize().ok()?.display().to_string();
                println!("cargo:rerun-if-changed={}", p.display());
                Some((name, abs))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    // Deterministic order for reproducible builds.
    files.sort();

    let mut body = String::from("pub static SEED: &[(&str, &[u8])] = &[\n");
    for (name, abs) in &files {
        body.push_str(&format!("    ({name:?}, include_bytes!({abs:?})),\n"));
    }
    body.push_str("];\n");

    std::fs::write(Path::new(&out).join("fs_seed.rs"), body)
        .expect("write generated fs_seed.rs");
}

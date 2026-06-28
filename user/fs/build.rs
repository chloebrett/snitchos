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

    // Recurse so subdirectories become `/`-joined seed paths (`bin/view`), which
    // `RamFs::seeded` turns into nested directories via mkdir -p.
    let mut files: Vec<(String, String)> = Vec::new();
    collect(&image_dir, "", &mut files);
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

/// Walk `dir`, pushing `(relative-path, absolute-path)` for every file. `prefix`
/// is the `/`-joined path of `dir` relative to the image root (empty at root).
fn collect(dir: &Path, prefix: &str, files: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if path.is_dir() {
            collect(&path, &rel, files);
        } else if let Ok(abs) = path.canonicalize() {
            println!("cargo:rerun-if-changed={}", path.display());
            files.push((rel, abs.display().to_string()));
        }
    }
}

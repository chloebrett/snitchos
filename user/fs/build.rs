fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Link with our own script: fixed low-half VA, position-dependent.
    println!("cargo:rustc-link-arg=-T{dir}/user.ld");
    // Strip symbols + debug at link time (see hello/build.rs) — keeps the
    // embedded ELF small.
    println!("cargo:rustc-link-arg=-s");
    println!("cargo:rerun-if-changed={dir}/user.ld");
}

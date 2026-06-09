fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Link with our own script: fixed low-half VA, position-dependent.
    println!("cargo:rustc-link-arg=-T{dir}/user.ld");
    println!("cargo:rerun-if-changed={dir}/user.ld");
    println!("cargo:rerun-if-changed={dir}/src/start.S");
}

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Link with our own script: fixed low-half VA, position-dependent.
    println!("cargo:rustc-link-arg=-T{dir}/user.ld");
    // Strip symbols + debug at link time. These tiny programs otherwise
    // carry ~700 KB of DWARF that bloats the committed fixtures and the
    // kernel image (which `include_bytes!`s them). The loader only needs
    // the program headers + PT_LOAD bytes, which `-s` keeps.
    println!("cargo:rustc-link-arg=-s");
    println!("cargo:rerun-if-changed={dir}/user.ld");
}

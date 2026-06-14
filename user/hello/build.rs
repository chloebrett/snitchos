fn main() {
    // Link with the shared `user.ld` — owned by `snitchos-user` and found via
    // the link-search path its build script publishes (see user/runtime/build.rs),
    // so this crate carries no copy of the script.
    println!("cargo:rustc-link-arg=-Tuser.ld");
    // Strip symbols + debug at link time. These tiny programs otherwise carry
    // ~700 KB of DWARF that bloats the committed fixtures and the kernel image
    // (which `include_bytes!`s them). The loader only needs the program headers
    // + PT_LOAD bytes, which `-s` keeps.
    println!("cargo:rustc-link-arg=-s");
}

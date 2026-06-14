fn main() {
    // Link with the shared `user.ld` — owned by `snitchos-user` and found via
    // the link-search path its build script publishes (see user/runtime/build.rs),
    // so this crate carries no copy of the script.
    println!("cargo:rustc-link-arg=-Tuser.ld");
    // Strip symbols + debug at link time (keeps the embedded ELF small).
    println!("cargo:rustc-link-arg=-s");
}

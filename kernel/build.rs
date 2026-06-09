fn main() {
  let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
  println!("cargo:rustc-link-arg=-T{}/linker.ld", dir);
  println!("cargo:rerun-if-changed={}/linker.ld", dir);
  println!("cargo:rerun-if-changed={}/src/entry.S", dir);

  // Resolve the userspace ELF to embed (see src/user.rs). `cargo xtask
  // build` compiles `user/hello` first and passes its path via
  // SNITCHOS_USER_ELF; a bare `cargo build -p kernel` leaves it unset and
  // falls back to the committed fixture, so the kernel always builds.
  let user_elf = std::env::var("SNITCHOS_USER_ELF")
    .ok()
    .filter(|p| !p.is_empty())
    .unwrap_or_else(|| format!("{dir}/../kernel-core/fixtures/hello.elf"));
  println!("cargo:rustc-env=SNITCHOS_USER_ELF={user_elf}");
  println!("cargo:rerun-if-changed={user_elf}");
  println!("cargo:rerun-if-env-changed=SNITCHOS_USER_ELF");
}

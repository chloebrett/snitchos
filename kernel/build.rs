fn main() {
  let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
  println!("cargo:rustc-link-arg=-T{}/linker.ld", dir);
  println!("cargo:rerun-if-changed={}/linker.ld", dir);
  println!("cargo:rerun-if-changed={}/src/entry.S", dir);

  // Resolve the userspace ELF to embed (see src/user.rs). `cargo xtask
  // build` compiles `user/hello` first and passes its path via
  // SNITCHOS_USER_ELF; a bare `cargo build -p kernel` leaves it unset and
  // falls back to the committed fixture, so the kernel always builds.
  embed_user_elf("SNITCHOS_USER_ELF", &format!("{dir}/../kernel-core/fixtures/hello.elf"));
  embed_user_elf("SNITCHOS_FAULTER_ELF", &format!("{dir}/../kernel-core/fixtures/faulter.elf"));
}

/// Resolve a userspace ELF path for `include_bytes!`: the path `xtask`
/// passes via `env_var` (the freshly-built artifact), else `fixture` (the
/// committed copy) so a bare `cargo build -p kernel` always compiles.
fn embed_user_elf(env_var: &str, fixture: &str) {
  let path = std::env::var(env_var)
    .ok()
    .filter(|p| !p.is_empty())
    .unwrap_or_else(|| fixture.to_string());
  println!("cargo:rustc-env={env_var}={path}");
  println!("cargo:rerun-if-changed={path}");
  println!("cargo:rerun-if-env-changed={env_var}");
}

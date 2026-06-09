//! Userspace program embedding and (v0.7a Step 4) loading.
//!
//! The first userspace program, `user/hello`, is baked into the kernel
//! image at build time. `build.rs` resolves the path: the freshly-built
//! artifact when building via `cargo xtask build` (which compiles `hello`
//! first and passes `SNITCHOS_USER_ELF`), otherwise the committed fixture
//! `kernel-core/fixtures/hello.elf`.
//!
//! Step 4 parses this with [`kernel_core::elf`] and maps the segments into
//! a user address space, then `sret`s to its entry. For now it is just the
//! embedded bytes — proving the embed path compiles end to end.

/// The embedded `user/hello` ELF image (a static, position-dependent
/// RISC-V executable linked at `0x1000_0000`).
#[allow(dead_code, reason = "consumed by the v0.7a Step 4 userspace loader")]
pub static HELLO_ELF: &[u8] = include_bytes!(env!("SNITCHOS_USER_ELF"));

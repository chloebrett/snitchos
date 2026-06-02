//! Test harness: spawns QEMU, reads the virtio-console socket on a
//! reader thread, decodes frames, and surfaces them to the main
//! (assertion) thread via a channel. See step 3 in
//! `plans/kernel-integration-tests.md`.
//!
//! Skeleton — fleshed out in step 3.

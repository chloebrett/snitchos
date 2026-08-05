//! `kvetch-client` — asks the completion server to continue a fixed prefix,
//! then emits what came back so the itest can assert on it.
//!
//! The prefix and token budget are fixed, and the server's seed derives from a
//! recorded boot root plus a request counter, so the completion is
//! byte-identical on snemu, QEMU and hardware. That equality is what the
//! `kvetch-babble-serves` scenario checks against the *host* sampler.

#![no_std]
#![no_main]

use kvetch_proto::{Complete, Reply, Status};
use snitchos_user::{
    Endpoint, delegated_handle, entry, exit_with, register_counter, tracer,
};

/// The prefix to complete. Mid-declaration on purpose: it exercises the
/// interesting case, where a completion is a legal *fragment* rather than a
/// whole program.
const PREFIX: &str = "greet(name) {";

/// Room for the prefix plus whatever comes back.
const CAP: usize = 256;

/// Tokens to ask for.
///
/// **Four**, and the number is set by what the byte-identity oracle can *discriminate*
/// rather than by what the path needs. Measured against the committed checkpoint, for
/// two unrelated prefixes:
///
/// ```text
/// 1 token   "\n   "              "\n   "              same
/// 2 tokens  "\n    let"          "\n    //"           differ
/// 4 tokens  "\n    let water ="  "\n    // Sort by"   differ
/// ```
///
/// At one token the answer is a newline and an indent whatever you ask, because a
/// code model's first move is always to start a line — so byte-identity against it is
/// nearly a tautology, and the negative control (recompute with the *wrong* prefix)
/// passes. Two is the minimum that discriminates; four is that with margin, and covers
/// more arithmetic per run.
///
/// It stays small because the cost was once superlinear: without a KV cache each token
/// re-ran a forward pass over the whole prefix, and eight tokens cost ~90s. The cache
/// made the marginal token cheap, which is what makes four affordable. Deeper sampler
/// behaviour — truncation, viability, every buffer size — is covered in the host tests
/// where it costs nothing.
const MAX_TOKENS: u32 = 4;

#[entry]
fn main() {
    let _span = tracer().span("kvetch.client");
    let mut buf = [0u8; CAP];
    buf[..PREFIX.len()].copy_from_slice(PREFIX.as_bytes());

    let request = Complete {
        max_tokens: MAX_TOKENS,
        ptr: buf.as_ptr() as u64,
        cap: CAP as u32,
        prefix_len: PREFIX.len() as u32,
    };

    let Ok((words, _)) = Endpoint::from_raw_handle(delegated_handle(0)).call(request.encode())
    else {
        exit_with(2)
    };
    let Ok(Reply { status, written }) = Reply::decode(words) else {
        exit_with(3)
    };
    if status != Status::Ok {
        exit_with(4)
    }

    // Put the answer on the wire for the harness: how many bytes came back, and
    // a checksum of them. The scenario recomputes both from the host sampler —
    // equality is the cross-engine determinism claim.
    register_counter("snitchos.kvetch.client.written").emit(i64::from(written));
    let completion = &buf[PREFIX.len()..PREFIX.len() + written as usize];
    register_counter("snitchos.kvetch.client.checksum").emit(checksum(completion));
    exit_with(0)
}

/// FNV-1a over the completion bytes — a compact way to assert "the same bytes"
/// through a metric, which carries one integer.
fn checksum(bytes: &[u8]) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Fold to a positive i64 so the metric carries it unambiguously.
    (hash >> 1) as i64
}

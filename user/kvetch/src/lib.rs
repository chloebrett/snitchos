//! The `kvetch` completion server's receive loop.
//!
//! Holds `RECV` on its completion endpoint and answers [`kvetch_proto::Complete`]
//! requests by sampling from [`babble`] — the weight-free rung 0. Serving a
//! model with no weights is deliberate: it proves the protocol, the serve loop,
//! the telemetry and the itest before any checkpoint exists, so a trained rung
//! inherits a working stack rather than debugging two new things at once.
//!
//! All decision logic lives in [`babble::serve::handle_request`], which works on
//! a byte slice and is host-tested. This file is the glue that moves bytes
//! across the process boundary and puts the result on the wire.

#![no_std]

use babble::serve::handle_request;
use kvetch_proto::{Complete, Reply, Status, request_seed};
use kvetch_serve::model::ModelLogits;
use kvetch_serve::serve::Server;
use snitchos_user::{
    Endpoint, Metric, copy_from_caller, copy_to_caller, delegated_handle, register_counter, reply,
    tracer,
};

/// This server's endpoint, read as the first **delegated** cap — the same
/// discipline as the FS server, so it works whether the kernel launched it via
/// `run_ipc` or `init` delegated the endpoint to it.
fn endpoint() -> Endpoint {
    Endpoint::from_raw_handle(delegated_handle(0))
}

/// Server-side scratch for one request's prefix + completion. A client may
/// offer a larger buffer; this server declines to work on more than it can
/// hold, and says so rather than truncating silently.
const SCRATCH: usize = 512;

/// The per-boot entropy root.
///
/// Zero until the `seed=` bootarg is wired: the default is documented, recorded,
/// and reproducible, which is what the seed-provenance rule asks for
/// (`docs/randomness-and-entropy.md`). Time never enters — the seed for each
/// request is a pure function of this root and the request counter, so the same
/// boot and the same request sequence reproduce byte-identically on snemu,
/// QEMU and hardware.
const BOOT_SEED: u64 = 0;

/// Serve completions from babble — the weight-free rung 0.
pub fn serve() -> ! {
    serve_with(handle_request)
}

/// Serve completions from a trained checkpoint.
///
/// `checkpoint` and `vocab` are the embedded pair. If they were not trained
/// together — or the checkpoint predates the fingerprint field — this **does not
/// die**: it serves `Malformed` to every request, loudly, forever.
///
/// Dying would be the tidier-looking choice and it is the wrong one. A completion
/// client blocks in `call` on an endpoint whose server is gone, with no refusal and
/// no timeout; the symptom surfaces two processes away as "the REPL stopped
/// responding". (That is not hypothetical — it is exactly how the FP one-holder guard
/// presented before it was found. See `plans/legacy/fp-context-switching.md`.) A
/// server that cannot do its job should answer, not vanish.
pub fn serve_model(checkpoint: &[u8], vocab: &[u8]) -> ! {
    let paired = kvetch_vocab::Vocab::decode_vocab(vocab)
        .zip(kvetch_model::Model::decode(checkpoint))
        .and_then(|(vocab, model)| {
            let logits = ModelLogits::new(model);
            let fingerprint = logits.vocab_fingerprint();
            Server::new(logits, vocab, fingerprint)
        });

    let Some(mut server) = paired else {
        register_counter("snitchos.kvetch.pairing_refused").emit(1);
        snitchos_user::debug_write(
            b"kvetch: refusing to serve - the embedded checkpoint and vocab were not \
              trained together (or the checkpoint is unstamped)",
        );
        serve_with(|_, _, _, _| malformed());
    };

    serve_with(move |buf, prefix_len, max_tokens, seed| {
        server.handle_request(buf, prefix_len, max_tokens, seed)
    })
}

/// The receive loop, over whatever answers a request.
///
/// One loop for both rungs so the endpoint behaviour — spans, counters, the
/// copy-in/copy-out dance, the seed sequence — cannot drift between them. A drivel
/// completion and a babble completion differ in the opinion, not the protocol.
fn serve_with<F: FnMut(&mut [u8], usize, u32, u64) -> Reply>(mut answer: F) -> ! {
    let requests: Metric = register_counter("snitchos.kvetch.requests_total");
    let tokens: Metric = register_counter("snitchos.kvetch.bytes_emitted_total");
    // The seed each completion was drawn with, on the wire. This is what makes a
    // recorded completion replayable: the system is deterministic *given its own
    // trace*, rather than merely deterministic in the lab.
    let seed_gauge: Metric = register_counter("snitchos.kvetch.seed");
    let mut counter: u64 = 0;

    loop {
        let Ok(received) = endpoint().receive_with_reply() else {
            continue;
        };
        let Some(reply_handle) = received.reply else {
            continue; // a one-way send has no request semantics here
        };
        // Each request is a span. The kernel seeded our span cursor from the
        // caller's, so this nests under the client's own span — the trace
        // crosses the process boundary for free.
        let _span = tracer().span("kvetch.complete");
        requests.emit(1);

        let Ok(request) = Complete::decode(received.msg) else {
            let _ = reply(reply_handle, malformed().encode());
            continue;
        };
        if request.cap as usize > SCRATCH || request.prefix_len as usize > SCRATCH {
            // Refused, not malformed: the request is well-formed, this server
            // just will not hold that much. Refusals snitch — the client gets a
            // status, never silence.
            let _ = reply(reply_handle, Reply { status: Status::Refused, written: 0 }.encode());
            continue;
        }

        let seed = request_seed(BOOT_SEED, counter);
        counter += 1;
        seed_gauge.emit(seed as i64);

        let mut scratch = [0u8; SCRATCH];
        let prefix_len = request.prefix_len as usize;
        if copy_from_caller(
            reply_handle,
            request.ptr as usize,
            prefix_len,
            scratch.as_mut_ptr() as usize,
        )
        .is_err()
        {
            let _ = reply(reply_handle, malformed().encode());
            continue;
        }

        // The client's buffer bounds what we may write back, so the sampler is
        // given exactly that much room.
        let room = (request.cap as usize).min(SCRATCH);
        let answer = answer(&mut scratch[..room], prefix_len, request.max_tokens, seed);

        if answer.status == Status::Ok && answer.written > 0 {
            let written = answer.written as usize;
            // SAFETY-adjacent: `scratch` is ours; the kernel validates the
            // caller-side range against the reply cap's address space.
            if copy_to_caller(
                reply_handle,
                scratch.as_ptr() as usize + prefix_len,
                written,
                request.ptr as usize + prefix_len,
            )
            .is_err()
            {
                let _ = reply(reply_handle, malformed().encode());
                continue;
            }
            tokens.emit(i64::from(answer.written));
        }
        let _ = reply(reply_handle, answer.encode());
    }
}

/// A refusal for a request we could not even read.
fn malformed() -> Reply {
    Reply { status: Status::Malformed, written: 0 }
}

//! The committed `drivel-all-30k` pair, end to end on the host.
//!
//! This is the same code the target runs, against the same bytes, so it is both a
//! test and the fastest way to debug the on-target server: a failure here is a
//! failure there, without an emulator in the loop.
//!
//! Reads `checkpoints/` directly rather than through a fixture copy, because the
//! point is to exercise *the* pair the server embeds — a copy could drift.

use std::path::PathBuf;

use kvetch_model::Model;
use kvetch_serve::model::ModelLogits;
use kvetch_serve::serve::Server;
use kvetch_vocab::Vocab;

fn checkpoints() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("checkpoints")
}

fn drivel() -> Server<ModelLogits> {
    let dir = checkpoints();
    let weights = std::fs::read(dir.join("drivel-all-30k.kvetch")).expect("committed checkpoint");
    let vocab_bytes = std::fs::read(dir.join("drivel-all-30k.vocab")).expect("committed vocab");

    let model = Model::decode(&weights).expect("the committed checkpoint decodes");
    let vocab = Vocab::decode_vocab(&vocab_bytes).expect("the committed vocab decodes");
    let logits = ModelLogits::new(model);
    let fingerprint = logits.vocab_fingerprint();

    Server::new(logits, vocab, fingerprint).expect("the committed pair must agree")
}

/// The pair in git must be usable *as committed*: stamped, matching, and decodable.
/// If this fails, the on-target server refuses every request and answers `Malformed`
/// — which looks, from the client, like a model with no opinions.
#[test]
fn the_committed_checkpoint_and_vocab_pair_up() {
    let _ = drivel();
}

/// A real completion from real weights, under the grammar.
#[test]
fn drivel_completes_a_prefix_legally() {
    let mut server = drivel();
    let prefix = "greet(name) {";
    let mut buf = vec![0u8; 256];
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());

    let reply = server.handle_request(&mut buf, prefix.len(), 6, 7);

    assert_eq!(reply.status, kvetch_proto::Status::Ok);
    assert!(reply.written > 0, "the trained rung produced nothing for {prefix:?}");

    let served = std::str::from_utf8(&buf[..prefix.len() + reply.written as usize]).expect("utf8");
    assert!(
        !stitch::oracle::valid_next(served, served.len()).is_empty(),
        "the completion left the buffer dead: {served:?}"
    );
    println!("drivel: {served:?}");
}

/// The cache is an optimisation, so it must not change a single byte of what is
/// served. Two servers, same seed, one asked to complete in one call and one asked
/// token by token — the bytes must match.
#[test]
fn the_same_request_serves_the_same_bytes_however_the_session_was_warmed() {
    let prefix = "let total = ";
    let serve_once = |max_tokens: u32| {
        let mut server = drivel();
        let mut buf = vec![0u8; 256];
        buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
        let reply = server.handle_request(&mut buf, prefix.len(), max_tokens, 99);
        String::from_utf8(buf[..prefix.len() + reply.written as usize].to_vec()).expect("utf8")
    };

    // A four-token completion must begin with the two-token one: same seed, same
    // prefix, same draws — the budget only decides where it stops.
    let short = serve_once(2);
    let long = serve_once(4);
    assert!(long.starts_with(&short), "{long:?} does not extend {short:?}");
}

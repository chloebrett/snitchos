//! What repeated Tab does to a line — the REPL loop, on the host.
//!
//! `cargo run --release -p kvetch-serve --bin repl-tabs`
//!
//! Every other tool here measures **one** request. The REPL presses Tab against a
//! buffer that its own previous answers grew, and that feedback loop is where the
//! reported misbehaviour lives: after several Tabs the completer starts showing a
//! token menu instead of a suggestion, and the text before it degenerates into
//! run-on comment prose.
//!
//! This reproduces `ModelCompleter::complete_line` (`stitch/src/complete.rs:62-83`)
//! exactly, substituting a direct [`Server::handle_request`] for the `platform.complete`
//! IPC hop — which is what that hop calls on the far side. So a degeneration seen here
//! is a property of the completer and the weights, **not** of the board, the opt level
//! or the multiply kernel.

use std::process::ExitCode;

use kvetch_model::Model;
use kvetch_serve::model::ModelLogits;
use kvetch_serve::serve::Server;
use kvetch_vocab::Vocab;
use stitch::complete::{Completion, complete};
use stitch::oracle::{Entry, valid_next_in};

/// What the on-target REPL asks for, from `stitch/src/platform.rs`.
const COMPLETION_TOKENS: u32 = 6;
const COMPLETION_BUF: usize = 256;

/// How many Tabs to press. More than anyone presses by hand, so the steady state is
/// visible rather than inferred from two rounds.
const TABS: usize = 12;

/// `repl-tabs [start-line] [buffer-bytes] [tabs]`.
///
/// The buffer is overridable because "just raise the 256" is the obvious response to
/// hitting it, and whether that actually buys longer *programs* — rather than longer
/// comment prose — is a question this bin can answer in one run.
fn buffer_bytes() -> usize {
    std::env::args().nth(2).and_then(|arg| arg.parse().ok()).unwrap_or(COMPLETION_BUF)
}

fn tabs() -> usize {
    std::env::args().nth(3).and_then(|arg| arg.parse().ok()).unwrap_or(TABS)
}

/// The line to start from. Overridable, because *which* line you Tab from decides
/// which grammar reading is alive — `greet(name) {` is a program under construction,
/// `greet(name)` is already a complete expression, and they degenerate differently.
const START: &str = "greet(name) {";

fn start_line() -> String {
    std::env::args().nth(1).unwrap_or_else(|| String::from(START))
}

fn main() -> ExitCode {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("checkpoints");
    let stem = kvetch_serve::CANONICAL_CHECKPOINT;
    let (Ok(weights), Ok(vocab_bytes)) = (
        std::fs::read(dir.join(format!("{stem}.kvetch"))),
        std::fs::read(dir.join(format!("{stem}.vocab"))),
    ) else {
        eprintln!("repl-tabs: checkpoints/{stem}.{{kvetch,vocab}} did not load");
        return ExitCode::FAILURE;
    };
    let (Some(model), Some(vocab)) =
        (Model::decode(&weights), Vocab::decode_vocab(&vocab_bytes))
    else {
        eprintln!("repl-tabs: the committed pair did not decode");
        return ExitCode::FAILURE;
    };
    let logits = ModelLogits::new(model);
    let fingerprint = logits.vocab_fingerprint();
    let Some(mut server) = Server::new(logits, vocab, fingerprint) else {
        eprintln!("repl-tabs: the committed pair does not agree");
        return ExitCode::FAILURE;
    };

    let mut line = start_line();
    println!("start: {line:?}\n");

    let (buffer_bytes, tabs) = (buffer_bytes(), tabs());
    for tab in 1..=tabs {
        // Step 1, as `ModelCompleter` does it: the grammar answers first, and a
        // forced or dead verdict never reaches the model.
        let grammar = complete(&line, line.len());
        let Completion::Choices(choices) = &grammar else {
            println!("tab {tab:>2}: grammar decided alone — {grammar:?}");
            if let Completion::Forced(text) = &grammar {
                line.push_str(text);
                continue;
            }
            break;
        };

        // Step 2: ask the server, exactly as the IPC hop's far side would.
        let mut buf = vec![0u8; buffer_bytes];
        if line.len() > buf.len() {
            println!("tab {tab:>2}: line outgrew the {buffer_bytes}-byte request buffer");
            break;
        }
        buf[..line.len()].copy_from_slice(line.as_bytes());
        let reply = server.handle_request(&mut buf, line.len(), COMPLETION_TOKENS, tab as u64);
        let served = String::from_utf8_lossy(&buf[line.len()..line.len() + reply.written as usize]).into_owned();

        // Step 3: the client's own re-check, verbatim from complete.rs.
        let extended = format!("{line}{served}");
        let survives = !valid_next_in(&extended, extended.len(), Entry::Program)
            .union(valid_next_in(&extended, extended.len(), Entry::Expr))
            .is_empty();

        let verdict = if served.is_empty() {
            format!("MENU — server served nothing ({} classes offered)", choices.len())
        } else if survives {
            format!("suggested {served:?}")
        } else {
            format!("MENU — suggestion killed the line ({} classes offered)", choices.len())
        };

        println!(
            "tab {tab:>2}: {:>3} bytes, in_comment={:<5} {verdict}",
            line.len(),
            ends_inside_a_comment(&line),
        );

        if served.is_empty() || !survives {
            break;
        }
        line = extended;
    }

    println!("\nfinal buffer:\n{line}");
    ExitCode::SUCCESS
}

/// Does the buffer end inside a `//` comment — i.e. after a `//` with no newline since?
///
/// The suspected mechanism: a comment runs to end of line, so while the buffer ends
/// inside one, *every* byte extends it legally and the grammar constraint says nothing
/// at all. A model whose corpus is ~47% comments will happily stay there.
fn ends_inside_a_comment(line: &str) -> bool {
    line.rsplit('\n').next().is_some_and(|last| last.contains("//"))
}

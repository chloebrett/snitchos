//! The harness, driven with the model absent.
//!
//! Every stage below the model call must be testable without one — that is what
//! the `Model` trait exists for, and a fake responder is what proves it.

use cram_gen::{Model, extract, prompt, run_once};

/// A model that returns whatever it was handed, one chunk at a time so the
/// streaming path is exercised without a server.
struct Fake(String);

impl Model for Fake {
    fn complete(
        &self,
        _system: &str,
        _user: &str,
        on_chunk: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        for chunk in self.0.split_inclusive('\n') {
            on_chunk(chunk);
        }
        Ok(self.0.clone())
    }
}

const GOOD: &str = "\
ext double(n: Int) -> Int = n * 2

test \"double doubles\" { expect double(2) == 4 }
";

#[test]
fn extracts_a_fenced_block_and_drops_the_prose_around_it() {
    let raw = format!("Here you go:\n\n```stitch\n{GOOD}```\n\nHope that helps!");
    let got = extract(&raw);
    assert_eq!(got.program.trim(), GOOD.trim());
    assert_eq!(got.extra_blocks, 0);
}

#[test]
fn accepts_a_bare_fence_without_a_language_tag() {
    let raw = format!("```\n{GOOD}```");
    assert_eq!(extract(&raw).program.trim(), GOOD.trim());
}

/// Reasoning models emit a `<think>` block that is never part of the program —
/// and which routinely *quotes* the prompt, fences and all, so it must be
/// removed before any fence-hunting happens rather than after.
#[test]
fn a_reasoning_block_is_stripped_before_fences_are_looked_for() {
    let raw = format!(
        "<think>\nPlanning. The prompt said: Exactly one fenced ```stitch block.\n</think>\n\n```stitch\n{GOOD}```"
    );
    let got = extract(&raw);
    assert_eq!(got.program.trim(), GOOD.trim());
    assert_eq!(got.extra_blocks, 0);
}

/// Hitting the token cap mid-thought leaves an unterminated `<think>` and no
/// program at all. That is an extraction failure, not a Stitch failure — and
/// emphatically not an empty program, which would parse clean and be counted a
/// success.
#[test]
fn an_unterminated_reasoning_block_yields_no_program() {
    let got = extract("<think>\nStill planning when the cap hit. `prod` `ext`");
    assert!(got.program.trim().is_empty(), "got {:?}", got.program);
}

/// A response with no fence at all is still a candidate — the fence is how the
/// model was *asked* to reply, not a precondition for the program being real.
#[test]
fn treats_an_unfenced_response_as_the_whole_program() {
    assert_eq!(extract(GOOD).program.trim(), GOOD.trim());
}

/// Extra blocks are a prompt failure, not a Stitch failure, so they get their
/// own counter rather than being silently dropped.
#[test]
fn counts_extra_blocks_rather_than_silently_dropping_them() {
    let raw = format!("```stitch\n{GOOD}```\n\nand an alternative:\n\n```stitch\next f() -> Int = 1\n```");
    let got = extract(&raw);
    assert_eq!(got.program.trim(), GOOD.trim());
    assert_eq!(got.extra_blocks, 1);
}

/// The reference and the prelude are *derived* from the language, never
/// maintained beside it — a stale prompt does not fail loudly, it silently caps
/// program quality.
#[test]
fn the_user_prompt_carries_the_prelude_the_natives_and_the_task() {
    let user = prompt::user("Write a widget module.");
    assert!(user.contains("Write a widget module."), "task missing");
    assert!(user.contains("unwrapOr(m, default)"), "prelude missing");
    assert!(user.contains("sortBy(xs, key)"), "built-ins missing");
    // `summarise` appears only in stats.st — `ext prod Summary` would also match
    // the reference, so it proves nothing about the exemplars being included.
    assert!(user.contains("ext summarise(xs: List<Int>)"), "exemplar missing");
}

#[test]
fn the_system_prompt_states_the_rules_that_priors_break() {
    let system = prompt::system();
    assert!(system.contains("and"), "boolean rule missing");
    assert!(system.contains("no `return`") || system.contains("no return"));
}

#[test]
fn a_fake_model_drives_the_whole_pipeline_to_a_verdict() {
    let run = run_once(&Fake(format!("```stitch\n{GOOD}```")), "anything", &mut |_| {})
        .expect("fake succeeds");
    assert_eq!(run.outcome.stage(), "ok");
    assert_eq!(run.extra_blocks, 0);
    assert!(run.program.contains("double"));
}

/// A long generation should narrate itself rather than arriving all at once —
/// the same principle as the training loop reporting on itself.
#[test]
fn chunks_reach_the_caller_as_they_arrive_and_assemble_to_the_whole() {
    let raw = format!("```stitch\n{GOOD}```");
    let mut seen = String::new();
    let run = run_once(&Fake(raw.clone()), "anything", &mut |chunk| seen.push_str(chunk))
        .expect("fake succeeds");
    assert!(seen.len() > 1, "expected more than one chunk");
    assert_eq!(seen, raw, "streamed chunks must reassemble to the raw response");
    assert_eq!(run.raw, raw);
}

/// The wire format is server-sent events: each frame carries an incremental
/// `delta`, and the stream ends with a sentinel rather than a frame.
#[test]
fn an_sse_frame_yields_its_delta_and_the_sentinel_yields_nothing() {
    let frame = r#"data: {"choices":[{"delta":{"content":"ext "}}]}"#;
    assert_eq!(cram_gen::sse_delta(frame).as_deref(), Some("ext "));

    assert_eq!(cram_gen::sse_delta("data: [DONE]"), None);
    assert_eq!(cram_gen::sse_delta(""), None);
    // The frame that opens a message carries a role and no content.
    assert_eq!(
        cram_gen::sse_delta(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#),
        None
    );
}

/// The funnel is the product: a single yield percentage collapses four
/// different actions into one shrug, so every stage is reported separately.
#[test]
fn the_funnel_reports_every_stage_rather_than_one_number() {
    let mut tally = cram_gen::Tally::default();
    tally.record(&stitch::gate::run("ext f( = 1"));
    tally.record(&stitch::gate::run(GOOD));
    tally.record(&stitch::gate::run(GOOD));
    tally.record_error();

    let funnel = tally.funnel(4);
    assert!(funnel.contains("4 attempted"), "{funnel}");
    assert!(funnel.contains("parse 1"), "{funnel}");
    assert!(funnel.contains("ok 2"), "{funnel}");
    assert!(funnel.contains("model errors 1"), "{funnel}");
}

#[test]
fn a_broken_program_reaches_the_gate_and_reports_its_stage() {
    let run = run_once(&Fake("```stitch\next f( = 1\n```".into()), "anything", &mut |_| {})
        .expect("fake succeeds");
    assert_eq!(run.outcome.stage(), "parse");
}

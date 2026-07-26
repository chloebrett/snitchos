//! The harness, driven with the model absent.
//!
//! Every stage below the model call must be testable without one — that is what
//! the `Model` trait exists for, and a fake responder is what proves it.

use cram_gen::{Model, extract, prompt, run_once};

/// A model that returns whatever it was handed, and records what it was asked.
struct Fake(String);

impl Model for Fake {
    fn complete(&self, _system: &str, _user: &str) -> Result<String, String> {
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
    let run = run_once(&Fake(format!("```stitch\n{GOOD}```")), "anything").expect("fake succeeds");
    assert_eq!(run.outcome.stage(), "ok");
    assert_eq!(run.extra_blocks, 0);
    assert!(run.program.contains("double"));
}

#[test]
fn a_broken_program_reaches_the_gate_and_reports_its_stage() {
    let run = run_once(&Fake("```stitch\next f( = 1\n```".into()), "anything").expect("fake succeeds");
    assert_eq!(run.outcome.stage(), "parse");
}

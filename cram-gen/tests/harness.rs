//! The harness, driven with the model absent.
//!
//! Every stage below the model call must be testable without one — that is what
//! the `Model` trait exists for, and a fake responder is what proves it.

use cram_gen::{Model, extract, prompt, run_once, run_once_guarded};

/// A model that returns whatever it was handed, one chunk at a time so the
/// streaming path is exercised without a server.
struct Fake(String);

impl Model for Fake {
    fn complete(
        &self,
        _system: &str,
        _user: &str,
        on_chunk: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String> {
        let mut sent = String::new();
        for chunk in self.0.split_inclusive('\n') {
            sent.push_str(chunk);
            if !on_chunk(chunk) {
                // A real server stops generating here, so the fake must too —
                // otherwise the guard looks like it works and does not.
                return Ok(sent);
            }
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

/// The continuation oracle answers "can any token rescue this prefix?" — which
/// is what lets generation stop the instant a candidate is doomed instead of
/// paying for the rest of it.
#[test]
fn a_doomed_prefix_is_detected_and_a_partial_one_is_not() {
    // Each of these killed a real candidate in corpora/batch1.
    for doomed in [
        "ext Time = Int",                 // no type aliases
        "cond overlap(b1: Booking",       // `cond` read as a keyword
        "xs |> filter((c: TimeWindow",    // type-annotated lambda parameter
    ] {
        assert!(cram_gen::is_doomed(doomed), "should be doomed: {doomed:?}");
    }

    // Nothing mid-token or merely incomplete may be rejected, or generation
    // would abort constantly on its own healthy output.
    for alive in [
        "ext f() -> Int = 1",
        "ext f() -> Int = { ex",                          // partial lexeme
        "ext overlaps(a: Booking) -> Bool = { a.start <", // incomplete expression
        "",
    ] {
        assert!(!cram_gen::is_doomed(alive), "should be alive: {alive:?}");
    }
}

/// Guarding stops the stream at the first fatal token. Candidate 004 spent 1162
/// tokens after `cond` had already killed it.
#[test]
fn guarding_abandons_a_candidate_at_the_token_that_killed_it() {
    let raw = "```stitch\next Time = Int\nand then a great deal more text\n";
    let run = run_once_guarded(&Fake(raw.into()), "anything", &mut |_| {})
        .expect("fake succeeds");
    assert!(run.abandoned, "should have been abandoned");
    assert!(
        !run.program.contains("a great deal more"),
        "generation should have stopped: {:?}",
        run.program
    );
}

/// A healthy candidate must be untouched by the guard.
#[test]
fn guarding_leaves_a_good_program_alone() {
    let run = run_once_guarded(&Fake(format!("```stitch\n{GOOD}```")), "anything", &mut |_| {})
        .expect("fake succeeds");
    assert!(!run.abandoned);
    assert_eq!(run.outcome.stage(), "ok");
}

/// Throughput is the number that decides whether a 500k-token corpus is hours
/// or days, so it has to come off the run rather than be estimated afterwards.
/// One SSE frame is one token, so counting frames counts tokens.
#[test]
fn a_run_counts_the_tokens_it_streamed() {
    let raw = "one\ntwo\nthree\n";
    let run = run_once(&Fake(raw.into()), "anything", &mut |_| {}).expect("fake succeeds");
    assert_eq!(run.tokens, 3, "one chunk per line from the fake");
}

/// The batch record has to survive the terminal. A funnel scrolled past is not
/// evidence, and the whole point of keeping failures is that they are the
/// scarcest input the RL branch has.
#[test]
fn a_candidate_record_serialises_the_whole_funnel_state() {
    let record = cram_gen::CandidateRecord {
        index: 7,
        stage: "parse".into(),
        detail: "unexpected character `&`".into(),
        tokens: 412,
        seconds: 9.5,
        reasoned: true,
        extra_blocks: 1,
    };
    let json = serde_json::to_string(&record).expect("serialises");
    for expected in
        ["\"index\":7", "\"stage\":\"parse\"", "\"tokens\":412", "\"reasoned\":true"]
    {
        assert!(json.contains(expected), "{expected} missing from {json}");
    }
}

/// Whether the server actually honoured "no thinking" is a fact about the run,
/// not something the operator should have to infer from a wall of prose.
#[test]
fn a_run_records_whether_the_response_contained_reasoning() {
    let thought = run_once(
        &Fake(format!("<think>\nplanning\n</think>\n\n```stitch\n{GOOD}```")),
        "anything",
        &mut |_| {},
    )
    .expect("fake succeeds");
    assert!(thought.reasoned, "a <think> block should be recorded");

    let direct = run_once(&Fake(format!("```stitch\n{GOOD}```")), "anything", &mut |_| {})
        .expect("fake succeeds");
    assert!(!direct.reasoned);
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

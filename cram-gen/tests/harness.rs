//! The harness, driven with the model absent.
//!
//! Every stage below the model call must be testable without one — that is what
//! the `Model` trait exists for, and a fake responder is what proves it.

use cram_gen::{Model, extract, prompt, run_once, run_once_corrected, run_once_guarded};

/// A model that returns whatever it was handed, one chunk at a time so the
/// streaming path is exercised without a server.
struct Fake(String);

impl Model for Fake {
    fn complete(
        &self,
        _system: &str,
        _user: &str,
        _prefill: &str,
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

/// A model that answers differently each call, so a correction loop can be
/// driven without a server. Records the prefill it was handed.
struct Sequence {
    replies: std::cell::RefCell<Vec<String>>,
    prefills: std::cell::RefCell<Vec<String>>,
}

impl Sequence {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: std::cell::RefCell::new(
                replies.iter().rev().map(|s| (*s).to_string()).collect(),
            ),
            prefills: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl Model for Sequence {
    fn complete(
        &self,
        _system: &str,
        _user: &str,
        prefill: &str,
        on_chunk: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String> {
        self.prefills.borrow_mut().push(prefill.to_string());
        // A real server always answers, so once the script runs out the last
        // reply repeats. Otherwise a fixture's length silently becomes part of
        // the test, and adding a call to the implementation breaks it.
        let reply = {
            let mut replies = self.replies.borrow_mut();
            if replies.len() > 1 { replies.pop() } else { replies.last().cloned() }
                .ok_or("no replies configured")?
        };
        let mut sent = String::new();
        for chunk in reply.split_inclusive('\n') {
            sent.push_str(chunk);
            if !on_chunk(chunk) {
                return Ok(sent);
            }
        }
        Ok(reply)
    }
}

/// The point of knowing *which* token killed a candidate is to go back to just
/// before it and try again — not merely to stop paying.
#[test]
fn correction_rewinds_to_the_last_good_prefix_and_resumes() {
    // First reply dies on a type alias; the retry, handed the good prefix back,
    // writes something legal instead.
    let model = Sequence::new(&[
        "```stitch\next f() -> Int = 1\next Time = Int\nmore doomed text\n",
        "ext double(n: Int) -> Int = n * 2\n```",
    ]);
    let run = run_once_corrected(&model, "anything", 2, &mut |_| {}, &mut |_| {}).expect("fake succeeds");

    let prefills = model.prefills.borrow();
    assert_eq!(prefills.len(), 2, "expected one retry");
    assert!(prefills[0].is_empty(), "first call starts fresh");
    assert!(
        prefills[1].contains("ext f() -> Int = 1"),
        "retry must resume from the good prefix, got {:?}",
        prefills[1]
    );
    assert!(
        !prefills[1].contains("ext Time = Int"),
        "the fatal line must be rewound away, got {:?}",
        prefills[1]
    );
    assert_eq!(run.corrections.len(), 1);
    assert_eq!(run.outcome.stage(), "ok");
}

/// Variety is the whole point of a corpus: one recipe repeated 500 times is one
/// program written 500 ways. The axes come from `plans/corpus-recipe-axes.md`.
#[test]
fn a_sheet_cycles_deterministically() {
    let sheet = cram_gen::recipe::sheet("batch9").unwrap();
    let first = sheet.nth(0);

    assert_eq!(sheet.nth(0).domain, first.domain, "must be deterministic");
    assert_ne!(sheet.nth(1).domain, first.domain);
    // Past the end it wraps, so `--count 500` gives five passes over the set.
    assert_eq!(sheet.nth(sheet.count()).domain, first.domain);
}

/// batch9's sheet is the record of what produced `corpora/batch9`, so it is
/// frozen: 100 recipes, in that order, rendered the way they were rendered then.
/// A later sheet is a new file, never an edit to this one.
#[test]
fn the_batch9_sheet_is_frozen_at_the_hundred_that_produced_batch9() {
    let sheet = cram_gen::recipe::sheet("batch9").unwrap();

    assert_eq!(sheet.count(), 100);
    assert_eq!(sheet.nth(0).domain, "warehouse bin allocation");
    assert_eq!(sheet.nth(99).domain, "lost pet register");
}

/// Pass-major, and this is the whole reason crossings are a list rather than
/// repeated rows: a 500-candidate run over a 500-domain sheet sees all 500
/// domains once, instead of the first 250 twice. The axes only buy variety if a
/// short run gets the spread as well as a long one does.
#[test]
fn crossings_flatten_pass_major_so_a_short_run_sees_every_domain() {
    let text = r#"
        [[recipe]]
        domain = "one"
        clause = "c"
        crossings = [
          { constructs = "prod", size = "small", shape = "module" },
          { constructs = "sum", size = "large", shape = "script" },
        ]

        [[recipe]]
        domain = "two"
        clause = "c"
        crossings = [
          { constructs = "prod", size = "small", shape = "module" },
          { constructs = "sum", size = "large", shape = "script" },
        ]
    "#;

    let sheet = cram_gen::recipe::Sheet::parse("fixture", text).unwrap();

    let order: Vec<String> = (0..4).map(|at| sheet.nth(at).domain).collect();
    assert_eq!(order, vec!["one", "two", "one", "two"]);
    assert_eq!(sheet.nth(0).size, "small", "the first pass is the first crossing");
    assert_eq!(sheet.nth(2).size, "large", "the second pass is the second crossing");
}

/// batch9's spelling — one inline crossing per row — still parses, because the
/// sheet that produced a batch has to keep producing it.
#[test]
fn an_inline_crossing_is_one_crossing() {
    let text = r#"
        [[recipe]]
        domain = "one"
        clause = "c"
        constructs = "prod"
        size = "small"
        shape = "module"
    "#;

    let sheet = cram_gen::recipe::Sheet::parse("fixture", text).unwrap();

    assert_eq!(sheet.count(), 1);
    assert_eq!(sheet.nth(0).constructs, "prod");
}

/// A row that names neither is a mistake in the data, and it has to fail loudly:
/// a sheet that silently drops rows generates a batch against fewer axes than
/// anybody chose, and the manifest would not say so.
#[test]
fn a_row_with_no_crossing_at_all_is_an_error() {
    let text = "[[recipe]]\ndomain = \"one\"\nclause = \"c\"\n";

    let error = cram_gen::recipe::Sheet::parse("fixture", text).unwrap_err();

    assert!(error.contains("one"), "must name the row: {error}");
}

/// The point of the sheet: 500 domains, each asked for twice at different
/// crossings. batch9 asked each of its 100 domains ten times at the *identical*
/// crossing, so nine of every ten programs varied only by sampling noise.
#[test]
fn the_default_sheet_asks_every_domain_twice() {
    let sheet = cram_gen::recipe::sheet(cram_gen::recipe::DEFAULT).unwrap();
    let domains = sheet.domains().len();

    assert_eq!(domains, 500);
    assert_eq!(sheet.count(), domains * 2);

    let first_pass: std::collections::HashSet<String> =
        (0..domains).map(|at| sheet.nth(at).domain).collect();
    assert_eq!(first_pass.len(), domains, "the first pass must cover every domain once");
}

/// Finding 1 of `notes/batch9-findings.md`: parse-death is a monotone function
/// of program length, 16% in the shortest decile against 92% in the longest. The
/// size mix is half of the response to that — the wording is the other half.
#[test]
fn the_default_sheet_skews_small() {
    let sheet = cram_gen::recipe::sheet(cram_gen::recipe::DEFAULT).unwrap();

    let count = |size: &str| (0..sheet.count()).filter(|at| sheet.nth(*at).size == size).count();
    let (small, medium, large) = (count("small"), count("medium"), count("large"));

    assert_eq!(small + medium + large, sheet.count(), "every size must be one of the three");
    assert!(small * 2 >= sheet.count(), "at least half should be small, got {small}");
    assert!(small > medium, "small should be the biggest bucket, got {small} vs {medium}");
    assert!(large * 10 < sheet.count(), "large should be a tail, got {large}");
}

/// The one hard crossing rule from `plans/corpus-recipe-axes.md`: construct
/// count scales with size — small 1–2, medium 2–3, large 3–4. Four constructs
/// asked of a small program is a brief that cannot be satisfied at that size,
/// and the model resolves that by writing a bigger program, which Finding 1
/// says is what kills the yield.
#[test]
fn every_recipe_respects_the_size_to_construct_count_rule() {
    let sheet = cram_gen::recipe::sheet(cram_gen::recipe::DEFAULT).unwrap();

    for at in 0..sheet.count() {
        let recipe = sheet.nth(at);
        let count = recipe.constructs.split(',').count();
        let (least, most) = match recipe.size.as_str() {
            "small" => (1, 2),
            "large" => (3, 4),
            _ => (2, 3),
        };
        assert!(
            (least..=most).contains(&count),
            "{} at {} asks for {count} constructs, not {least}–{most}: {}",
            recipe.domain,
            recipe.size,
            recipe.constructs
        );
    }
}

/// batch9 breaks that rule in five rows, all of them a `small` asked for three
/// constructs — `plans/corpus-recipe-axes.md` claimed every crossing respected
/// it, and that claim was never checked. They stay, because the sheet is the
/// record of what produced the corpus and editing it would break the
/// correspondence. Pinned rather than fixed, so the count cannot grow.
#[test]
fn batch9_carries_five_crossings_that_break_the_construct_count_rule() {
    let sheet = cram_gen::recipe::sheet("batch9").unwrap();

    let broken: Vec<String> = (0..sheet.count())
        .map(|at| sheet.nth(at))
        .filter(|recipe| recipe.size == "small" && recipe.constructs.split(',').count() > 2)
        .map(|recipe| recipe.domain)
        .collect();

    assert_eq!(
        broken,
        vec!["taxi meter", "tip pooling", "chess clock", "sauna booking", "dog licence register"]
    );
}

/// batch9's seed was 62% module, which left `script` and `server loop` barely
/// exercised — and shape is the axis that changes a program's skeleton rather
/// than its nouns. No shape may be more than half the sheet.
#[test]
fn the_default_sheet_spreads_its_shapes() {
    let sheet = cram_gen::recipe::sheet(cram_gen::recipe::DEFAULT).unwrap();

    let count = |shape: &str| (0..sheet.count()).filter(|at| sheet.nth(*at).shape == shape).count();
    let shapes = ["module", "script", "server loop", "library-with-heavy-tests"];
    let counts: Vec<usize> = shapes.iter().map(|shape| count(shape)).collect();

    assert_eq!(counts.iter().sum::<usize>(), sheet.count(), "every shape must be a known one");
    for (shape, at) in shapes.iter().zip(&counts) {
        assert!(at * 10 >= sheet.count(), "{shape} is under a tenth of the sheet at {at}");
        assert!(at * 2 <= sheet.count(), "{shape} is over half the sheet at {at}");
    }
}

/// The other half of the response to Finding 1. batch9's briefs ended "if the
/// program naturally wants to be bigger, let it be" — which is exactly the
/// latitude the yield curve says is expensive. The old sheet keeps the old
/// wording, because it is the record of what produced that corpus.
#[test]
fn a_held_size_policy_does_not_invite_a_longer_program() {
    let held = cram_gen::recipe::sheet("batch10").unwrap().nth(0).render();
    let grew = cram_gen::recipe::sheet("batch9").unwrap().nth(0).render();

    assert!(held.contains("A longer program is not a better one"), "{held}");
    assert!(!held.contains("let it be"), "{held}");
    assert!(grew.contains("let it be"), "batch9's wording is frozen: {grew}");
    // Both still count declarations, never lines — a model cannot count lines
    // and wrecks a working program trying to.
    assert!(!held.contains("lines") && !grew.contains("lines"));
}

/// The opening line used to call every program a module and then the next line
/// called it a script. batch9 could carry that because 62% of it really was
/// modules; batch10 spreads the shapes, so the brief now contradicts itself more
/// often than not — and a brief that argues with itself is exactly the
/// reconciliation a small model fails at.
#[test]
fn the_opening_line_names_the_shape_it_then_describes() {
    let sheet = cram_gen::recipe::sheet("batch10").unwrap();

    for at in 0..sheet.count() {
        let recipe = sheet.nth(at);
        let noun = match recipe.shape.as_str() {
            "script" => "script",
            "server loop" => "service",
            "library-with-heavy-tests" => "library",
            _ => "module",
        };
        let brief = recipe.render();
        assert!(
            brief.starts_with(&format!("Write a {} {noun}:", recipe.domain)),
            "a {} should not open as a {noun}: {brief}",
            recipe.shape
        );
    }
}

/// batch9's briefs called everything a module, and that wording is what produced
/// `corpora/batch9`. It does not get retrospectively improved.
#[test]
fn batch9_still_calls_every_program_a_module() {
    let sheet = cram_gen::recipe::sheet("batch9").unwrap();
    let script = (0..sheet.count()).map(|at| sheet.nth(at)).find(|r| r.shape == "script").unwrap();

    assert!(script.render().starts_with(&format!("Write a {} module:", script.domain)));
}

/// An unknown name is an error that says what does exist — a typo'd `--recipes`
/// silently falling back to the default would train a batch on the wrong axes
/// and nothing downstream would report it.
#[test]
fn an_unknown_sheet_names_the_ones_that_exist() {
    let error = cram_gen::recipe::sheet("batch11").unwrap_err();

    assert!(error.contains("batch11"), "must name what was asked for: {error}");
    assert!(error.contains("batch9") && error.contains("batch10"), "must list the sheets: {error}");
}

/// The rendered brief must carry the distinguishing clause — a bare domain name
/// lets the model default to the same records-plus-filter program every time.
#[test]
fn a_rendered_recipe_states_the_computation_not_just_the_domain() {
    let recipe = cram_gen::recipe::sheet("batch9").unwrap().nth(0);
    let brief = recipe.render();
    assert!(brief.contains(&recipe.domain), "domain missing");
    assert!(brief.contains(&recipe.clause), "distinguishing clause missing");
    assert!(brief.contains(&recipe.constructs), "constructs missing");
    // Size is expressed in declarations, never lines — a model cannot count
    // lines and wrecks a working program trying to.
    assert!(brief.contains("types") && brief.contains("functions"), "size missing");
    assert!(!brief.contains("lines"), "size must not be given in lines");
}

/// Control characters have to be visible in a rewind marker, or the terminal is
/// ambiguous about what was actually thrown away.
#[test]
fn escaping_makes_discarded_text_unambiguous_on_one_line() {
    assert_eq!(cram_gen::escape("ext Time = Int\n"), "ext Time = Int\\n");
    assert_eq!(cram_gen::escape("a\tb\r\n"), "a\\tb\\r\\n");
    assert_eq!(cram_gen::escape("plain"), "plain");
}

/// The stream has already printed text that a rewind then discards, so the
/// terminal stops matching the saved program unless the rewind announces itself.
#[test]
fn a_rewind_is_announced_with_what_it_discarded() {
    let model = Sequence::new(&[
        "```stitch\next f() -> Int = 1\next Time = Int\n",
        "ext double(n: Int) -> Int = n * 2\n```",
    ]);
    let mut rewinds: Vec<String> = Vec::new();
    let run = run_once_corrected(&model, "anything", 2, &mut |_| {}, &mut |discarded| {
        rewinds.push(discarded.to_string());
    })
    .expect("fake succeeds");

    assert_eq!(rewinds.len(), 1, "the rewind should have been announced");
    assert!(rewinds[0].contains("ext Time = Int"), "got {:?}", rewinds[0]);
    assert_eq!(run.corrections.len(), 1);
}

/// Handing back the prefix that ends exactly where the model went wrong tends
/// to produce the same mistake again — it is the same state. So each successive
/// rewind reaches further back, giving the model room to take a different path.
#[test]
fn a_repeated_mistake_rewinds_further_each_time() {
    let model = Sequence::new(&[
        "```stitch\next a() -> Int = 1\next b() -> Int = 2\next Time = Int\n",
        "ext Time = Int\n",           // same mistake, one chunk back
        "ext c() -> Int = 3\n```",    // escapes once given more room
    ]);
    let run = run_once_corrected(&model, "anything", 4, &mut |_| {}, &mut |_| {}).expect("fake succeeds");

    let prefills = model.prefills.borrow();
    assert_eq!(prefills.len(), 3);
    assert!(
        prefills[2].len() < prefills[1].len(),
        "the second rewind must reach further back than the first:\n  1: {:?}\n  2: {:?}",
        prefills[1],
        prefills[2]
    );
    assert_eq!(run.outcome.stage(), "ok");
}

/// Corrections are budgeted: a model that keeps producing the same fatal token
/// must not loop forever.
#[test]
fn correction_gives_up_after_its_budget() {
    // Retries are continuation-shaped — a prefilled model resumes the program,
    // it does not re-open the fence — and this one keeps making the same mistake.
    let model = Sequence::new(&[
        "```stitch\next f() -> Int = 1\next Time = Int\n",
        "ext Time = Int\n",
        "ext Time = Int\n",
        "ext Time = Int\n",
    ]);
    let run = run_once_corrected(&model, "anything", 2, &mut |_| {}, &mut |_| {}).expect("fake succeeds");
    assert!(run.abandoned, "should have given up");
    assert!(run.corrections.len() <= 3, "bounded by the budget: {}", run.corrections.len());
}

/// Giving up on *correcting* must not mean giving up on the *program*. A
/// truncated candidate is the worst outcome — broken and incomplete — and it
/// teaches a model that programs end mid-expression. Once the budget is spent,
/// stop guarding and let it finish.
#[test]
fn a_spent_budget_lets_the_program_finish_unguarded() {
    let model = Sequence::new(&[
        "```stitch\next f() -> Int = 1\next Time = Int\n",
        "ext Time = Int\n",
        "ext Time = Int\next g() -> Int = 2\next h() -> Int = 3\n```",
    ]);
    let run = run_once_corrected(&model, "anything", 1, &mut |_| {}, &mut |_| {})
        .expect("fake succeeds");

    assert!(run.abandoned, "correction gave up");
    assert!(
        run.program.contains("ext h() -> Int = 3"),
        "the program must have run to completion: {:?}",
        run.program
    );
}

/// Emitting chunks is not the same as making progress. A model that produces
/// `\n` then `}` then dies has written two chunks and advanced nothing — and if
/// that resets the budget, it loops forever. Observed live: forty rewinds of
/// `" }"` in a row.
#[test]
fn chunks_without_growth_do_not_count_as_progress() {
    let stall = "\n}\n";
    let model = Sequence::new(&[
        "```stitch\next f() -> Int = 1\next Time = Int\n",
        stall, stall, stall, stall, stall, stall, stall, stall,
    ]);
    let run = run_once_corrected(&model, "anything", 3, &mut |_| {}, &mut |_| {})
        .expect("fake succeeds");
    assert!(run.abandoned, "a stalled candidate must be given up on");
    assert!(
        run.corrections.len() <= 5,
        "should not have looped: {} rewinds",
        run.corrections.len()
    );
}

/// The budget bounds *being stuck*, not total rewinds. A long program that
/// recovers cleanly each time is going fine, and abandoning it for a running
/// total would punish length — exactly the candidates worth having.
#[test]
fn progress_between_failures_does_not_count_against_the_budget() {
    // Four separate failures, each followed by real progress. With a budget of
    // 2 consecutive failures this must still finish.
    let model = Sequence::new(&[
        "```stitch\next a() -> Int = 1\next Time = Int\n",
        "ext b() -> Int = 2\next Time = Int\n",
        "ext c() -> Int = 3\next Time = Int\n",
        "ext d() -> Int = 4\next Time = Int\n",
        "ext e() -> Int = 5\n```",
    ]);
    let run = run_once_corrected(&model, "anything", 2, &mut |_| {}, &mut |_| {}).expect("fake succeeds");
    assert!(!run.abandoned, "progress each round should keep it alive");
    assert_eq!(run.corrections.len(), 4, "every rewind is still recorded");
    assert_eq!(run.outcome.stage(), "ok");
}

/// Each rewind is a labelled pair: what the model wanted to write, and what it
/// wrote once that was taken away. Those are exactly the repair traces
/// `docs/kvetch-rl-design.md` §5 wants, harvested as a by-product of generating.
#[test]
fn a_correction_records_what_was_discarded_and_what_replaced_it() {
    let model = Sequence::new(&[
        "```stitch\next f() -> Int = 1\next Time = Int\nmore doomed text\n",
        "ext double(n: Int) -> Int = n * 2\n```",
    ]);
    let run = run_once_corrected(&model, "anything", 2, &mut |_| {}, &mut |_| {}).expect("fake succeeds");

    assert_eq!(run.corrections.len(), 1);
    let correction = &run.corrections[0];
    assert!(
        correction.discarded.contains("ext Time = Int"),
        "should record what the model wanted: {correction:?}"
    );
    assert!(
        correction.replacement.contains("ext double"),
        "should record what replaced it: {correction:?}"
    );
    assert!(
        correction.context.contains("ext f() -> Int = 1"),
        "should record the program it was writing into: {correction:?}"
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
        domain: "sauna booking".into(),
        size: "small".into(),
        shape: "module".into(),
        stage: "parse".into(),
        detail: "unexpected character `&`".into(),
        tokens: 412,
        seconds: 9.5,
        reasoned: true,
        extra_blocks: 1,
        corrections: Vec::new(),
    };
    let json = serde_json::to_string(&record).expect("serialises");
    // The crossing is on the row, not just the domain: a sheet that asks the
    // same domain at two sizes cannot be analysed per domain alone.
    for expected in [
        "\"index\":7",
        "\"stage\":\"parse\"",
        "\"tokens\":412",
        "\"reasoned\":true",
        "\"size\":\"small\"",
        "\"shape\":\"module\"",
    ] {
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

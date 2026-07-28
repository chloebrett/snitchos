//! Corpus generation harness — ask a local model for a Stitch program, and run
//! what comes back through the gate.
//!
//! Scope is deliberately the *impure* half of the pipeline: the model call and
//! the shape of a candidate. The gate is `stitch::gate`; recipe values that are
//! pure data belong in `cram-corpus`. See `plans/corpus-mvp.md` "Structure" —
//! the line goes at the seam, and the seam is the model call.
//!
//! Consumed from `xtask-cram`, never lean `xtask`: an edit here must not
//! recompile the tool that runs `cargo xtask test`.

use std::io::{BufRead, BufReader};

use stitch::gate::{self, Outcome};

pub mod prompt;
pub mod recipe;

/// A model that can answer a system+user pair. The trait exists so every stage
/// below it is testable with no model present.
pub trait Model {
    /// `on_chunk` is called with each fragment as it arrives, so a long
    /// generation narrates itself instead of appearing all at once. The return
    /// value is still the whole response — callers that do not care about
    /// progress pass `&mut |_| {}`.
    ///
    /// Errors are transport/protocol failures, not bad programs — a bad program
    /// is an `Ok` whose gate `Outcome` is unhappy.
    /// `on_chunk` returns `false` to stop generation early — which is how the
    /// guard abandons a candidate the instant it becomes unrecoverable instead
    /// of paying for the rest of it.
    /// `prefill` is text the assistant is treated as having already written, so
    /// generation *continues* it rather than starting over. Empty for a fresh
    /// candidate; the last-good prefix when a correction is rewinding.
    fn complete(
        &self,
        system: &str,
        user: &str,
        prefill: &str,
        on_chunk: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String>;
}

/// Can *any* token rescue this program, or is it already unrecoverable?
///
/// Wraps the continuation oracle, which answers the question from the prefix
/// alone. Crucially it does **not** fire mid-lexeme (`{ ex` on its way to `ext`)
/// or on a merely incomplete expression (`a.start <`) — only on a prefix no
/// continuation can save, like `ext Time =` (Stitch has no type aliases).
#[must_use]
pub fn is_doomed(program: &str) -> bool {
    !program.is_empty() && stitch::oracle::valid_next(program, program.len()).is_empty()
}

/// Pull the incremental text out of one server-sent-events frame.
///
/// Returns `None` for the `[DONE]` sentinel, for blank keep-alive lines, and for
/// the opening frame (which carries a role and no content) — all of which are
/// structure rather than output.
#[must_use]
pub fn sse_delta(line: &str) -> Option<String> {
    let payload = line.strip_prefix("data:")?.trim();
    if payload == "[DONE]" {
        return None;
    }
    let frame: serde_json::Value = serde_json::from_str(payload).ok()?;
    frame
        .get("choices")?
        .get(0)?
        .get("delta")?
        .get("content")?
        .as_str()
        .map(str::to_string)
}

/// What the extractor pulled out of a raw response.
#[derive(Debug, Clone)]
pub struct Extracted {
    pub program: String,
    /// Fenced blocks beyond the first. A prompt failure, not a Stitch failure —
    /// counted separately because the fix is different.
    pub extra_blocks: usize,
}

/// Pull the program out of a model response.
///
/// A fenced block wins; prose around it is dropped. With no fence at all the
/// whole response is the candidate — the fence is how the model was *asked* to
/// reply, not a precondition for the program being real.
#[must_use]
pub fn extract(raw: &str) -> Extracted {
    let body = strip_reasoning(raw);
    let blocks: Vec<String> = fenced_blocks(body);
    match blocks.split_first() {
        Some((first, rest)) => {
            Extracted { program: first.clone(), extra_blocks: rest.len() }
        }
        // Once a fence has been opened the blocks are authoritative, even when
        // empty. Falling back to the raw text here would hand back the fence
        // marker itself — which is not Stitch, and which reads as a doomed
        // program the instant a stream opens its fence.
        None if opens_a_fence(body) => Extracted { program: String::new(), extra_blocks: 0 },
        None => Extracted { program: body.trim().to_string(), extra_blocks: 0 },
    }
}

fn opens_a_fence(body: &str) -> bool {
    body.lines().any(|line| line.trim_start().starts_with("```"))
}

/// Drop a reasoning model's `<think>` block.
///
/// It must go *before* fences are looked for, because the block routinely quotes
/// the prompt — including the instruction to reply in a fenced block — and a
/// quoted fence is not a program.
///
/// An **unterminated** block means the token cap landed mid-thought, so there is
/// no program at all. Returning the thinking text would hand the gate a wall of
/// English and report a parse error, which blames the wrong thing; returning
/// nothing lets the caller name it an extraction failure.
fn strip_reasoning(raw: &str) -> &str {
    let Some(open) = raw.find("<think>") else { return raw };
    match raw[open..].find("</think>") {
        Some(close) => &raw[open + close + "</think>".len()..],
        None => "",
    }
}

fn fenced_blocks(raw: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in raw.lines() {
        let fence = line.trim_start().starts_with("```");
        match (&mut current, fence) {
            // Closing fence: the block is done.
            (Some(body), true) => {
                blocks.push(core::mem::take(body));
                current = None;
            }
            // Opening fence: start collecting, discarding any language tag.
            (None, true) => current = Some(String::new()),
            (Some(body), false) => {
                body.push_str(line);
                body.push('\n');
            }
            (None, false) => {}
        }
    }
    // An unterminated block still holds a program worth gating.
    if let Some(body) = current
        && !body.trim().is_empty()
    {
        blocks.push(body);
    }
    blocks
}

/// One candidate, from raw response to verdict.
#[derive(Debug, Clone)]
pub struct Run {
    pub raw: String,
    pub program: String,
    pub extra_blocks: usize,
    /// The response carried a `<think>` block, i.e. the server did **not**
    /// honour the request to disable reasoning. Recorded rather than inferred:
    /// it is the difference between "the prompt is wrong" and "the server
    /// ignored a setting", and those have different fixes.
    pub reasoned: bool,
    /// Streamed fragments, which is one per token — the basis for tok/s, and so
    /// for whether a 500k-token corpus is an afternoon or a week.
    pub tokens: usize,
    /// Correction ran out of budget. Under [`run_once_corrected`] the program
    /// still ran to completion afterwards — this records that the guard gave
    /// up, not that generation was cut short. [`run_once_guarded`] does cut it
    /// short, and sets this too.
    pub abandoned: bool,
    /// Every rewind, in order.
    pub corrections: Vec<Correction>,
    pub outcome: Outcome,
}

/// One rewind: what the model wanted to write, what it wrote instead, and the
/// program it was writing into.
///
/// This is the by-product worth more than the corpus. Each row is a labelled
/// pair — *this construct is what the model reaches for, this is what the
/// language actually permits* — which is simultaneously a diagnostic (which
/// features does it systematically get wrong?), something to visualise, and the
/// repair-trace training data `docs/kvetch-rl-design.md` §5 wants but expected
/// to have to manufacture.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Correction {
    /// The valid program immediately before the rejected text — the state the
    /// model was in when it made the choice. Trimmed to the tail, because the
    /// preceding two hundred lines are not what explains the slip.
    pub context: String,
    /// The text the oracle refused. **What the model wanted to say.**
    pub discarded: String,
    /// What it produced on resume, with the same prefix and one more draw.
    pub replacement: String,
    /// Chunks thrown away. 1 unless the model repeated itself at the same spot,
    /// in which case the rewind reached further.
    pub depth: usize,
}

/// Render control characters visibly, so a one-line marker is unambiguous
/// about what it is quoting. A rewind has already printed its text to the
/// terminal as real newlines; naming it again as `ext Time = Int\n` makes clear
/// which bytes are being discarded.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// How much preceding program to keep with a correction. Enough to see the
/// construct being written, not so much that the interesting part is buried.
const CORRECTION_CONTEXT: usize = 240;

/// One row of a batch manifest.
///
/// The terminal scrolls; the funnel has to survive it. Kept beside the saved
/// `.st`/`.raw.md` pair so a batch can be re-read months later without
/// re-deriving what happened to each candidate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CandidateRecord {
    pub index: usize,
    /// Which recipe produced it — without this a batch is a bag of programs
    /// with no way to ask which axes actually yield.
    pub domain: String,
    /// The crossing that domain was asked for. batch9 could only be analysed
    /// per domain because every asking of a domain used the same crossing;
    /// a sheet that varies them has to record which one, or the same question
    /// cannot be asked of this batch.
    pub size: String,
    pub shape: String,
    /// `empty` | `parse` | `type` | `tests` | `ok`.
    pub stage: String,
    /// The gate's own message — the raw material for a repair trace.
    pub detail: String,
    pub tokens: usize,
    pub seconds: f64,
    pub reasoned: bool,
    pub extra_blocks: usize,
    /// Every rewind, with what the model wanted beside what replaced it.
    pub corrections: Vec<Correction>,
}

/// Ask `model` for a program and take it all the way to a gate verdict.
///
/// # Errors
/// Only if the model call itself fails. A rejected program is an `Ok`.
pub fn run_once<M: Model + ?Sized>(
    model: &M,
    task: &str,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<Run, String> {
    let mut tokens = 0usize;
    let raw = model.complete(&prompt::system(), &prompt::user(task), "", &mut |chunk| {
        tokens += 1;
        on_chunk(chunk);
        true
    })?;
    Ok(assemble(raw, tokens, false))
}

/// [`run_once`], but stop the moment the program becomes unrecoverable.
///
/// Every failure in `corpora/batch1` was a single fatal token followed by
/// hundreds of tokens of doomed continuation — candidate 004 spent 1162 tokens
/// after `cond` had already killed it. The oracle knows at the token, so there
/// is no reason to pay for the rest.
///
/// This is not yet a decoder mask — it cannot stop the model *choosing* the
/// fatal token, only stop paying once it has, because an OpenAI-compatible HTTP
/// API hands back sampled tokens rather than the distribution they came from.
///
/// It is, however, what makes a cheap mask possible. Masking every position
/// would cost one round-trip per token; the oracle says >99% of positions need
/// no intervention, and this pinpoints the handful that do. Upgrading it means:
/// on abandonment, re-request that one position with `top_logprobs`, drop the
/// candidates [`is_doomed`] rejects, splice the best survivor, and continue —
/// a few round-trips per program instead of a few thousand.
///
/// That upgrade also yields the interesting artifact: at each intervention you
/// have *what the model wanted to say* beside *what the language allowed*.
///
/// # Errors
/// Only if the model call itself fails.
pub fn run_once_guarded<M: Model + ?Sized>(
    model: &M,
    task: &str,
    on_chunk: &mut dyn FnMut(&str),
) -> Result<Run, String> {
    let mut tokens = 0usize;
    let mut seen = String::new();
    let mut abandoned = false;
    let raw = model.complete(&prompt::system(), &prompt::user(task), "", &mut |chunk| {
        tokens += 1;
        on_chunk(chunk);
        seen.push_str(chunk);
        // Judge the *extracted* program, not the raw response: prose and fences
        // around it are not Stitch and would read as doomed immediately.
        abandoned = is_doomed(extract(&seen).program.trim_end());
        !abandoned
    })?;
    Ok(assemble(raw, tokens, abandoned))
}

/// [`run_once_guarded`], but on abandonment **rewind to the last good prefix and
/// resume** rather than throwing the candidate away.
///
/// This is the payoff for knowing *which* token was fatal. Everything before it
/// is fine — often hundreds of lines of correct Stitch — so the only thing worth
/// discarding is the token itself. Generation resumes from the surviving prefix,
/// and the model gets another draw at that one position.
///
/// It is not a decoder mask: the fatal token is not removed from the
/// distribution, so the model may pick it again. `budget` bounds how many times
/// that is tolerated. The mask proper needs `top_logprobs` at the rewind point —
/// which this makes cheap, because there are only ever a handful of them.
///
/// # Errors
/// Only if a model call fails.
pub fn run_once_corrected<M: Model + ?Sized>(
    model: &M,
    task: &str,
    budget: usize,
    on_chunk: &mut dyn FnMut(&str),
    on_rewind: &mut dyn FnMut(&str),
) -> Result<Run, String> {
    let (system, user) = (prompt::system(), prompt::user(task));
    // Kept as chunks, not one string, so a rewind can be measured in tokens.
    let mut kept: Vec<String> = Vec::new();
    let mut tokens = 0usize;
    let mut corrections: Vec<Correction> = Vec::new();
    // The rewind waiting to learn what replaced it.
    let mut pending: Option<(String, String, usize)> = None;
    // How far back to reach. Handing back a prefix that ends exactly where the
    // model went wrong tends to reproduce the mistake — it is the same state —
    // so each successive attempt reaches further.
    let mut depth = 1usize;
    // Consecutive failures with no progress. The budget bounds *being stuck*,
    // not total rewinds: a long program that recovers cleanly ten times is
    // going fine, and abandoning it for a total count would punish length.
    let mut stuck = 0usize;
    // The longest valid program reached so far. Progress means *beating this* —
    // emitting chunks is not the same thing. A model that writes `\n` then `}`
    // and dies has produced two chunks and advanced nothing, and treating that
    // as progress loops forever (observed live: forty rewinds of `" }"`).
    let mut high_water = 0usize;

    loop {
        let prefix: String = kept.concat();
        let mut round: Vec<String> = Vec::new();
        let mut doomed = false;
        let raw = model.complete(&system, &user, &prefix, &mut |chunk| {
            tokens += 1;
            on_chunk(chunk);
            if let Some((context, discarded, depth)) = pending.take() {
                corrections.push(Correction {
                    context,
                    discarded,
                    replacement: chunk.to_string(),
                    depth,
                });
            }
            round.push(chunk.to_string());
            doomed = is_doomed(
                extract(&format!("{prefix}{}", round.concat())).program.trim_end(),
            );
            !doomed
        })?;

        if !doomed {
            let mut run = assemble(format!("{prefix}{raw}"), tokens, false);
            run.corrections = corrections;
            return Ok(run);
        }

        if stuck >= budget {
            // Giving up on *correcting* is not giving up on the *program*. A
            // truncated candidate is the worst outcome — broken and incomplete —
            // and it teaches a model that programs end mid-expression. Drop the
            // guard and let it run to the end: what comes back is a finished
            // program with a few bad tokens in it, which is overwhelmingly
            // correct Stitch by token and perfectly good training data.
            let finished = model.complete(&system, &user, &prefix, &mut |chunk| {
                tokens += 1;
                on_chunk(chunk);
                true
            })?;
            // A `Correction` is a *completed* rewind — discarded text plus what
            // replaced it. This last refusal never got one; `abandoned` records
            // that correction ran out, not that the program did.
            let mut run = assemble(format!("{prefix}{finished}"), tokens, true);
            run.corrections = corrections;
            return Ok(run);
        }

        // Everything this round except the fatal chunk is still good.
        kept.extend(round.drain(..));
        // Did the program actually get longer than it has ever been? Only that
        // is progress. A new high-water mark means a *new* failure further on,
        // so the deeper rewind is not warranted and both counters reset.
        let reached = kept.iter().map(String::len).sum::<usize>();
        if reached > high_water {
            high_water = reached;
            depth = 1;
            stuck = 0;
        } else {
            stuck += 1;
        }
        // Drop the fatal chunk, then `depth - 1` more.
        let mut discarded = String::new();
        let requested = depth;
        for _ in 0..depth {
            match kept.pop() {
                Some(chunk) => discarded.insert_str(0, &chunk),
                None => break,
            }
        }
        depth = depth.saturating_mul(2);
        // The discarded text is already on the terminal from the stream, so the
        // display no longer matches what will be saved. Say so.
        on_rewind(&discarded);
        pending = Some((tail(&kept.concat()), discarded, requested));
    }
}

/// The last `CORRECTION_CONTEXT` bytes, on a character boundary.
fn tail(text: &str) -> String {
    let start = text.len().saturating_sub(CORRECTION_CONTEXT);
    let start = (start..=text.len()).find(|at| text.is_char_boundary(*at)).unwrap_or(text.len());
    text[start..].to_string()
}

fn assemble(raw: String, tokens: usize, abandoned: bool) -> Run {
    let Extracted { program, extra_blocks } = extract(&raw);
    let reasoned = raw.contains("<think>");
    let outcome = gate::run(&program);
    Run { raw, program, extra_blocks, reasoned, tokens, abandoned, corrections: Vec::new(), outcome }
}

/// Per-stage counts for a batch.
///
/// **The funnel is the product, never one number.** The stage a candidate dies
/// at is the diagnosis: parse deaths mean the generator does not know the
/// grammar (an exemplar problem), type deaths mean it has the shape and not the
/// semantics, test deaths mean it had the semantics and got them wrong. A single
/// yield percentage collapses four different actions into one shrug.
#[derive(Debug, Default, Clone)]
pub struct Tally {
    /// The response contained no program at all — almost always a reasoning
    /// block that ran into the token cap. A prompt/config problem, not a Stitch
    /// one, so it is counted before the gate rather than as a parse death.
    pub empty: usize,
    pub parse: usize,
    pub type_errors: usize,
    pub tests: usize,
    pub ok: usize,
    pub errors: usize,
    pub extra_blocks: usize,
}

impl Tally {
    pub fn record_empty(&mut self) {
        self.empty += 1;
    }

    pub fn record(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Parse(_) => self.parse += 1,
            Outcome::Type(_) => self.type_errors += 1,
            Outcome::Tests { .. } => self.tests += 1,
            Outcome::Ok { .. } => self.ok += 1,
        }
    }

    /// A transport failure, which is not a verdict about a program.
    pub fn record_error(&mut self) {
        self.errors += 1;
    }

    #[must_use]
    pub fn funnel(&self, attempted: usize) -> String {
        format!(
            "{attempted} attempted → model errors {} → no program {} → parse {} → type {} → tests {} → ok {}",
            self.errors, self.empty, self.parse, self.type_errors, self.tests, self.ok
        )
    }
}

/// Render a verdict for a progress line.
#[must_use]
pub fn describe(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Parse(error) => format!("parse — {error}"),
        Outcome::Type(errors) => format!("type — {}", errors.join("; ")),
        Outcome::Tests { failed, passed } => {
            format!("tests — {passed} passed, failed: {}", failed.join(", "))
        }
        Outcome::Ok { tests } => format!("ok — {tests} tests passed"),
    }
}

/// Sampling settings, pinned so a later bulk run stays comparable.
#[derive(Debug, Clone)]
pub struct Sampling {
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u32,
    /// Qwen recommends 1.5 alongside non-thinking mode for *general* tasks.
    /// Left at 0 here on purpose: code legitimately repeats `let`, `ext`, `@`
    /// and closing parens, and penalising that degrades programs in a way that
    /// is hard to attribute later. Raise it only with a measurement.
    pub presence_penalty: f64,
    pub max_tokens: u32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.8,
            top_k: 20,
            presence_penalty: 0.0,
            // Generous, because a hybrid-reasoning model that ignores the
            // no-thinking request spends most of its budget before the program
            // starts — and a truncated response is not a verdict about Stitch.
            max_tokens: 4096,
        }
    }
}

/// An OpenAI-compatible endpoint — LM Studio's local server, by default.
pub struct LmStudio {
    pub base_url: String,
    pub model: String,
    pub sampling: Sampling,
}

impl LmStudio {
    #[must_use]
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: "http://localhost:1234/v1".to_string(),
            model: model.into(),
            sampling: Sampling::default(),
        }
    }
}

impl Model for LmStudio {
    fn complete(
        &self,
        system: &str,
        user: &str,
        prefill: &str,
        on_chunk: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String> {
        let mut messages = vec![
            serde_json::json!({ "role": "system", "content": system }),
            serde_json::json!({ "role": "user", "content": user }),
        ];
        // A trailing assistant message is a prefill: the server continues it
        // rather than answering it. This is how a rewind resumes mid-program.
        if !prefill.is_empty() {
            messages.push(serde_json::json!({ "role": "assistant", "content": prefill }));
        }
        let body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.sampling.temperature,
            "top_p": self.sampling.top_p,
            "top_k": self.sampling.top_k,
            "presence_penalty": self.sampling.presence_penalty,
            "max_tokens": self.sampling.max_tokens,
            "stream": true,
            // Hybrid-reasoning models default to thinking, which here is pure
            // cost — the tokens are discarded and they routinely consume the
            // whole budget before a program appears. Not part of the OpenAI
            // schema, so a server that does not understand it ignores it; the
            // ones that do pass it to the chat template.
            "chat_template_kwargs": { "enable_thinking": false },
        });

        let response = ureq::post(&format!("{}/chat/completions", self.base_url))
            .send_json(body)
            .map_err(|error| format!("request failed: {error}"))?;

        // Server-sent events: one `data:` frame per fragment, terminated by a
        // `[DONE]` sentinel. Read it line by line so the caller sees output as
        // the model produces it rather than after it finishes.
        let mut reader = BufReader::new(response.into_reader());
        let mut whole = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(delta) = sse_delta(line.trim_end()) {
                        whole.push_str(&delta);
                        // Dropping the reader closes the connection, which is
                        // what stops the server generating the rest.
                        if !on_chunk(&delta) {
                            break;
                        }
                    }
                }
                Err(error) => return Err(format!("stream broke: {error}")),
            }
        }
        if whole.is_empty() {
            return Err(String::from("stream produced no content"));
        }
        Ok(whole)
    }
}

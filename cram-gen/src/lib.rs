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
    fn complete(
        &self,
        system: &str,
        user: &str,
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
    /// Generation was stopped early because the program became unrecoverable.
    /// Only ever set by [`run_once_guarded`].
    pub abandoned: bool,
    pub outcome: Outcome,
}

/// One row of a batch manifest.
///
/// The terminal scrolls; the funnel has to survive it. Kept beside the saved
/// `.st`/`.raw.md` pair so a batch can be re-read months later without
/// re-deriving what happened to each candidate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CandidateRecord {
    pub index: usize,
    /// `empty` | `parse` | `type` | `tests` | `ok`.
    pub stage: String,
    /// The gate's own message — the raw material for a repair trace.
    pub detail: String,
    pub tokens: usize,
    pub seconds: f64,
    pub reasoned: bool,
    pub extra_blocks: usize,
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
    let raw = model.complete(&prompt::system(), &prompt::user(task), &mut |chunk| {
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
    let raw = model.complete(&prompt::system(), &prompt::user(task), &mut |chunk| {
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

fn assemble(raw: String, tokens: usize, abandoned: bool) -> Run {
    let Extracted { program, extra_blocks } = extract(&raw);
    let reasoned = raw.contains("<think>");
    let outcome = gate::run(&program);
    Run { raw, program, extra_blocks, reasoned, tokens, abandoned, outcome }
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
        on_chunk: &mut dyn FnMut(&str) -> bool,
    ) -> Result<String, String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
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

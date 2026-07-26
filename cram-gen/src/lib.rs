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

use serde::Deserialize;
use stitch::gate::{self, Outcome};

pub mod prompt;

/// A model that can answer a system+user pair. The trait exists so every stage
/// below it is testable with no model present.
pub trait Model {
    /// Errors are transport/protocol failures, not bad programs — a bad program
    /// is an `Ok` whose gate `Outcome` is unhappy.
    fn complete(&self, system: &str, user: &str) -> Result<String, String>;
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
    let blocks: Vec<String> = fenced_blocks(raw);
    match blocks.split_first() {
        Some((first, rest)) => {
            Extracted { program: first.clone(), extra_blocks: rest.len() }
        }
        None => Extracted { program: raw.trim().to_string(), extra_blocks: 0 },
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
    pub outcome: Outcome,
}

/// Ask `model` for a program and take it all the way to a gate verdict.
///
/// # Errors
/// Only if the model call itself fails. A rejected program is an `Ok`.
pub fn run_once<M: Model + ?Sized>(model: &M, task: &str) -> Result<Run, String> {
    let raw = model.complete(&prompt::system(), &prompt::user(task))?;
    let Extracted { program, extra_blocks } = extract(&raw);
    let outcome = gate::run(&program);
    Ok(Run { raw, program, extra_blocks, outcome })
}

/// Sampling settings, pinned so a later bulk run stays comparable.
#[derive(Debug, Clone)]
pub struct Sampling {
    pub temperature: f64,
    pub top_p: f64,
    pub max_tokens: u32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self { temperature: 0.7, top_p: 0.8, max_tokens: 1200 }
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

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    content: String,
}

impl Model for LmStudio {
    fn complete(&self, system: &str, user: &str) -> Result<String, String> {
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "temperature": self.sampling.temperature,
            "top_p": self.sampling.top_p,
            "max_tokens": self.sampling.max_tokens,
            "stream": false,
        });

        let response = ureq::post(&format!("{}/chat/completions", self.base_url))
            .send_json(body)
            .map_err(|error| format!("request failed: {error}"))?;
        let parsed: ChatResponse = response
            .into_json()
            .map_err(|error| format!("malformed response: {error}"))?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|choice| choice.message.content)
            .ok_or_else(|| "response had no choices".to_string())
    }
}

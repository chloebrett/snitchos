//! Sonnet-assisted staging core. See `plans/snip-stage-picker.md`.
//!
//! Pure, testable: [`build_prompt`] renders the prompt sent to `claude -p`, and
//! [`parse_reply`] parses + validates the model's JSON answer against the real
//! candidate set. The one impure entry point, [`pick`], shells out to `claude`.
//! All git I/O lives in the `xtask` caller, not here.

use std::io::Write;
use std::process::{Command, Stdio};

use serde::Deserialize;

/// Working-tree status of a candidate file, as reported by `git status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    TypeChange,
}

/// Model-reported confidence. Coarse buckets on purpose — the model can't
/// calibrate a fake-precise 0.0–1.0, so three honest levels carry more signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    /// Parse a wire string; anything unrecognised (or missing) is treated as
    /// [`Confidence::Low`] rather than an error — an uncalibrated answer is a
    /// low-confidence answer.
    fn from_wire(s: Option<&str>) -> Self {
        match s.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            Some("high") => Confidence::High,
            Some("medium" | "med") => Confidence::Medium,
            _ => Confidence::Low,
        }
    }
}

/// A parsed `git status --porcelain=v1 -z` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub status: Status,
}

fn status_from_xy(xy: &str) -> Status {
    if xy == "??" {
        return Status::Untracked;
    }
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or(' ');
    let y = chars.next().unwrap_or(' ');
    let has = |c: char| x == c || y == c;
    if x == 'R' {
        Status::Renamed
    } else if has('D') {
        Status::Deleted
    } else if has('A') {
        Status::Added
    } else if has('T') {
        Status::TypeChange
    } else {
        Status::Modified
    }
}

/// Parse the NUL-separated records of `git status --porcelain=v1 -z`.
///
/// Each record is `XY <path>`; a rename (`R`) is followed by a second field
/// holding the source path, which we consume and drop (we key on the new path).
pub fn parse_status(porcelain_z: &str) -> Vec<StatusEntry> {
    let mut fields = porcelain_z.split('\0').filter(|f| !f.is_empty());
    let mut out = Vec::new();
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let (xy, rest) = record.split_at(2);
        let path = rest.trim_start().to_string();
        let status = status_from_xy(xy);
        if status == Status::Renamed {
            let _ = fields.next(); // consume the source-path field
        }
        out.push(StatusEntry { path, status });
    }
    out
}

/// Cap a diff/blob to at most `max_lines` lines, appending a truncation marker
/// noting how many were dropped. Short inputs are returned unchanged.
pub fn cap_diff(diff: &str, max_lines: usize) -> String {
    let total = diff.lines().count();
    if total <= max_lines {
        return diff.to_string();
    }
    let mut out: String = diff.lines().take(max_lines).collect::<Vec<_>>().join("\n");
    out.push_str("\n… ");
    out.push_str(&(total - max_lines).to_string());
    out.push_str(" lines truncated …\n");
    out
}

/// A content fingerprint of the candidate set, used to detect that the working
/// tree drifted between `snip` proposing and `stage`/`commit` finalising.
///
/// Order-independent (XORs per-candidate hashes) so a reordered `git status`
/// doesn't read as drift; sensitive to path, status, and diff content.
pub fn fingerprint(candidates: &[Candidate]) -> String {
    use std::hash::{Hash, Hasher};

    let mut acc: u64 = 0;
    for c in candidates {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        c.path.hash(&mut h);
        c.status.label().hash(&mut h);
        c.diff.hash(&mut h);
        acc ^= h.finish();
    }
    format!("{acc:016x}")
}

/// One changed file offered to the model for triage.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: String,
    pub status: Status,
    /// Rendered (capped) diff or file contents; the caller decides the cap.
    pub diff: String,
}

/// A file the model chose to include, with its rationale and confidence.
#[derive(Debug, Clone)]
pub struct Included {
    pub path: String,
    pub reason: String,
    pub confidence: Confidence,
}

/// A file the model chose to exclude, with its rationale and confidence.
#[derive(Debug, Clone)]
pub struct Excluded {
    pub path: String,
    pub reason: String,
    pub confidence: Confidence,
}

/// The validated result of a triage: which candidates to stage, which to leave.
#[derive(Debug, Clone)]
pub struct Selection {
    pub include: Vec<Included>,
    pub exclude: Vec<Excluded>,
    /// Confidence in the whole partition ("did I understand this tree?").
    pub overall: Confidence,
    /// Optional caveat the model attaches when it hesitates.
    pub note: Option<String>,
}

/// How to invoke the `claude` CLI. The `program` seam lets tests inject a fake.
#[derive(Debug, Clone)]
pub struct ClaudeCfg {
    /// The executable to run. Defaults to `"claude"` (resolved on `PATH`).
    pub program: String,
    /// Model passed via `--model`. Defaults to `"sonnet"`.
    pub model: String,
}

impl Default for ClaudeCfg {
    fn default() -> Self {
        ClaudeCfg { program: "claude".to_string(), model: "sonnet".to_string() }
    }
}

/// Why a raw reply couldn't be turned into a [`Selection`].
#[derive(Debug)]
pub enum ParseError {
    /// The reply body was not the expected JSON object.
    Json(String),
}

/// Why a `pick` call failed end-to-end.
#[derive(Debug)]
pub enum PickError {
    /// Spawning or running the `claude` process failed.
    Spawn(String),
    /// The process exited non-zero.
    Exit { code: Option<i32>, stderr: String },
    /// The reply (after one retry) could not be parsed/validated.
    Parse(ParseError),
}

impl std::fmt::Display for PickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PickError::Spawn(m) => write!(f, "could not run claude: {m}"),
            PickError::Exit { code, stderr } => {
                write!(f, "claude exited with {code:?}: {}", stderr.trim())
            }
            PickError::Parse(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PickError {}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Json(msg) => write!(f, "could not parse model reply as JSON: {msg}"),
        }
    }
}

impl std::error::Error for ParseError {}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Added => "added",
            Status::Modified => "modified",
            Status::Deleted => "deleted",
            Status::Renamed => "renamed",
            Status::Untracked => "untracked",
            Status::TypeChange => "type-change",
        }
    }
}

/// Render the prompt sent to `claude -p`. Pure — the caller supplies already
/// git-gathered, diff-capped candidates.
///
/// The prompt frames the exact problem: a working tree holding several
/// unrelated changes made concurrently by parallel agents, from which the model
/// must select only the files belonging to one commit message, and report its
/// confidence honestly.
pub fn build_prompt(message: &str, candidates: &[Candidate]) -> String {
    let mut out = String::new();

    out.push_str(
        "You are triaging a git working tree that contains SEVERAL UNRELATED \
changes made concurrently by parallel agents. Given ONE commit message, select \
exactly the files that belong to that commit and no others.\n\n\
Rules:\n\
- A file that plausibly belongs to a DIFFERENT concurrent change must be excluded.\n\
- A shared file touched by multiple concerns that you cannot cleanly attribute \
(e.g. a lockfile or a mod.rs) should be excluded, with the reason stated. Whole \
files only — you cannot split a file.\n\
- Be honest about confidence. \"low\" when a file is plausibly part of a different \
concurrent change is the useful signal, not a failure.\n\n",
    );

    out.push_str("COMMIT MESSAGE:\n");
    out.push_str(message);
    out.push_str("\n\n");

    out.push_str("CANDIDATE FILES (");
    out.push_str(&candidates.len().to_string());
    out.push_str("):\n\n");
    for c in candidates {
        out.push_str("### ");
        out.push_str(&c.path);
        out.push_str("  [");
        out.push_str(c.status.label());
        out.push_str("]\n");
        if c.diff.trim().is_empty() {
            out.push_str("(no diff shown)\n");
        } else {
            out.push_str("```\n");
            out.push_str(&c.diff);
            if !c.diff.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n");
        }
        out.push('\n');
    }

    out.push_str(
        "Respond with ONLY a JSON object of this EXACT shape — no markdown, no prose:\n\
{\n\
  \"include\": [{\"path\": \"...\", \"reason\": \"one line\", \"confidence\": \"high|medium|low\"}],\n\
  \"exclude\": [{\"path\": \"...\", \"reason\": \"one line\", \"confidence\": \"high|medium|low\"}],\n\
  \"overall\": \"high|medium|low\",\n\
  \"note\": \"optional caveat when overall is not high\"\n\
}\n\
Every path MUST be one of the candidate files above; do not invent paths.\n",
    );

    out
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
}

/// Extract the model's answer text from the JSON envelope emitted by
/// `claude -p --output-format json`. An error envelope (`is_error`) is surfaced
/// as an `Err` carrying the subtype/result so the caller can report it.
pub fn extract_result_text(envelope_json: &str) -> Result<String, ParseError> {
    let env: Envelope = serde_json::from_str(envelope_json)
        .map_err(|e| ParseError::Json(format!("envelope: {e}")))?;
    let body = env.result.unwrap_or_default();
    if env.is_error {
        let kind = env.subtype.unwrap_or_else(|| "error".to_string());
        return Err(ParseError::Json(format!("claude reported {kind}: {body}")));
    }
    Ok(body)
}

/// Slice out the first top-level JSON object, spanning the outermost matched
/// `{`…`}`. Lets a reply survive markdown fences or a leading "Here you go:".
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end > start).then(|| &raw[start..=end])
}

#[derive(Deserialize)]
struct WireEntry {
    path: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    confidence: Option<String>,
}

#[derive(Deserialize)]
struct WireReply {
    #[serde(default)]
    include: Vec<WireEntry>,
    #[serde(default)]
    exclude: Vec<WireEntry>,
    #[serde(default)]
    overall: Option<String>,
    #[serde(default)]
    note: Option<String>,
}

/// Parse the model's JSON reply and validate it against `candidates`.
///
/// Any `include`/`exclude` path the model returned that is not a real candidate
/// is dropped (the model may not invent files). The `note` is normalised so an
/// empty string reads as `None`.
pub fn parse_reply(raw_json: &str, candidates: &[Candidate]) -> Result<Selection, ParseError> {
    let json = extract_json_object(raw_json)
        .ok_or_else(|| ParseError::Json("no JSON object found in reply".to_string()))?;
    let wire: WireReply =
        serde_json::from_str(json).map_err(|e| ParseError::Json(e.to_string()))?;

    let known = |path: &str| candidates.iter().any(|c| c.path == path);

    let include: Vec<Included> = wire
        .include
        .into_iter()
        .filter(|e| known(&e.path))
        .map(|e| Included {
            confidence: Confidence::from_wire(e.confidence.as_deref()),
            path: e.path,
            reason: e.reason,
        })
        .collect();

    let mut exclude: Vec<Excluded> = wire
        .exclude
        .into_iter()
        .filter(|e| known(&e.path))
        .map(|e| Excluded {
            confidence: Confidence::from_wire(e.confidence.as_deref()),
            path: e.path,
            reason: e.reason,
        })
        .collect();

    let omitted: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            !include.iter().any(|i: &Included| i.path == c.path)
                && !exclude.iter().any(|e| e.path == c.path)
        })
        .collect();
    for c in omitted {
        exclude.push(Excluded {
            path: c.path.clone(),
            reason: "not mentioned by the model; excluded by omission".to_string(),
            confidence: Confidence::Low,
        });
    }

    let note = wire.note.filter(|n| !n.trim().is_empty());

    Ok(Selection {
        include,
        exclude,
        overall: Confidence::from_wire(wire.overall.as_deref()),
        note,
    })
}

/// Run one `claude -p` invocation with `prompt` on stdin, returning the model's
/// answer text (envelope already unwrapped).
fn run_claude(prompt: &str, cfg: &ClaudeCfg) -> Result<String, PickError> {
    let mut child = Command::new(&cfg.program)
        .args([
            "-p",
            "--model",
            &cfg.model,
            "--output-format",
            "json",
            "--allowedTools",
            "",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| PickError::Spawn(e.to_string()))?;

    child
        .stdin
        .take()
        .ok_or_else(|| PickError::Spawn("no stdin handle".to_string()))?
        .write_all(prompt.as_bytes())
        .map_err(|e| PickError::Spawn(e.to_string()))?;

    let out = child.wait_with_output().map_err(|e| PickError::Spawn(e.to_string()))?;
    if !out.status.success() {
        return Err(PickError::Exit {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    let envelope = String::from_utf8_lossy(&out.stdout);
    extract_result_text(&envelope).map_err(PickError::Parse)
}

/// Ask Sonnet (via `claude -p`) which candidates to stage for `message`.
///
/// Impure: shells out to `cfg.program`. On a first-pass parse failure it retries
/// once with a terser "return ONLY the JSON" nudge before giving up.
pub fn pick(
    message: &str,
    candidates: &[Candidate],
    cfg: &ClaudeCfg,
) -> Result<Selection, PickError> {
    let prompt = build_prompt(message, candidates);
    let text = run_claude(&prompt, cfg)?;
    if let Ok(sel) = parse_reply(&text, candidates) {
        return Ok(sel);
    }

    let retry = format!(
        "{prompt}\n\nIMPORTANT: your previous answer could not be parsed. \
Reply with ONLY the raw JSON object, nothing else."
    );
    let text = run_claude(&retry, cfg)?;
    parse_reply(&text, candidates).map_err(PickError::Parse)
}

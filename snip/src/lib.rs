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

/// One hunk of a unified diff, labelled `H1`, `H2`, … by position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub id: String,
    /// The full hunk text, `@@` line through its last body line, byte-exact.
    pub text: String,
}

impl Hunk {
    /// The `@@ … @@` line, for compact display.
    pub fn header_line(&self) -> &str {
        self.text.lines().next().unwrap_or("")
    }
}

/// A single file's unified diff, split into its header and positional hunks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Everything before the first hunk (`diff --git` … `+++`), byte-exact.
    pub header: String,
    pub hunks: Vec<Hunk>,
}

/// Split a single-file unified diff into its header and hunks. Byte-preserving:
/// `header` followed by every `hunk.text` reproduces the input exactly.
pub fn parse_hunks(diff: &str) -> FileDiff {
    let mut starts = Vec::new();
    let mut offset = 0;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@ ") {
            starts.push(offset);
        }
        offset += line.len();
    }

    let header_end = starts.first().copied().unwrap_or(diff.len());
    let header = diff[..header_end].to_string();

    let hunks = starts
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = starts.get(i + 1).copied().unwrap_or(diff.len());
            Hunk { id: format!("H{}", i + 1), text: diff[start..end].to_string() }
        })
        .collect();

    FileDiff { header, hunks }
}

/// Reconstruct a valid patch containing only the hunks whose ids appear in
/// `ids`, in their original order. Returns `None` if none of the ids match a
/// hunk (an empty patch is never staged).
pub fn build_patch(file: &FileDiff, ids: &[String]) -> Option<String> {
    let kept: String = file
        .hunks
        .iter()
        .filter(|h| ids.iter().any(|id| id == &h.id))
        .map(|h| h.text.as_str())
        .collect();
    if kept.is_empty() {
        return None;
    }
    Some(format!("{}{}", file.header, kept))
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
///
/// `hunks` is `None` for a whole-file stage, or `Some(ids)` when only those
/// (validated) hunks of the file should be staged — the partial-application case
/// where a file mixes related and unrelated changes.
#[derive(Debug, Clone)]
pub struct Included {
    pub path: String,
    pub reason: String,
    pub confidence: Confidence,
    pub hunks: Option<Vec<String>>,
}

/// A file the model chose to exclude, with its rationale and confidence.
#[derive(Debug, Clone)]
pub struct Excluded {
    pub path: String,
    pub reason: String,
    pub confidence: Confidence,
    /// True when the model never mentioned this candidate at all (excluded by
    /// omission), rather than actively deciding to leave it out. Omission
    /// excludes are Low-by-construction and don't count against confidence.
    pub omitted: bool,
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

impl Selection {
    /// True when the model is unambiguously confident: `overall` is High, every
    /// included file is High, and every *actively-decided* exclude is High.
    /// Excludes-by-omission (files the model never mentioned) don't count — they
    /// are Low by construction. Used to decide whether `propose` may auto-stage.
    pub fn is_confident(&self) -> bool {
        let high = |c: Confidence| matches!(c, Confidence::High);
        high(self.overall)
            && self.include.iter().all(|i| high(i.confidence))
            && self.exclude.iter().all(|e| e.omitted || high(e.confidence))
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
    // Bound the payload: every file is always named (so the model knows it
    // exists), but diff bodies draw from a shared global line budget and a
    // per-file cap, so no single file — nor the whole set — can blow up the
    // prompt. Over-budget bodies are elided with a truncation marker.
    let mut budget = GLOBAL_LINE_BUDGET;
    for c in candidates {
        out.push_str("### ");
        out.push_str(&c.path);
        out.push_str("  [");
        out.push_str(c.status.label());
        out.push_str("]\n");
        let body = cap_diff(&candidate_body(c), PER_FILE_LINE_CAP.min(budget));
        budget = budget.saturating_sub(body.lines().count());
        out.push_str(&body);
        out.push_str("\n\n");
    }

    out.push_str(
        "Respond with ONLY a JSON object of this EXACT shape — no markdown, no prose:\n\
{\n\
  \"include\": [{\"path\": \"...\", \"reason\": \"one line\", \"confidence\": \"high|medium|low\"}],\n\
  \"exclude\": [{\"path\": \"...\", \"reason\": \"one line\", \"confidence\": \"high|medium|low\"}],\n\
  \"overall\": \"high|medium|low\",\n\
  \"note\": \"optional caveat when overall is not high\"\n\
}\n\
Every path MUST be one of the candidate files above; do not invent paths.\n\n\
PARTIAL FILES: if a file contains BOTH changes that belong to this commit AND \
unrelated changes, include it with a \"hunks\" array naming only the relevant \
hunk ids shown in [brackets], e.g. {\"path\":\"x.rs\",\"reason\":\"only the parser fix\",\
\"confidence\":\"high\",\"hunks\":[\"H1\",\"H3\"]}. Omit \"hunks\" to stage the whole file. \
Use partial staging whenever a file mixes concerns rather than excluding a change \
that does belong.\n",
    );

    out
}

/// A coarse pass-1 partition of the candidates, decided from paths + change
/// sizes alone (no diff bodies). `needs_diff` files get a full-diff pass 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triage {
    pub settled_include: Vec<String>,
    pub settled_exclude: Vec<String>,
    pub needs_diff: Vec<String>,
}

#[derive(Deserialize, Default)]
struct WireTriage {
    #[serde(default)]
    settled_include: Vec<String>,
    #[serde(default)]
    settled_exclude: Vec<String>,
    #[serde(default)]
    needs_diff: Vec<String>,
}

/// Count added / removed lines in a unified diff (ignoring the `+++`/`---`
/// file headers), for the pass-1 change-size hint.
fn diff_line_counts(diff: &str) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => added += 1,
            Some(b'-') => removed += 1,
            _ => {}
        }
    }
    (added, removed)
}

/// Build the lean pass-1 prompt: paths, statuses, and change sizes only — no
/// diff bodies — asking the model to bucket every file into settled-in,
/// settled-out, or needs-a-closer-look.
pub fn build_triage_prompt(message: &str, candidates: &[Candidate]) -> String {
    let mut out = String::new();
    out.push_str(
        "You are triaging a git working tree that contains SEVERAL UNRELATED changes \
made concurrently by parallel agents. Given ONE commit message, bucket every \
candidate file WITHOUT yet seeing its diff, using only its path and change size:\n\
- settled_include: you are confident the WHOLE file belongs to this commit.\n\
- settled_exclude: you are confident it belongs to a DIFFERENT change.\n\
- needs_diff: you cannot tell from the path, OR the file might mix changes from \
several concerns (so it may need partial staging). When unsure, choose needs_diff.\n\
EVERY candidate must appear in exactly one list.\n\n",
    );
    out.push_str("COMMIT MESSAGE:\n");
    out.push_str(message);
    out.push_str("\n\nCANDIDATE FILES:\n");
    for c in candidates {
        let (added, removed) = diff_line_counts(&c.diff);
        out.push_str("- ");
        out.push_str(&c.path);
        out.push_str("  [");
        out.push_str(c.status.label());
        out.push_str(", +");
        out.push_str(&added.to_string());
        out.push_str(" -");
        out.push_str(&removed.to_string());
        out.push_str("]\n");
    }
    out.push_str(
        "\nRespond with ONLY this JSON — no prose:\n\
{\"settled_include\":[\"path\",...],\"settled_exclude\":[\"path\",...],\"needs_diff\":[\"path\",...]}\n\
Every path MUST be one of the candidates above.\n",
    );
    out
}

/// Parse the pass-1 reply into a [`Triage`]. Unknown paths are dropped; any
/// candidate the model failed to bucket is escalated to `needs_diff` (a closer
/// look is the safe default for anything unaccounted for).
pub fn parse_triage(raw_json: &str, candidates: &[Candidate]) -> Triage {
    let wire: WireTriage = extract_json_object(raw_json)
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or_default();

    let known = |p: &String| candidates.iter().any(|c| &c.path == p);
    let keep = |v: Vec<String>| v.into_iter().filter(&known).collect::<Vec<_>>();
    let settled_include = keep(wire.settled_include);
    let settled_exclude = keep(wire.settled_exclude);
    let mut needs_diff = keep(wire.needs_diff);

    let unbucketed: Vec<String> = candidates
        .iter()
        .filter(|c| {
            !settled_include.iter().any(|x| x == &c.path)
                && !settled_exclude.iter().any(|x| x == &c.path)
                && !needs_diff.iter().any(|x| x == &c.path)
        })
        .map(|c| c.path.clone())
        .collect();
    needs_diff.extend(unbucketed);

    Triage { settled_include, settled_exclude, needs_diff }
}

/// Combine a pass-1 [`Triage`] with the optional pass-2 [`Selection`] over its
/// `needs_diff` files into one final [`Selection`]. Settled files become
/// High-confidence whole-file decisions; pass-2 files carry their own verdicts
/// (including any partial-hunk selection) and set the overall confidence.
pub fn merge_triage(triage: Triage, pass2: Option<Selection>) -> Selection {
    let mut include: Vec<Included> = triage
        .settled_include
        .into_iter()
        .map(|path| Included {
            path,
            reason: "clearly in scope from its path".to_string(),
            confidence: Confidence::High,
            hunks: None,
        })
        .collect();
    let mut exclude: Vec<Excluded> = triage
        .settled_exclude
        .into_iter()
        .map(|path| Excluded {
            path,
            reason: "clearly a different change from its path".to_string(),
            confidence: Confidence::High,
            omitted: false,
        })
        .collect();

    let (overall, note) = match pass2 {
        Some(p) => {
            include.extend(p.include);
            exclude.extend(p.exclude);
            (p.overall, p.note)
        }
        None => (Confidence::High, None),
    };

    Selection { include, exclude, overall, note }
}

/// Max diff lines shown for any one file. The whole prompt's diff bodies share
/// [`GLOBAL_LINE_BUDGET`]; whichever bites first wins.
const PER_FILE_LINE_CAP: usize = 300;
/// Total diff lines across all files' bodies in one prompt.
const GLOBAL_LINE_BUDGET: usize = 2000;

/// Render one candidate's uncapped body: labelled hunks when the diff has them,
/// else the raw diff/content for untracked or binary files. Capping is applied
/// by the caller against the shared budget.
fn candidate_body(c: &Candidate) -> String {
    if c.diff.trim().is_empty() {
        return "(no diff shown)".to_string();
    }
    let parsed = parse_hunks(&c.diff);
    if parsed.hunks.is_empty() {
        return format!("```\n{}\n```", c.diff.trim_end());
    }
    let mut out = String::new();
    for h in &parsed.hunks {
        out.push('[');
        out.push_str(&h.id);
        out.push_str("]\n```\n");
        out.push_str(h.text.trim_end());
        out.push_str("\n```\n");
    }
    out
}

/// Token usage reported by the model for one `pick` (summed across retries).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Total input tokens (uncached + cache-read + cache-creation).
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The portion of `input_tokens` that was served from cache (a subset).
    pub cache_read_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl std::ops::Add for Usage {
    type Output = Usage;
    fn add(self, other: Usage) -> Usage {
        Usage {
            input_tokens: self.input_tokens + other.input_tokens,
            output_tokens: self.output_tokens + other.output_tokens,
            cache_read_tokens: self.cache_read_tokens + other.cache_read_tokens,
        }
    }
}

#[derive(Deserialize, Default)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

/// Pull token usage from the `claude -p --output-format json` envelope. Cache
/// read/creation tokens count toward input. Missing usage reads as zero.
pub fn extract_usage(envelope_json: &str) -> Usage {
    let Ok(env) = serde_json::from_str::<Envelope>(envelope_json) else {
        return Usage::default();
    };
    let u = env.usage.unwrap_or_default();
    Usage {
        input_tokens: u.input_tokens + u.cache_read_input_tokens + u.cache_creation_input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_input_tokens,
    }
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
    #[serde(default)]
    hunks: Option<Vec<String>>,
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

/// Outcome of validating a wire `hunks` list for an included file.
enum HunkResolution {
    /// Drop the include: a partial selection named no real hunk, so staging the
    /// whole file would risk staging changes the model wanted left out.
    Drop,
    /// Stage the whole file (no `hunks` given).
    WholeFile,
    /// Stage only these (validated, existing) hunk ids.
    Partial(Vec<String>),
}

/// Resolve a wire `hunks` list into a validated selection for `path`, keeping
/// only hunk ids that really exist in the candidate's diff.
fn validate_hunks(
    requested: Option<Vec<String>>,
    path: &str,
    candidates: &[Candidate],
) -> HunkResolution {
    let Some(requested) = requested else {
        return HunkResolution::WholeFile;
    };
    let Some(cand) = candidates.iter().find(|c| c.path == path) else {
        return HunkResolution::Drop;
    };
    let valid: Vec<String> = parse_hunks(&cand.diff).hunks.into_iter().map(|h| h.id).collect();
    let kept: Vec<String> = requested.into_iter().filter(|id| valid.contains(id)).collect();
    if kept.is_empty() {
        HunkResolution::Drop
    } else {
        HunkResolution::Partial(kept)
    }
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
        .filter_map(|e| {
            let hunks = match validate_hunks(e.hunks, &e.path, candidates) {
                HunkResolution::Drop => return None,
                HunkResolution::WholeFile => None,
                HunkResolution::Partial(ids) => Some(ids),
            };
            Some(Included {
                confidence: Confidence::from_wire(e.confidence.as_deref()),
                path: e.path,
                reason: e.reason,
                hunks,
            })
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
            omitted: false,
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
            omitted: true,
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
fn run_claude(prompt: &str, cfg: &ClaudeCfg) -> Result<(String, Usage), PickError> {
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
    let text = extract_result_text(&envelope).map_err(PickError::Parse)?;
    Ok((text, extract_usage(&envelope)))
}

/// Ask Sonnet (via `claude -p`) which candidates to stage for `message`.
///
/// Impure: shells out to `cfg.program`. On a first-pass parse failure it retries
/// once with a terser "return ONLY the JSON" nudge before giving up.
pub fn pick(
    message: &str,
    candidates: &[Candidate],
    cfg: &ClaudeCfg,
) -> Result<(Selection, Usage), PickError> {
    let prompt = build_prompt(message, candidates);
    run_and_parse(&prompt, candidates, cfg)
}

/// Run a full-diff prompt and parse the reply into a [`Selection`], retrying
/// once with a terser nudge if the first reply doesn't parse.
fn run_and_parse(
    prompt: &str,
    candidates: &[Candidate],
    cfg: &ClaudeCfg,
) -> Result<(Selection, Usage), PickError> {
    let (text, usage) = run_claude(prompt, cfg)?;
    if let Ok(sel) = parse_reply(&text, candidates) {
        return Ok((sel, usage));
    }

    let retry = format!(
        "{prompt}\n\nIMPORTANT: your previous answer could not be parsed. \
Reply with ONLY the raw JSON object, nothing else."
    );
    let (text, retry_usage) = run_claude(&retry, cfg)?;
    let sel = parse_reply(&text, candidates).map_err(PickError::Parse)?;
    Ok((sel, usage + retry_usage))
}

/// Two-pass triage: a lean pass-1 call (paths + change sizes, no diff bodies)
/// buckets the files; only the `needs_diff` subset gets a full-diff pass-2 call.
/// When pass 1 settles everything, one cheap call decides the whole tree.
///
/// If the pass-1 reply is unparseable, [`parse_triage`] escalates everything to
/// `needs_diff`, degrading gracefully to a single full-diff pass.
pub fn pick_two_pass(
    message: &str,
    candidates: &[Candidate],
    cfg: &ClaudeCfg,
) -> Result<(Selection, Usage), PickError> {
    let triage_prompt = build_triage_prompt(message, candidates);
    let (text, usage) = run_claude(&triage_prompt, cfg)?;
    let triage = parse_triage(&text, candidates);

    if triage.needs_diff.is_empty() {
        return Ok((merge_triage(triage, None), usage));
    }

    let subset: Vec<Candidate> = candidates
        .iter()
        .filter(|c| triage.needs_diff.iter().any(|p| p == &c.path))
        .cloned()
        .collect();
    let subset_prompt = build_prompt(message, &subset);
    let (pass2, pass2_usage) = run_and_parse(&subset_prompt, &subset, cfg)?;
    Ok((merge_triage(triage, Some(pass2)), usage + pass2_usage))
}

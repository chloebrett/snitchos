//! Recipe tuples — the diversity axes, as data.
//!
//! One recipe repeated five hundred times is one program written five hundred
//! ways, and a corpus of that teaches a model one program. The axes come from
//! [`plans/corpus-recipe-axes.md`]; `assets/recipes.toml` was generated from it,
//! and the doc stays the source of truth for *why* these are the axes.
//!
//! **Each domain carries a distinguishing clause**, and that is the load-bearing
//! part. A bare domain name lets a weak model default to the same
//! records-with-timestamps-plus-filter program for every entry; the clause names
//! the actual computation, which is what makes `sauna booking` (interval
//! overlap), `ice rink session booking` (capacity over a recurring timetable)
//! and `library hold queue` (FIFO per title with expiry) diverge into three
//! genuinely different programs.

const RECIPES: &str = include_str!("../assets/recipes.toml");

/// One point in the axis space.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Recipe {
    pub domain: String,
    /// What the program actually computes — the reason this domain is not
    /// interchangeable with the next one.
    pub clause: String,
    pub constructs: String,
    /// `small` | `medium` | `large`.
    pub size: String,
    /// `module` | `script` | `server loop` | `library-with-heavy-tests`.
    pub shape: String,
}

#[derive(serde::Deserialize)]
struct Sheet {
    recipe: Vec<Recipe>,
}

/// Parsed on demand. The file is a few hundred lines and a batch calls this
/// once per candidate against a model that takes thirty seconds to answer, so
/// caching it would be optimising the wrong end.
///
/// # Panics
/// If `assets/recipes.toml` is malformed — it is compiled in, so that is a
/// build-time mistake surfacing at the first call rather than a runtime input.
fn rows() -> Vec<Recipe> {
    let sheet: Sheet =
        toml::from_str(RECIPES).expect("assets/recipes.toml is malformed");
    sheet.recipe
}

/// How many recipes exist.
#[must_use]
pub fn count() -> usize {
    rows().len()
}

/// The `index`-th recipe, wrapping — so `--count 500` is five passes over the
/// whole set rather than five hundred of the first one.
#[must_use]
pub fn nth(index: usize) -> Recipe {
    let all = rows();
    let at = index % all.len();
    all[at].clone()
}

impl Recipe {
    /// Render the recipe as a brief.
    ///
    /// **A brief, not a list of axes.** `Domain: tide table / Shape: server loop`
    /// makes the model reconcile two independent facts, and reconciliation is
    /// where a small model fails; "a tides API: a server loop that answers
    /// queries against a tide table" has the reconciliation already done.
    ///
    /// Size is counted in **declarations, never lines**: a model cannot count
    /// lines and will delete working code trying to hit a number it computed
    /// wrong (`plans/corpus-mvp-spike.md` Findings 004).
    #[must_use]
    pub fn render(&self) -> String {
        let (types, functions) = match self.size.as_str() {
            "small" => ("1 to 4", "2 to 6"),
            "large" => ("3 to 10", "8 to 20"),
            _ => ("2 to 6", "4 to 10"),
        };
        let shape = match self.shape.as_str() {
            "script" => "a script — it has a `main`",
            "server loop" => "a server loop — receive a request, dispatch, respond",
            "library-with-heavy-tests" => {
                "a library — `ext` items, no `main`, and unusually thorough tests"
            }
            _ => "a module — `ext` items, no `main`",
        };
        format!(
            "Write a {} module: {}.\n\n\
             Shape: {shape}.\n\
             Size: {types} types and {functions} functions. Tests are extra and do not\n\
             count toward that. This is a rough guide, not a limit: never delete working\n\
             code or drop a test to hit it, and if the program naturally wants to be\n\
             bigger, let it be.\n\
             Use these constructs where they fit: {}\n\
             Name things for what they do.",
            self.domain, self.clause, self.constructs
        )
    }
}

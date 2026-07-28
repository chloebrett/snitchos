//! Recipe tuples — the diversity axes, as data.
//!
//! One recipe repeated five hundred times is one program written five hundred
//! ways, and a corpus of that teaches a model one program. The axes come from
//! `plans/corpus-recipe-axes.md`; the sheets in `assets/recipes/` were
//! generated from it, and the doc stays the source of truth for *why* these are
//! the axes.
//!
//! **Each domain carries a distinguishing clause**, and that is the load-bearing
//! part. A bare domain name lets a weak model default to the same
//! records-with-timestamps-plus-filter program for every entry; the clause names
//! the actual computation, which is what makes `sauna booking` (interval
//! overlap), `ice rink session booking` (capacity over a recurring timetable)
//! and `library hold queue` (FIFO per title with expiry) diverge into three
//! genuinely different programs.
//!
//! # One sheet per batch
//!
//! A sheet is **frozen once a batch has been generated from it**. `batch9.toml`
//! is the record of what produced `corpora/batch9`, down to the wording its
//! briefs were rendered with; reading a finding about that corpus back onto the
//! axes that produced it is only possible while the two still correspond. So a
//! revision is a *new sheet*, never an edit to an old one, and `--recipes`
//! picks between them.

/// The sheets, in the order they were written. The last one is the default —
/// a new batch should use the newest axes unless it is deliberately reproducing
/// an old one.
const SHEETS: &[(&str, &str)] = &[
    ("batch9", include_str!("../assets/recipes/batch9.toml")),
    ("batch10", include_str!("../assets/recipes/batch10.toml")),
];

/// The sheet a run uses when `--recipes` is not given.
pub const DEFAULT: &str = "batch10";

/// How much latitude a brief gives the model around its size bucket.
///
/// batch9's yield was a monotone function of program length — 16% parse-death in
/// the shortest decile against 92% in the longest (`notes/batch9-findings.md`
/// Finding 1) — because every extra token is another chance for the correction
/// guard to splice badly. `Grow` is the wording that produced that; `Hold` is
/// the response to it.
///
/// Both are expressed in **declarations, never lines**: a model cannot count
/// lines and will delete working code trying to hit a number it computed wrong
/// (`plans/corpus-mvp-spike.md` Findings 004). The cap has to be in a currency
/// the model can actually count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Latitude {
    /// "if the program naturally wants to be bigger, let it be".
    Grow,
    /// Stay at or under the bucket; a longer program is not a better one.
    Hold,
}

/// What a brief calls the program in its opening line.
///
/// `Module` calls everything a module and then describes its actual shape on the
/// next line. batch9 could carry that contradiction because 62% of it really was
/// modules; a sheet that spreads its shapes contradicts itself more often than
/// not, and reconciling two facts that disagree is exactly where a small model
/// fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    /// Always "module", whatever the shape says. batch9's wording.
    Module,
    /// The noun the shape actually is.
    Shape,
}

/// A sheet's policy — the parts of a brief that are the same for every recipe in
/// it, and so belong to the sheet rather than repeated across a thousand rows.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Policy {
    #[serde(default = "grow")]
    pub latitude: Latitude,
    #[serde(default = "module")]
    pub subject: Subject,
}

fn grow() -> Latitude {
    Latitude::Grow
}

fn module() -> Subject {
    Subject::Module
}

impl Default for Policy {
    /// What a sheet with no `[policy]` block gets: the wording batch9 was
    /// generated with, so an old sheet keeps rendering the way it always did.
    fn default() -> Self {
        Self { latitude: Latitude::Grow, subject: Subject::Module }
    }
}

/// One point in the axis space, with its crossing resolved.
#[derive(Debug, Clone)]
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
    /// From the sheet, not the row.
    pub latitude: Latitude,
    /// From the sheet, not the row.
    pub subject: Subject,
}

/// A crossing of the non-domain axes: what a domain is asked for *this time*.
#[derive(Debug, Clone, serde::Deserialize)]
struct Crossing {
    constructs: String,
    size: String,
    shape: String,
}

/// A row as written in a sheet.
///
/// Two spellings, because the sheets disagree and both are right. batch9 gives
/// each domain one crossing inline; batch10 gives each domain a `crossings`
/// list, so the clause is written once and cannot drift between the two askings
/// of the same domain.
#[derive(Debug, Clone, serde::Deserialize)]
struct Row {
    domain: String,
    clause: String,
    constructs: Option<String>,
    size: Option<String>,
    shape: Option<String>,
    #[serde(default)]
    crossings: Vec<Crossing>,
}

impl Row {
    /// The crossings this row asks for, however it spelled them.
    fn crossings(&self) -> Result<Vec<Crossing>, String> {
        if !self.crossings.is_empty() {
            return Ok(self.crossings.clone());
        }
        match (&self.constructs, &self.size, &self.shape) {
            (Some(constructs), Some(size), Some(shape)) => Ok(vec![Crossing {
                constructs: constructs.clone(),
                size: size.clone(),
                shape: shape.clone(),
            }]),
            _ => Err(format!("{}: no crossings, and no inline constructs/size/shape", self.domain)),
        }
    }
}

#[derive(serde::Deserialize)]
struct Parsed {
    #[serde(default)]
    policy: Policy,
    recipe: Vec<Row>,
}

/// A named set of recipes, already flattened into one crossing per entry.
#[derive(Debug)]
pub struct Sheet {
    pub name: String,
    pub policy: Policy,
    recipes: Vec<Recipe>,
}

/// The sheet called `name`.
///
/// # Errors
/// If no sheet has that name — a typo'd `--recipes` silently falling back to the
/// default would generate a batch against axes nobody chose, and the manifest
/// would not say so.
pub fn sheet(name: &str) -> Result<Sheet, String> {
    let (_, text) = SHEETS
        .iter()
        .find(|(sheet, _)| *sheet == name)
        .ok_or_else(|| format!("no recipe sheet `{name}` — there is {}", names().join(", ")))?;
    Sheet::parse(name, text)
}

/// Every sheet that exists, oldest first.
#[must_use]
pub fn names() -> Vec<&'static str> {
    SHEETS.iter().map(|(name, _)| *name).collect()
}

impl Sheet {
    /// Parse sheet text. Public so the format itself is testable against a
    /// fixture rather than only against the thousand rows that ship.
    ///
    /// # Errors
    /// If the TOML is malformed, a row has no crossings, or the sheet is empty —
    /// generating against nothing is the failure that looks like success.
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let parsed: Parsed = toml::from_str(text).map_err(|error| format!("{name}: {error}"))?;

        // Pass-major: every domain's first crossing, then every domain's second.
        // A 500-candidate run over a 500-domain sheet then sees all 500 domains
        // once, rather than the first 250 twice — the axes only buy variety if a
        // short run gets the spread as well as a long one.
        let crossings: Vec<Vec<Crossing>> =
            parsed.recipe.iter().map(Row::crossings).collect::<Result<_, _>>()?;
        let passes = crossings.iter().map(Vec::len).max().unwrap_or(0);

        let recipes: Vec<Recipe> = (0..passes)
            .flat_map(|pass| {
                parsed.recipe.iter().zip(&crossings).filter_map(move |(row, crossings)| {
                    crossings.get(pass).map(|crossing| Recipe {
                        domain: row.domain.clone(),
                        clause: row.clause.clone(),
                        constructs: crossing.constructs.clone(),
                        size: crossing.size.clone(),
                        shape: crossing.shape.clone(),
                        latitude: parsed.policy.latitude,
                        subject: parsed.policy.subject,
                    })
                })
            })
            .collect();

        if recipes.is_empty() {
            return Err(format!("{name}: no recipes"));
        }
        Ok(Self { name: name.to_string(), policy: parsed.policy, recipes })
    }

    /// How many recipes the sheet holds, crossings counted separately.
    #[must_use]
    pub fn count(&self) -> usize {
        self.recipes.len()
    }

    /// The `index`-th recipe, wrapping — so `--count 2000` over a 1000-row sheet
    /// is two passes over the whole set rather than 2000 of the first one.
    ///
    /// # Panics
    /// Never: [`Sheet::parse`] rejects an empty sheet.
    #[must_use]
    pub fn nth(&self, index: usize) -> Recipe {
        self.recipes[index % self.recipes.len()].clone()
    }

    /// Every distinct domain in the sheet.
    #[must_use]
    pub fn domains(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = self.recipes.iter().map(|recipe| recipe.domain.as_str()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen
    }
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
        let subject = match self.subject {
            Subject::Module => "module",
            Subject::Shape => match self.shape.as_str() {
                "script" => "script",
                "server loop" => "service",
                "library-with-heavy-tests" => "library",
                _ => "module",
            },
        };
        let latitude = match self.latitude {
            Latitude::Grow => {
                "This is a rough guide, not a limit: never delete working\n\
                 code or drop a test to hit it, and if the program naturally wants to be\n\
                 bigger, let it be."
            }
            Latitude::Hold => {
                "Stay within it. A longer program is not a better one — cover\n\
                 the core computation, test it, and stop. Never delete working code or drop\n\
                 a test to hit the number."
            }
        };
        format!(
            "Write a {} {subject}: {}.\n\n\
             Shape: {shape}.\n\
             Size: {types} types and {functions} functions. Tests are extra and do not\n\
             count toward that. {latitude}\n\
             Use these constructs where they fit: {}\n\
             Name things for what they do.",
            self.domain, self.clause, self.constructs
        )
    }
}

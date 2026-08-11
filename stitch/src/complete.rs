//! Tab completion: what can the user type next, and can we type it for them?
//!
//! A pure function of `(line, cursor)` over [`crate::oracle`]. The REPL and
//! (later) stim both consume it; neither owns it — the same shape as the
//! oracle itself.
//!
//! The division this realizes: **the grammar says what is legal, a model says
//! what is likely**. Everything here is the grammar half, so it needs no model
//! and no IPC — and where exactly one spelling is legal, it is not a suggestion
//! at all but a certainty.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::oracle::{Entry, TokenClass, describe, has_one_spelling, representative, valid_next_in};

/// Something that can answer "what may be typed at the end of this line?".
///
/// The seam. The line editor holds one of these rather than calling
/// [`complete`] directly, so the grammar-only completer and a later
/// model-ranked one are a *substitution*, not a rewrite — and so the editor
/// stays a pure, host-testable thing that never knows whether a model exists.
pub trait Completer {
    fn complete_line(&self, line: &str) -> Completion;
}

/// The grammar alone: no model, no IPC, no network. Correct by construction
/// and instant; the floor any ranked completer has to beat.
pub struct GrammarCompleter;

impl Completer for GrammarCompleter {
    fn complete_line(&self, line: &str) -> Completion {
        // The forced-token livelock is the grammar's own, not the model's — see
        // [`reopened`]. A completer that skipped this would loop here too.
        let (closer, line) = reopened(line);
        behind(closer, complete(&line, line.len()))
    }
}

/// The grammar, plus a completion service for the cases the grammar cannot
/// decide.
///
/// **A forced token never costs a round trip.** Where exactly one spelling is
/// legal the grammar already knows the answer, and no model can improve on a
/// certainty — waiting on one would make the best case the slowest. A dead line
/// is the same: nothing can rescue it. Only an ambiguous choice is worth
/// asking about.
///
/// This is also where the round-trip discipline belongs, rather than in the
/// line editor: putting it there would have leaked grammar knowledge into a
/// component whose whole virtue is not having any.
pub struct ModelCompleter<'a> {
    platform: &'a dyn crate::platform::Platform,
    max_tokens: u32,
}

impl<'a> ModelCompleter<'a> {
    #[must_use]
    pub fn new(platform: &'a dyn crate::platform::Platform, max_tokens: u32) -> Self {
        Self { platform, max_tokens }
    }
}

/// If the line ends inside a comment, the only useful completion is the one that ends
/// the comment.
///
/// **Two bugs live here, and they are the same bug.** A completer decides what may
/// come next from the token stream but writes bytes at the end of the buffer, and
/// inside a comment the lexer has skipped everything between them. So:
///
/// - a *forced* token appended there never joins the token stream, the grammar state
///   never advances, and the same token is forced again on the next keystroke —
///   observed on the VF2 as a line filling with `(((((((`, one byte per Tab, forever;
/// - a *model* suggestion there is unconstrained, because every byte extends a comment
///   legally, so the completion budget goes on prose. drivel's corpus is ~47% comments,
///   so it stays once it drifts in.
///
/// Emitting the closer rather than declining is deliberate: declining would strand the
/// line, and the point of pressing Tab repeatedly is to keep building. One Tab closes
/// the comment; the next resumes ordinary completion with the insertion point back in
/// code.
/// The text that must precede any completion for this line, and the line as it will
/// read once that text is in place.
///
/// Returned together rather than applied early because the closer is not the answer —
/// it is a *prefix* to the answer. Closing the comment and stopping would spend a whole
/// keypress on a newline, which is a third of them when the model is writing comments;
/// closing and then completing against the reopened line spends none.
fn reopened(line: &str) -> (&'static str, String) {
    let closer = crate::lexer::trailing_region(line).closer();
    let mut effective = String::from(line);
    effective.push_str(closer);
    (closer, effective)
}

/// `completion`, with `closer` in front of whatever text it carries.
///
/// A `Choices` answer loses its menu here, deliberately: when a comment is open the
/// menu is the one thing that cannot help — every byte is legal inside a comment, so
/// the classes it would list are about a position the user is not at. Closing is the
/// only progress available, so offer that instead.
fn behind(closer: &'static str, completion: Completion) -> Completion {
    if closer.is_empty() {
        return completion;
    }
    match completion {
        Completion::Forced(text) => Completion::Forced(alloc::format!("{closer}{text}")),
        Completion::Suggested(text) => Completion::Suggested(alloc::format!("{closer}{text}")),
        Completion::Choices(_) | Completion::None => Completion::Suggested(String::from(closer)),
    }
}

impl Completer for ModelCompleter<'_> {
    fn complete_line(&self, line: &str) -> Completion {
        // An open comment first: until it closes, the insertion point and the token
        // stream are in different places and nothing downstream can make progress.
        let (closer, line) = reopened(line);
        let line = line.as_str();

        let grammar = complete(line, line.len());
        let Completion::Choices(choices) = grammar else {
            return behind(closer, grammar); // forced or dead — decided already, and for free
        };
        let Some(text) = self.platform.complete(line, self.max_tokens) else {
            // No service: the menu is the floor — but a pending closer still beats it.
            return behind(closer, Completion::Choices(choices));
        };
        // The suggestion crossed a process boundary. kvetch only emits
        // oracle-approved tokens, but a client that *assumed* that would be
        // trusting another process to police its own output — so check here
        // that the line survives it. A suggestion that kills the buffer is
        // worse than no suggestion.
        let extended = format!("{line}{text}");
        let survives = !valid_next_in(&extended, extended.len(), Entry::Program)
            .union(valid_next_in(&extended, extended.len(), Entry::Expr))
            .is_empty();
        if text.is_empty() || !survives {
            return behind(closer, Completion::Choices(choices));
        }
        behind(closer, Completion::Suggested(text))
    }
}

/// Completes nothing — what [`crate::line_edit::LineEditor::feed`] uses, so
/// callers that never opted in behave exactly as they did before completion
/// existed.
pub struct NoCompleter;

impl Completer for NoCompleter {
    fn complete_line(&self, _line: &str) -> Completion {
        Completion::None
    }
}

/// How many choices a menu names before summarising. An expression position
/// admits about seventeen openers and a bare identifier two dozen operators;
/// listing them all at a prompt buries the useful ones. Same policy, and the
/// same shape of ellipsis, as `ParseError::render` — a person reading either is
/// after the *first* few plausible things, not an inventory.
pub const MENU_LIMIT: usize = 8;

/// Render `choices` for a human: descriptions, not variant names, capped at
/// [`MENU_LIMIT`] with a count of what was hidden.
///
/// Capping lives here rather than in [`complete`] because it is presentation
/// policy: a ranker (a model, later) wants the whole legal set, and only the
/// display is bounded.
///
/// **Renders whatever order it is handed.** Ranking is deliberately not done
/// here: this is the grammar layer, which knows what is *legal* and has no
/// opinion about what is *likely*. A ranker is therefore a pure pre-sort of
/// `choices`, with no change to this function.
///
/// The oracle's discriminant order is what callers pass today, and it is
/// measurably good for expression *openers* (literals and names first) and
/// measurably bad for *continuations*: after a bare name the useful
/// suggestions are `(` and `.`, but the cap fills with `and`, `or` and
/// arithmetic first. That gap is the concrete case for ranking — the first
/// place a model would earn its place here, and a measurable target rather
/// than a hunch.
#[must_use]
pub fn menu(choices: &[TokenClass]) -> String {
    let shown: Vec<String> = choices.iter().copied().take(MENU_LIMIT).map(describe).collect();
    let listed = shown.join(", ");
    if choices.len() > MENU_LIMIT {
        format!("{listed}, … ({} total)", choices.len())
    } else {
        listed
    }
}

/// What the completer can offer at a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Exactly one thing can be written here, and there is only one way to
    /// write it. Not a suggestion — a certainty, so it can be inserted without
    /// asking anything (and without a round trip to a model).
    Forced(String),
    /// A model's guess: legal, but one of several legal things. Distinct from
    /// [`Self::Forced`] so a caller can treat certainty and guesswork
    /// differently — the register of an editor hint, if nothing else.
    Suggested(String),
    /// Several classes are legal, or one that the user must spell themselves.
    Choices(Vec<TokenClass>),
    /// Nothing can follow: the line is dead in both readings.
    None,
}

/// What may be typed at `cursor` in `line`?
///
/// **Both readings, unioned.** A REPL line may be a declaration or an
/// expression — `Repl::eval_line` tries `parse_program` and falls back to
/// `parse` — so the answer is the union of both entries. Offering only one
/// would mean guessing mid-line which the user meant, and being confidently
/// wrong half the time. A consequence worth knowing: a token is `Forced` only
/// when *both* readings force the same single spelling, which makes the forced
/// case rarer and every instance of it trustworthy.
///
/// `Eof` is dropped: "you could stop here" is true but not typeable.
#[must_use]
pub fn complete(line: &str, cursor: usize) -> Completion {
    let legal = valid_next_in(line, cursor, Entry::Program)
        .union(valid_next_in(line, cursor, Entry::Expr))
        .without(TokenClass::Eof);
    if legal.is_empty() {
        return Completion::None;
    }
    let classes = legal.to_vec();
    match classes.as_slice() {
        [only] if has_one_spelling(*only) => match representative(*only) {
            Some(lexeme) => Completion::Forced(String::from(lexeme)),
            None => Completion::Choices(classes),
        },
        _ => Completion::Choices(classes),
    }
}

#[cfg(test)]
mod tests {
    use super::{Completer, Completion, GrammarCompleter, complete};
    use crate::oracle::TokenClass;
    use alloc::string::String;

    /// **The livelock, pinned.** A line ending inside a comment used to make the
    /// grammar force the same token forever: the lexer skips the comment, so the
    /// appended byte never reaches the token stream, the grammar state never advances,
    /// and the next press forces it again. On the VF2 this filled a line with
    /// `(((((((`, one byte per Tab.
    ///
    /// The property asserted is "repeatedly completing makes progress", not "the
    /// completion is a newline" — progress is what was violated, and pinning the exact
    /// text would pass just as well for a loop that emitted something else.
    #[test]
    fn completing_repeatedly_inside_a_comment_makes_progress_rather_than_looping() {
        let mut line = String::from("greet(name)\n    // Try to");

        for press in 0..4 {
            let before = line.clone();
            match GrammarCompleter.complete_line(&line) {
                Completion::Forced(text) | Completion::Suggested(text) => line.push_str(&text),
                Completion::Choices(_) | Completion::None => break,
            }
            assert_ne!(line, before, "press {press} appended nothing");
        }

        assert_eq!(
            crate::lexer::trailing_region(&line),
            crate::lexer::Trailing::Code,
            "still inside a comment after four presses: {line:?}",
        );
    }

    /// A block comment is not closed by a newline, so the completer must offer `*/` —
    /// the case a "just emit a newline" fix would loop on forever.
    #[test]
    fn an_open_block_comment_is_closed_by_its_own_terminator() {
        assert_eq!(
            GrammarCompleter.complete_line("greet(name) /* aside"),
            Completion::Suggested("*/".into()),
        );
    }

    #[test]
    fn a_single_legal_spelling_is_typed_for_the_user() {
        // After `use M.` only `{` can follow — as a program. As an *expression*
        // the line is already dead (`use` cannot open one), so the union is the
        // program's answer alone, and it is forced.
        assert_eq!(complete("use M.", 6), Completion::Forced("{".into()));
    }

    #[test]
    fn a_forced_class_with_no_single_spelling_is_only_a_suggestion() {
        // After `use`, exactly one *class* is legal — an identifier — but there
        // is no lexeme to insert: only the user knows the module's name. A
        // completer that typed the oracle's probe lexeme (`x`) here would be
        // inventing code.
        assert_eq!(complete("use", 3), Completion::Choices(vec![TokenClass::Ident]));
    }

    #[test]
    fn an_ambiguous_position_offers_its_choices() {
        let Completion::Choices(choices) = complete("let x = ", 8) else {
            panic!("an expression position has many legal openers");
        };
        for class in [TokenClass::Int, TokenClass::Ident, TokenClass::LParen] {
            assert!(choices.contains(&class), "{class:?} should be offered");
        }
        assert!(
            !choices.contains(&TokenClass::Eof),
            "`end of input` is not something the user can type"
        );
    }

    #[test]
    fn both_readings_are_offered_because_the_repl_accepts_either() {
        // A REPL line may be a declaration *or* an expression (`Repl::eval_line`
        // tries `parse_program` first, then `parse`). After `greet` the program
        // reading forces `(` — a function declaration — while the expression
        // reading allows every infix operator. Offering only one would guess at
        // what the user meant mid-line.
        let Completion::Choices(choices) = complete("greet", 5) else {
            panic!("both readings are live here, so nothing is forced");
        };
        assert!(choices.contains(&TokenClass::LParen), "the declaration reading");
        assert!(choices.contains(&TokenClass::Plus), "the expression reading");
    }

    #[test]
    fn a_short_menu_lists_every_choice() {
        // After `use M.{ a` only `}` or `,` can follow.
        let Completion::Choices(choices) = complete("use M.{ a", 9) else {
            panic!("two continuations, so nothing is forced");
        };
        assert_eq!(super::menu(&choices), "`}`, `,`");
    }

    #[test]
    fn a_long_menu_is_capped_and_says_how_much_it_hid() {
        // An expression position admits ~17 openers; listing them all at a
        // prompt is a wall of text, so the menu shows a handful and counts the
        // rest — the same policy as `ParseError::render`.
        let Completion::Choices(choices) = complete("let x = ", 8) else {
            panic!("an expression position has many openers");
        };
        let menu = super::menu(&choices);
        assert!(menu.ends_with(&format!("… ({} total)", choices.len())), "got: {menu}");
        assert_eq!(menu.matches(british_comma()).count(), super::MENU_LIMIT);
    }

    /// The separator `menu` joins with — spelled once so the count above cannot
    /// drift from the renderer.
    fn british_comma() -> &'static str {
        ", "
    }

    #[test]
    fn the_menu_describes_classes_rather_than_naming_them() {
        // A prompt should say "a name", not "Ident" — the menu is read by a
        // person, and `describe` is the same rendering diagnostics use.
        let Completion::Choices(choices) = complete("use", 3) else {
            panic!("an identifier is the only legal class");
        };
        assert_eq!(super::menu(&choices), "a name");
    }

    #[test]
    fn a_forced_token_never_costs_a_round_trip() {
        // The discipline that keeps completion instant where it can be: the
        // grammar already knows `{` is the only thing that follows `use M.`, so
        // no service is consulted. A model cannot improve a certainty, and
        // waiting on one would make the best case the slowest.
        let platform = crate::platform::FakePlatform::new();
        platform.set_completion("SHOULD NOT BE ASKED");
        let completer = super::ModelCompleter::new(&platform, 8);

        assert_eq!(completer.complete_line("use M."), Completion::Forced("{".into()));
        assert_eq!(platform.completions_requested(), 0);
    }

    #[test]
    fn an_ambiguous_position_asks_the_model() {
        let platform = crate::platform::FakePlatform::new();
        platform.set_completion(" 42");
        let completer = super::ModelCompleter::new(&platform, 8);

        assert_eq!(completer.complete_line("let x ="), Completion::Suggested(" 42".into()));
        assert_eq!(platform.completions_requested(), 1);
    }

    #[test]
    fn without_a_service_the_grammar_still_answers() {
        // No endpoint is the common case (the host CLI, the plain REPL). The
        // menu is a floor, not an error path.
        let platform = crate::platform::FakePlatform::new();
        let completer = super::ModelCompleter::new(&platform, 8);

        assert!(matches!(completer.complete_line("let x ="), Completion::Choices(_)));
    }

    #[test]
    fn an_illegal_suggestion_is_refused_and_the_menu_shown_instead() {
        // The suggestion crosses a process boundary. kvetch only emits
        // oracle-approved tokens, but a client that *assumed* that would be
        // trusting another process to police its own output — so check locally
        // that the line survives the suggestion.
        let platform = crate::platform::FakePlatform::new();
        platform.set_completion(" ;;;");
        let completer = super::ModelCompleter::new(&platform, 8);

        assert!(matches!(completer.complete_line("let x ="), Completion::Choices(_)));
    }

    #[test]
    fn a_dead_line_offers_nothing() {
        // `;` is lexed but never grammatical in either reading.
        assert_eq!(complete("greet() { 1 } ;", 15), Completion::None);
    }

    #[test]
    fn only_the_text_before_the_cursor_is_considered() {
        assert_eq!(complete("use M.{ a }", 6), Completion::Forced("{".into()));
    }
}

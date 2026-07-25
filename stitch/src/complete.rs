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
    let mut rendered = shown.join(", ");
    if choices.len() > MENU_LIMIT {
        rendered.push_str(&format!(", … ({} total)", choices.len()));
    }
    rendered
}

/// What the completer can offer at a position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Exactly one thing can be written here, and there is only one way to
    /// write it. Not a suggestion — a certainty, so it can be inserted without
    /// asking anything (and without a round trip to a model).
    Forced(String),
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
    use super::{Completion, complete};
    use crate::oracle::TokenClass;

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
    fn a_dead_line_offers_nothing() {
        // `;` is lexed but never grammatical in either reading.
        assert_eq!(complete("greet() { 1 } ;", 15), Completion::None);
    }

    #[test]
    fn only_the_text_before_the_cursor_is_considered() {
        assert_eq!(complete("use M.{ a }", 6), Completion::Forced("{".into()));
    }
}

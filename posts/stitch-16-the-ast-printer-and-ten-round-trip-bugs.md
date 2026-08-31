# Stitch 16 — the AST printer, and ten bugs found by round-trip

- I set out to write a pretty-printer. Take an AST, produce source. A weekend thing, maybe an evening — the grammar is small, I own the parser, and the contract writes itself: parse what you printed and you should get the same tree back.
- ten distinct bugs later, every single one found by a machine and not by me, I have a printer that round-trips a million programs and a much better understanding of what I actually built. this post is about the printer. the model it feeds is the next post.

## why a printer at all

- the short version: [babble](../docs/babble-design.md) — the grammar-directed sampler, rung zero of the model ladder — can generate unlimited valid Stitch, and it renders it as a **flat, space-separated token stream.** no newlines. no indentation. operators padded: `> =`, `?. name`, `( )`.
- that's not a bug in babble. every lexeme it emits is an independent choice from the continuation oracle, and the padding is what *preserves* those choices — write `>` and `=` adjacent and maximal munch re-reads them as `>=`, which is a different token than the two the oracle approved. the spaces are load-bearing for the generator.
- they're also a distribution real Stitch never occupies. train a model on that and it learns babble's *renderer*, not the language. so: don't teach the generator layout — parse its output and print the AST. the tree is already known-good. layout becomes one module's problem.
- (the [ladder doc](../docs/generative-ladder.md) has said "pretty-printing is required before Tier-0 output joins the real corpus" for a while. this is that.)

## the easy half

- precedence is the textbook part and it went exactly as expected. every position carries the minimum binding power the parser would accept there; a subexpression whose own binding power falls below it gets parenthesised. `(1 + 2) * 3` keeps its parens, `1 + 2 * 3` doesn't get any.
- one decision worth recording: **the binding powers come from the parser, not a second table.** `binding_power` and `is_non_assoc` went from private to `pub(crate)` and the printer calls them. a printer with its own copy of the precedence table is a printer that silently disagrees with the parser the first time someone adds an operator — and "silently disagrees" here means *emits a different program*.
- I wrote the round-trip test, wrote arms for every AST node, and had it green in an afternoon. every hand-written construct I could think of, plus every `.st` file the repo ships. done, I thought.

## the half I didn't see coming

- then I pointed it at babble and swept. seed 227:

```
1.5 ? . index ?        →  1.5?.index?
```

- the AST is `Try(Field(Try(1.5), index))` — `(1.5?).index?`. printed adjacent, `?` and `.` re-lex as `?.`, the safe-navigation token, and it comes back `Try(SafeField(1.5, index))`. a different program.
- that's maximal munch, the same hazard babble's padding exists to dodge, and I'd walked straight into it by *removing* the padding. fine — one seam, one guard. I added a `glue` helper for `?` before `.` and kept sweeping.
- the sweep kept going. and the thing it kept finding was not more precedence bugs. it was more **seams.**

## juxtaposition is application

- here is the fact underneath all of it. Stitch is whitespace-insensitive **and has no statement terminator.** no semicolons — the parser will tell you so by name if you type one. statements in a block are separated by nothing at all.
- which means the boundary between two statements is decided entirely by the tokens on either side of it. and since a call is written by putting an argument list next to an expression, **a statement that begins with `(` is read as a call of the statement above it.**
- so this, from seed 633, is not two statements:

```
{ }
(not { .. }.port ~> "frame") = score?
```

- it's one: the empty block, called with an argument. and I had *created* it, by parenthesising an assignment target that didn't need it.
- that reframed the whole problem. I had been thinking of parentheses as noise — an over-parenthesising printer is correct but emits a distribution real Stitch doesn't contain, which as training data is worse than useless. that's true and it is *not the main reason to care*. the main reason is that in this grammar, **a stray `(` is a correctness bug**, because it changes what the previous line means.

## the family album

- the sweep found ten classes. grouped by what's actually going on:
- **the lexer re-reads the seam.** `?` before `.` (`?.`), and `?` before `..=`, which fails outright rather than parsing wrong.
- **a token reaches forward for an identifier.** `@` alone at the end of a statement eats the next line's first word, because `@name` is receiver-field shorthand. `let a = @` above `beta() = 1` is the field `@beta` and then a stray `(`.
- **a token reaches forward for an expression.** an open-ended range (`a..`, `..`) takes whatever follows as its end. across a statement seam, across an *item* seam — `f() = ..=` above `node() = 1` is the range `..=node()`.
- **a token means something else in this position.** `..` at the head of a call argument is the *spread* (`Point(..p, x: 1)`), so `f(..)` is a spread missing its base, not a range. and the parser's `starts_expr` list — the one thing that decides whether a `..` has an end — omits `handle` and `without`, so a range end beginning with either has to be parenthesised.
- **and one real precedence bug**, which I'll give its own section because it's the one I'd have sworn wasn't there.

## the two numbers

- seed 124150. the printer emitted `(entry..=) / "queue" % 10.0?` as a block's result, and the leading `(` got it called by the statement above.
- the parens looked necessary: an open range binds at 9, a `/` at 13, so the range is a loose operand of a tight operator, so it needs wrapping. except the source that produced this AST wrote it bare, and bare re-parses correctly.
- the reason took me embarrassingly long. **in a left-associative precedence climb, the leftmost operand is parsed at the level the parser *entered the expression*, not at the operator's level.** the parser reads `entry`, *then* sees `..=`, *then* sees `/`. it never "enters at 13" — it climbs to 13 with a left operand it already had.
- so there are two different numbers and I was passing one for both. the operator's `left_bp` decides **whether the operand needs parentheses.** the incoming `min_bp` is **the context the operand's text will actually be read in.** thread the wrong one down and you parenthesise things that needed nothing, which — see above — is not a cosmetic mistake.
- I've written a few Pratt parsers. I don't think I'd ever had to state that distinction out loud, because when you're *parsing* the two collapse: you have the number, you use it, you move on. it only separates when you run the climb backwards.

## the repair is smaller than the problem

- some seams are genuinely unavoidable — the source that produced the AST had parentheses there too, and the printer has to put them back. so I added a seam pass: look at each adjacent pair, and if the left one would reach into the right one, wrap it.
- wrap *what*, though. two corrections, both from the sweep:
- seed 441991: I wrapped the whole statement, and the statement was `let label = ..=`. **`(let label = ..=)` is not a parenthesised binding, it's a syntax error** — a statement is not always an expression. so the repair became structural: wrap the *trailing expression*, the binding's value, where the offending token actually lives.
- seed 858752: still not enough. `1.5 ~> @` above a `token` statement is a real seam, but wrapping its trailing expression gives `(1.5 ~> @)` — and now the `["field"]` statement above *that* calls it. I'd traded a seam for a seam one line up.
- babble's own source had the answer in it the whole time: `1.5 ~> ( @ )`. **wrap the smallest offending node**, not the enclosing expression, and the statement's first token never moves. so `seal_tail` walks the right spine — the same spine the "does this reach forward" predicate walks — and parenthesises exactly the node that ends in the bad token.
- which is, I notice, what a person would write. the minimal repair isn't a clever optimisation; it's the only one that doesn't create a new problem, and that's *why* it's what the language's actual users produce.

## I was wrong twice, out loud

- worth recording, because it's the methodology point.
- at seed 2220 I reasoned that wrapping a statement couldn't cascade into the previous seam: "that would require the source to have needed the same parentheses, and then babble couldn't have generated it in the first place." sound argument. **correct, that time.**
- at seed 858752 I made the same argument about a slightly different shape and it was flatly wrong. the counterexample had been sitting in the generator's output space the entire time, at a rate of about one in 859,000.
- so: **when a generator can produce unlimited valid inputs and the property is cheap to check, run the sweep instead of arguing.** the seeds where these lived — 227, 466, 633, 2220, 3270, 19613, 22605, 44036, 124150, 245959, 441991, 858752 — are a nice illustration of why. the first few turn up in seconds. the last one needs forty-five minutes of machine time and would never have turned up in my head.
- the test takes the seed count from an environment variable for exactly this reason: 500 by default so it's a normal unit test, a million when you want to believe it. and a *clean* pass is the slow one, because nothing exits early.

## where it landed

- **1,000,000 generated programs, round-tripped, zero shape changes, 46 minutes.**
- the corpus pipeline doesn't take that on faith either. `cram-corpus` re-parses every printed program and compares it to the original; a program that fails keeps its flat rendering and gets counted. the count prints at generation time, even at zero, because "no line appeared" and "the check didn't run" look identical. the full corpus reports `0 programs kept the flat rendering`, which is a second confirmation through a different code path than the sweep.
- the Tier-0 corpus is now 1,000,000 programs / 79.7 MB / **20.25M BPE tokens**, with real layout and no padded operators. what that's worth to an actual model is the next post.
- I also gave the printer's *other* consumer a gate while I was in here. the hand-written `.st` files — the [canon stratum](../docs/generative-ladder.md), which is simultaneously userland, docs, fixtures and the best tokens in the corpus — now have a host-level test that every one of them parses and type-checks clean in milliseconds, plus a deliberately-broken control program proving the gate can *fail*. gradual typing means a checker that returned nothing would look identical from outside, and a green light you can't distinguish from a disconnected wire isn't a green light.
- two new canon modules came with it (`lib/text.st`, `lib/stats.st`, eleven behavioural tests), and writing them taught me two things about my own language that no test had ever surfaced: **`drop` is a `Seq` operation** (a `List` drops its head by index), and **`sum` is a keyword**, so it can't be a binding name. both are obvious in hindsight and neither had come up because I'd never written that particular line before.

## what I'm not pretending

- **comments don't survive.** the lexer skips them, so they aren't in the AST, so the printer can't emit them. this is a printer for generated code, not a formatter for hand-written files. a real formatter needs comments in the tree, which is a parser change, which is a different errand.
- **the cache can't see the printer clearly.** cached corpora carry a fingerprint of the pipeline, computed over three probe programs. it catches systematic changes fine. it emphatically did *not* catch a fix that touched one program in 246,000 — I regenerated, and the tool cheerfully said `reused`. no feasible sample size fixes that, so the rule is now written down: after a printer change, delete the manifest. the sweep is the guard; the digest is a convenience.
- **the layout is one opinion and I didn't measure it.** four-space indent, one item per line, blank lines between declarations but not between `use` imports. that's what real Stitch in this repo looks like, so it's what generated Stitch now looks like, and the circularity is not lost on me. if the model later shows me the layout was wrong, the way I'd find out is a metric I haven't built.
- **ten classes found does not mean ten classes exist.** a clean million-seed pass says the failure rate is below roughly one in a million *for programs babble can generate*. babble's grammar coverage is not the whole language, and the sweep is a lower bound on my ignorance, not an upper one.
- **the sweep only sees shape, not sense.** it proves the printed program is the same program. it says nothing about whether it's a program anyone would want, and it can't — that's the corpus's problem, and Tier-0's answer is "no." these are twenty million syntactically-real, semantically-vacuous tokens, capped at about a quarter of the eventual training mix by design.

- the thing I keep turning over: I went in thinking a printer is the parser run backwards, and it isn't. a parser only ever has to answer "what does this text mean." a printer has to answer "what will this text mean *when something else reads it*," and in a grammar where adjacency is application and nothing terminates a statement, that question reaches across every gap between every pair of lines. the parentheses in real Stitch aren't decoration I get to drop because the tree already knows the nesting. some of them are the only reason the line above still ends where it did.

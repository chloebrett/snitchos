# Post 43 — The payload was never the cost

> I run several agents in parallel, so my working tree is a pile of unrelated changes and "which files belong to this commit?" is a manual chore. So I built `snip`: I write the commit message, Sonnet picks the files, I stage and commit. Then I watched it burn ~70k tokens a call and spent an afternoon shrinking what I send the model — smaller diffs, tighter caps, a two-pass scheme that avoids sending most diffs at all. All of it real, all of it beside the point. One trivial measurement — asking `claude -p` to say the word "hi" — cost **57k tokens**. The diffs I'd been optimizing were ~20% of the bill. The rest was context I was shipping to the model for no reason, and the fix wasn't a cleverer prompt. It was `cd` to an empty directory.

## what I actually wanted

- the workflow that prompted this: N agents editing the same checkout at once. Four concurrent changes across `kernel/`, `stitch/`, `snip/`, docs — and to commit them cleanly I sit there reading diffs deciding which file belongs to which concern. Every time.
- that triage is the whole job. Writing the message is easy; sorting the pile is the tax.
- so hand the sorting to a model: **I write the message, Sonnet selects the files that match it, I approve.** The agent-in-the-loop never touches git — a deterministic `xtask` binary does the staging after I say yes. My "I own the commits" rule survives; no agent gets git access.

## the tool

- `cargo xtask snip "<message>"` → gathers the working tree, asks Sonnet, prints an include/exclude table with a one-line reason and a confidence per file, writes a plan. Mutates nothing.
- `snip --stage` → `git add` the planned files. `snip --commit` → commit the plan's message. Three steps, so there's a checkpoint to eyeball `git diff --cached` before it's sealed.
- the model returns structured output (a forced JSON shape), validated against the real candidate set — it can't invent a path, and a file it never mentions is surfaced as excluded-by-omission, not silently dropped.
- two things it earned in review:
  - **partial staging.** When a file mixes concerns (two agents both edited `main.rs`), the model returns just the hunk ids that belong, and `snip` reconstructs a patch and `git apply --cached`s it. Whole-file is the common case; hunks are the escape hatch.
  - **auto-stage on full confidence.** If the proposal is high-confidence *throughout* — overall high, every include high, every actively-decided exclude high — it stages without a second command. One med/low anywhere holds it back. (Observed live: overall "high" but one `[med]` exclude → correctly did *not* auto-stage.)
- it works. First live run sorted 9 files: pulled the `snip` ones, excluded the unrelated parallel work, flagged `Cargo.lock` as `[low]` — "touched by multiple changes, can't cleanly attribute." Exactly the call I was making by hand.

## then it started costing

- the timer line prints token usage. A propose was `~71k tokens: ~70k in / ~600 out`. Then `118k` on a bigger tree. That's a lot to answer "which of these files go together."
- obvious diagnosis: I send every file's full diff, every call. `lib.rs` alone grew hundreds of lines this session. Shrink the payload.
- so I built four levers, in order of ambition:
  1. **`-U1` context** — diff with one line of context instead of three. (Careful: hunk boundaries depend on context width, so the prompt diff and the stage-time reconstruction *must* use the same `-U`, or the model's hunk ids stop lining up.)
  2. **global + per-file line caps** — no single file, nor the whole set, can blow up the prompt; over-budget bodies get an elision marker. Every file still *named*, just not fully shown.
  3. **cache-split reporting** — surface the `cache_read` tokens separately in the timer, to see if anything was being cached.
  4. **lean-first, two-pass** — the ambitious one. Pass 1 sends only paths + change sizes (no diff bodies) and asks the model to bucket each file: settled-in, settled-out, or needs-a-closer-look. Only the "needs a closer look" files get a full-diff pass 2. When the changes live in disjoint files — the common case — the filenames alone sort them, and you never pay for the diffs.

- all four built test-first, all four green. And the two-pass run, on a real tree, came back at **118k** — *higher* than before.

## measure the thing you're optimizing

- 118k with two-pass, when the whole point of two-pass was to send less. Something was wrong with my model of where the tokens went.
- so, the cheapest possible experiment — send `claude -p` the prompt "reply with the single word: hi" and read the usage:
  ```
  input_tokens: 2
  cache_creation_input_tokens: 32364
  cache_read_input_tokens: 24846
  output_tokens: 4
  ```
- **57,212 input tokens to say "hi."** That's not my payload — my payload was two tokens. That's `claude -p`'s *baseline*: Claude Code's own system prompt, the full JSON schemas for every built-in tool, the project's `CLAUDE.md`, the skills, the MCP config. Paid on every call, before my diffs add a single token.
- this reframes all four levers. `-U1` and the caps shave the *delta* on top of a ~57k floor — marginal. And **two-pass is actively worse**: it makes two calls, so it pays the ~57k baseline *twice* to avoid sending diffs that were ~20% of one call. I'd built a lever that pulls the wrong way.
- same lesson as the flake in post 18, different domain: *I couldn't optimize the number until I measured what made it up, and measuring showed I'd been optimizing the wrong term.* "The diffs are too big" was the premise. The premise was wrong.

## the free win was a directory

- if ~57k is fixed context, the question becomes: how much of it do I actually need? For `snip`, none — the whole task is in the prompt, and tools are disabled. It doesn't need `CLAUDE.md`, it doesn't need skills, it doesn't need MCP.
- `claude -p` loads project context from its working directory. So run it from a directory with nothing to load. `run_claude` now spawns from an empty scratch dir.
- measured, trivial call again, from a neutral cwd: **32,079 tokens.** Down from 57k. **~25k of the baseline was the project's own `CLAUDE.md` + `.claude` + `.mcp.json`** — pure dead weight for this task.
- on the real tree: **~71k → ~46k a call.** A `cd`.
- then the natural next question (credit: the human asked it) — is some of the residual 32k *user-global* cruft, a `~/.claude/CLAUDE.md`? Sized it:

  | layer | tokens | reducible? |
  |---|---|---|
  | project `CLAUDE.md` + `.claude` + `.mcp.json` | ~25k | ✅ stripped by neutral cwd |
  | user-global `CLAUDE.md` | 0 | — (doesn't exist) |
  | user skills / agents | ~0 | — (none) |
  | Claude Code core: system prompt + built-in tool schemas | ~32k | ❌ not from the CLI |

- there was nothing global to strip. The residual ~32k is Claude Code's built-in core — its system prompt plus the full schemas for Bash/Read/Edit/Agent/… `--allowedTools ""` stops those tools being *used* but not their schemas being *sent*. No CLI knob trims it. The only way under ~32k is the raw Messages API — no CC wrapper at all — which needs separate API billing my Max plan doesn't cover. Out of scope by decision.

## two-pass was double the bill — so I flipped the default

- with the baseline understood, lean-first is a net loss: it trades one ~57k call for two, to save a payload delta smaller than a baseline. It only wins if your diff payload genuinely *exceeds* ~32k — a huge tree.
- so single-pass is the default now; two-pass is opt-in behind `--lean`, documented as rarely worth it. It loses nothing on accuracy — partial hunk staging works either way; two-pass only ever "saved" by not sending diffs, and that saving was the small term.
- the one thing that *does* soften the baseline: it's cached for ~an hour (`ephemeral_1h`). Repeated `snip` calls within the hour serve it as ~10×-cheaper cache reads. So the practical advice is dumb but real: **batch your staging.** The first call pays; the rest mostly read cache.
- and the quietest lever turned out to be the MVP: the **cache-split in the timer line**. `45284 in (23201 cached)` is the single number that made all of this visible. Without it I'd still think the diffs were the cost.

## what I kept, and the meta-lesson

- kept `-U1` and the caps — free, and they bound the worst case even if they don't move the headline.
- kept, and promoted, the cache-split reporting.
- flipped two-pass to opt-in.
- and the discipline beat, again, the same one posts 16–18 keep landing on: **when a targeted optimization comes back inconclusive or backwards, question the premise, not the design.** My premise was "the diffs are the cost." One trivial measurement — the cheapest experiment available — falsified it in one line of output. I'd built four things on an unmeasured assumption. The measurement should have been the first move, not the fifth.

## what shipped

- **`snip`** — a new crate (pure prompt-build + reply-parse + hunk parsing + patch reconstruction, all host-tested) plus `xtask/src/snip.rs` (all the git I/O). 45 tests, clippy-clean.
- **the flow** — `snip "<msg>"` → `--stage` → `--commit`; the agent never runs git.
- **partial hunk staging** — model returns hunk ids for mixed files; reconstructed and `git apply --cached`ed. Proven end-to-end in a temp repo: two-hunk change, stage only H2, assert the index holds H2 and H1 stays unstaged.
- **auto-stage on full confidence**, with `--no-auto` and the omission-vs-decision distinction so excluded-by-omission files don't block it.
- **the token work** — neutral cwd (~25k/call saved), `-U1`, global+per-file caps, cache-split reporting, and two-pass demoted to `--lean`.
- writeup and the full measurement table in `plans/snip-stage-picker.md`.

## what's next

- three ideas parked in the design doc, in rough order of appeal:
  - **whole-tree partition** — invert the flow: instead of "here's a message, pick its files," ask the model to split the *entire* messy tree into a disjoint set of proposed commits, each with a generated message, presented as a checklist to tick. The natural end state for the parallel-agent workflow — one command turns chaos into a reviewed sequence.
  - **edit-provenance ledger** — a `PostToolUse` hook logs every agent edit (timestamp, agent, lines) to a local file. A file touched by a single agent is "pure" and commits deterministically with *no model call at all*; only the genuinely-mixed files fall back to Sonnet. Ground-truth-accurate for the pure majority, and it makes the model call the exception.
  - **reverse direction** — given an already-staged set, suggest the commit message. Closes the loop both ways.

---

*Footnote for anyone bolting a model onto a tool and watching the meter spin: measure a no-op call before you optimize the payload. Mine cost 57k tokens to say "hi," which meant the thing I was shrinking was a fifth of the bill and the other four-fifths was context I was shipping for no reason. The biggest saving in the whole exercise wasn't a prompt technique or a diff cap — it was noticing I was running the model from a directory full of stuff it didn't need, and moving it somewhere empty. The clever levers netted the least; the dumb observation netted the most. Measure first, or you'll optimize the term you can see instead of the one that's charging you.*

*[TBD: a screenshot of the timer line before/after — `71398 tokens` next to `46414 tokens (23201 cached)`]*

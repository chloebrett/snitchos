# Post 86 — the first tour chapter, verified against a live boot

- the plan for this arc is a documentation system whose whole purpose is that a page cannot go on saying something the machine has stopped doing. a chapter declares a world-state and some claims; the gate boots the kernel, replays to that state, and fails the build if a claim is no longer true.
- I built most of it. and while building it I wrote, at least four times, a claim that said more than anything could check.
- that is the post. the rest is what shipped, and three places where measuring beat reasoning by an embarrassing margin.

## what exists now

the `tour` crate, and one chapter.

- **`tour`** owns the chapter contract: the manifest schema, the anchor predicate, a bounded anchor search, and the claim verifier. pure logic, no snemu, no I/O beyond reading its own chapters — 21 host tests, and `cargo xtask mutants tour` at 29 mutants, 26 caught, 3 unviable, 0 survivors.
- **the chapter** is `tour/chapters/capabilities.{toml,mdx}` — `init` creating an IPC endpoint and delegating it twice: `RECV | MINT` to the file server, a bare `SEND` to the client. two processes, one object, two different powers over it.
- **the drift check** runs inside `cargo xtask itest`. it boots `init` under snemu, stops at the anchor, and verifies three claims:

```
132/132 scenarios pass under snemu (100% fidelity, 7.7s)

=== tour ===
  PASS  capabilities — 3 claim(s) hold at its anchor (5241306 steps, 840 frames)
```

- and it fails properly. falsifying a claim by hand gives:

```
the tour no longer describes the machine (after 57601165 steps, 1367 frames):
  chapter "capabilities" claims "the file server is handed the `fs` endpoint
  with the right to receive and to mint" — but it did not happen
```

the sentence in the failure is the sentence in the prose.

- also: a stamped input log in `snemu-wasm` (`(instret, text)` — a session as a replay script), and the SPA routing shell with back/forward, scroll restoration and focus management.
- **the tour is not viewable.** `/tour/capabilities` renders a heading and a link. the prose and the embed are next. this is a notebook entry, not a launch.

## four claims that outran their checks

each one would have been caught by the mechanism I was in the middle of building.

- **"the stamp is read before delivery — that ordering is the whole contract."** written into `input.rs` as a doc comment. it is not a contract: `push_console_input` buffers into the UART and retires nothing, so the guest clock reads identically either side of it, and **no test in that file could tell the two orderings apart**. mutation testing cannot reorder statements, so nothing would ever have found the claim empty. it is a convention, and now says so.
- **"the client is never handed the right to receive."** a chapter claim, and the most interesting thing the chapter had to say. rights match on *equality*, so the predicate only ruled out the exact mask `SEND|RECV` — not any mask containing `RECV`. I weakened the prose to "never handed to anyone with send and receive together" rather than widen the predicate, and left a note in the manifest saying why. the whole apparatus exists to stop prose overrunning proof; the fix has to be the prose.
- **a doc comment citing a guard test that did not exist yet.** I wrote that `NAMED_RIGHTS` was kept honest by an `every_right_in_the_abi_can_be_named` test, then wrote the test. it failed on its **first run**: `KILL` was missing from my hand-written table of six. re-reading the ABI found `AUDIO` missing too. writing the guarantee before the guard nearly shipped two rights a chapter could not name.
- **"host-tested and linked into `snemu-wasm`."** in the crate docs *and* in the plan's status header. it is not linked into `snemu-wasm`; that is a later step. two files asserting a wiring that does not exist.

the arc is getting hard to miss. [post 80](post-80-checkpoint-vocab-pairing.md) was a guard that passed while checking nothing. [post 82](post-82-snitchos-in-a-browser-tab.md) was a diagnosis that arrived attached to a correct observation and was never tested. this is the next one along: **an assertion that outruns its evidence**, written by someone building a machine against exactly that.

- the punchline arrived unprompted. a `DISPLAY` right landed on the ABI mid-session from parallel work, and the guard caught it the same day — the third right it has caught, and the second time it has been the reason a table was wrong.

## three times measuring beat reasoning

- **the drift check was in the wrong phase, and my own plan said so.** I had written that it belonged in `cargo xtask test` beside the generated-diagram drift check. running it showed why not: it forced a riscv kernel build into the host-check phase — **three minutes** on a cold tree — before `itest` had built anything. the repo's own shape had already said this, and I had read it: the telemetry diagram targets are deliberately *not* `--check`-gated, for precisely this reason.
- **then it built the kernel a second time.** moved into `itest`, it still took 6m30s, because `prepare()` defaults to `OptLevel::Low` while the itest run had just built `release`. a whole separate kernel, for a guest that then behaves identically. passing itest's own opt level through dropped it to `0.13s`.
- **and it ran seven times further than it needed to.** the first version reused `collect_frames_until_cap_quiescence` — the call `diagram caps` makes — which runs to ~57M steps. but a chapter's claims are read over the frames *up to its anchor*; everything after is waste. a stop predicate plus the release kernel's tighter codegen took the anchor from 37.5M steps to **5.24M**.
- none of those three were visible from reading. all three were visible in the first real run.

- the same shape, smaller, in the browser: I patched a failing scroll-restoration test twice by guessing — set `history.scrollRestoration = "manual"`, then defer the scroll a frame. both are correct and neither was the bug. one diagnostic, dumping `history.state` after the navigation, settled it in a single run: **Playwright scrolls an element into view before clicking it.** the link is at the top of the page, so clicking it scrolled back to zero, and `navigate()` faithfully recorded `scrollY: 0`. the bug was in the test.

## what the tests caught that I would not have

- **a wrapper `<div>` broke the emulator.** the shell needed somewhere to look for the new heading, so I wrapped the route switch in a div. that broke the height chain from `#root`, xterm's fit addon then resized forever, and three interactive browser tests timed out on `element is not stable`. no unit test could see it. `display: contents` — a wrapper that is not a box — fixed it.
- **`useSyncExternalStore` needs a referentially stable snapshot.** resolving the route afresh on every call reports a change on every render, and React gives up with "maximum update depth exceeded". caching by path is not an optimisation there; it is what makes the hook terminate.
- **two mutation survivors, both silent inversions.** `present`'s default flipping from `true` to `false` — every unqualified claim would have quietly asserted *the opposite*, and passed against a machine where the thing genuinely does not happen. and `|` → `^` in rights parsing, which is *identical* for distinct bits and only diverges when a name repeats, at which point it cancels the right and yields an empty mask. every existing test passed under either operator.
- **a test that could not discriminate.** my first back/forward browser test used two `page.goto`s — real page loads. it would have passed with no client-side router at all, because the browser would have been doing the entire job. rewritten to navigate from inside the app.

## the decisions worth keeping

recorded properly in [plans/tour-v1.md](../plans/tour-v1.md), because the reasoning is the expensive part.

- **replay, not snapshots.** the design doc's load-bearing idea was content-addressed snapshot blobs. that machinery does not exist — snemu's snapshot primitive is `#[derive(Clone)]` on `Machine` — and guest RAM in the browser is 128 MiB, so a naive blob is 128 MiB per chapter. determinism makes it unnecessary: declare the initial conditions and execute to the anchor. byte-identical, and it costs boot time instead of page weight.
- **anchor by predicate, not by instret.** a count is invalidated by every kernel rebuild; a predicate re-finds itself. this also answered the compatibility question I had been about to solve with a build fingerprint — compatibility is *validated by re-evaluating the predicate*, and a kernel change that stops it firing failing the gate is the intended behaviour, not a problem to route around.
- **an SPA, not a docs framework.** I spent a while on Astro + Starlight and it is a good fit for a documentation site. it is not a fit for this: the panels are global overlays openable on any page, which means the emulator must stay alive across navigation, and a statically-rendered multi-page site discards the wasm instance on every one. Astro's `transition:persist` could carry it, but that feature is built for counters, not for a multi-MB VM driving an animation-frame pump. with client-side routing the problem is *absent* rather than solved. the honest framing came from the other side of the conversation: it is an app shell with documents inside it, not a document site with widgets on some pages.
- **the tracer needs no navigation.** one chapter means no sidebar, no search, no table of contents, no prev/next. all of it deferred.

## what I'd tell myself

- **a claim is a thing that can be checked, or it is decoration.** "this is the contract" is worth writing only if something fails when it stops being true. four times in one arc I wrote the sentence and not the check.
- **write the guard before the sentence that cites it.** the one time I did it in the other order, the guard found two real gaps the moment it existed. the doc comment had been confidently describing a test I had not written.
- **when the prose and the predicate disagree, weaken the prose.** widening the predicate is the tempting fix and it is how a check quietly grows to mean something nobody decided.
- **a placement decision is a measurement, not an argument.** I reasoned my way to the wrong phase, then measured three separate costs in the first real run, each invisible from reading.
- **two guesses is one guess too many.** the diagnostic that settled the scroll bug took one run and told me the answer was in the test harness, not the code. I should have reached for it after the first patch failed.
- **the tests that found the most were the ones running in the real thing.** the layout break, the router loop, the second kernel build, the 7× overrun — every one of them was invisible to reading and to the fast suite.

---

- the plan, with its four decisions and what is left: [plans/tour-v1.md](../plans/tour-v1.md). the design it implements stage 3 of: [docs/tour-and-user-docs-design.md](../docs/tour-and-user-docs-design.md).
- next: the embed. that is where `tour` finally gets compiled into `snemu-wasm` — making true the sentence I have now twice written prematurely — and where the chapter grows its prose and its guest.

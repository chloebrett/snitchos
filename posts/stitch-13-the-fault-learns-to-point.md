# Stitch 13 — the fault learns to point

- SnitchOS has one idea it repeats everywhere: the system should narrate what it does. spans on the wire, metrics, cap events — you watch the machine work instead of guessing. and for a long time there was exactly one thing on this platform that refused to narrate: the error path. when a Stitch program divided by zero it said `division by zero` and nothing else. not where. not in what. a bare string, floating free of the program that produced it.
- this post is the error path learning to talk. by the end a fault says `<program>:2:9: division by zero`, draws a caret under the offending expression, and prints the call stack that led there. the last silent thing on the system joined the narration.
- and there's a small pattern at the center of it that I got more satisfaction from than the feature itself — the same three-line trick, used three times, to answer three different questions about a fault: *where* is it, *which file*, and *how did we get here.* I'll get to it.

## the setup: spans that go nowhere

- posts 9 through 11 were the groundwork and they didn't look like much on their own. post 9 was the editor sending me back to the front end demanding real positions and a real evaluator. post 10 and 11 built the evaluator — fuel, a depth guard, a trampoline. and somewhere in there the parser learned to stamp a byte-range `Span` on every token and every AST node.
- so by the time this post starts, every expression in a Stitch program knows where it came from. `1 / 0` knows it's bytes 9 through 14 of its source. the information was *there*. it just died at the parser — nothing downstream ever looked at it. a runtime fault was a `RuntimeError::Fault(String)`, a plain message, and the span three layers up in the AST had no way to reach it.
- the whole job was connecting those two ends: the span the parser knew, and the fault the interpreter raised.

## innermost wins

- here's the pattern, because it's the load-bearing idea. a fault is born deep — inside `eval_binary`, which does the actual division and has no idea what expression it's evaluating. it just knows the numerator and denominator. so the span can't be attached where the fault is *created*; it has to be attached as the fault *travels outward*, at the first point that knows both the fault and the expression.
- that point is `eval`. every expression goes through it, and `eval(expr)` knows `expr.span`. so I wrapped it:

```rust
eval_dispatch(expr, env).map_err(|error| error.stamped(expr.span))
```

- `stamped` sets the span **only if it isn't already set:**

```rust
fn stamped(self, span: Span) -> Self {
    match self {
        Fault { message, at: None, .. } => Fault { at: Some(span), .. },
        other => other,   // already located — leave it
    }
}
```

- that `at: None` guard is the whole trick. the fault surfaces from the *innermost* `eval` first — the one evaluating `1 / 0` itself — and that one stamps its span. every enclosing `eval` on the way up sees `at` is already `Some` and passes the fault through untouched. so a fault in `4 + (1 / 0)` cites the inner `1 / 0`, not the outer `+`. the tightest expression that could possibly be blamed is the one that gets blamed, and it falls out of "innermost runs first, first writer wins" without any explicit depth tracking.
- I want to flag this because I reused it verbatim two more times. the fault ended up carrying three things — a span, a source, and a backtrace — and all three are stamped the same way: no-op-once-set, on the way out, innermost wins. one idea, three questions.

## which file? the part I got wrong first

- a span is a byte range. `bytes 9..14` of *what?* here's the thing I didn't appreciate until it bit me: Stitch parses many sources independently — the prelude, the user program, each REPL line, each imported module — and every one of them starts its byte count at zero. `9..14` is a real location in a dozen different texts at once. a span alone is meaningless.
- so faults needed a second field: a `SourceId` saying *which* text the span indexes. and there's a `SourceMap` — a little registry of `(name, text)` — that turns a `(SourceId, span)` pair into `hello.st:2:9` plus the offending line plus a caret.
- my first instinct was to put the `SourceId` inside `Span` itself, rustc-style — then a span is self-describing and the id flows everywhere for free. I talked myself out of it, and I'm glad I did. putting it on `Span` grows every AST node by a word, forces the lexer to know about the source registry, and — the one that decided it — re-tangles span *equality*, which I'd deliberately made ignore positions two posts ago so structural tests wouldn't churn. instead the source rides on the `Env`: the running code knows which source it came from, and the fault gets stamped with `env.source()` on the way out — the same innermost-wins map, second verse.
- the subtle bug I almost shipped: for a while I tagged the whole built environment — prelude *and* user code — with the user program's source. it typechecks, it runs, and it renders **garbage**, because a prelude function's spans point into the prelude text but I'd render them against the user's program. a fault deep in `fold` would draw a caret under some random line of your file. so registration got split: the prelude registers as `<prelude>`, your program as `<program>`, each function tagged with the source its body was actually parsed from. now a fault in library code cites the library and a fault in your code cites your code, because their spans genuinely index different strings and the ids finally say so.

## how did we get here

- a location tells you where a fault is. it doesn't tell you how control reached it. for that you want the call stack, and the interpreter was throwing it away.
- it had a *depth counter* — a single `u32` that `apply_values` bumped on every call and decremented on return, purely as a recursion backstop so infinite recursion faults instead of overflowing the Rust stack. it knew *how deep* you were. it had discarded *what you were in.*
- so I swapped the counter for a stack of frames. `enter_call` pushes a `Frame { name }`, the guard pops it on the way out, and `frames.len()` is the depth guard for free — same backstop, more memory. each function now carries its own name (set when it's registered; a lambda's is `None`), so the frame knows what to call itself. and at the raise point — innermost-wins again, third verse — the fault snapshots the live stack.
- the payoff is a real backtrace:

```
runtime error: <program>:3:11: division by zero
inner() = 1 / 0
          ^
  in inner
  in outer
  in main
```

- the bit I'm quietly proud of is the tail-call case, because it took *zero* extra code to get right. post 11's trampoline turns self-tail-calls into a loop instead of nested Rust frames — that's the whole reason a Stitch loop doesn't blow the stack. and because the loop pops the call guard at the end of each iteration and pushes a fresh one at the start of the next, a function that tail-recurses a thousand times only ever has *one* frame on the stack at a time. so a tail-recursive function that faults shows a single frame, not a thousand — which is exactly what you want, and it's just what the stack already does. the trampoline was telling the truth about the machine's real depth all along; I just started reading it.

## what I'm not pretending

- the honest gaps, in order of how much they'd annoy you:
- **the backtrace names the chain but not where each call happened.** you get `in outer`, not `in outer  <program>:2:11`. the innermost frame's location is the fault line, which is the one you usually want — but the intermediate call sites aren't shown. wiring them in means threading the call-site span through `apply_values`, and about ten of its call sites are stdlib combinators invoking closures with no meaningful source location, so it's threading `None` through noise. worth doing when a real debugging session demands it; not before.
- **a REPL fault inside a `:load`ed function renders message-only.** the line you type gets a location; a function you loaded doesn't, because `:load` parses the file and throws the text away. keeping the source around is a small change I haven't made.
- **multi-module faults render message-only too** — each module would need to register its own source, same shape as the prelude/program split, just more of it.
- and the one that's most on-thesis to admit: **the fault narrates to the console, not to the wire.** everything else on this system that narrates does it as a telemetry frame you can watch in Grafana. a fault, for now, renders to stderr and the REPL. a fault that emitted its own spanned backtrace *as a telemetry event* — so a crash shows up on the same dashboard as everything else — is the obviously-right ending and I haven't built it. the data's all there on the `RuntimeError`; it just isn't on the wire yet.

- still. the thing I set out to fix is fixed. an error on this platform used to be the one event that happened in the dark — a string with no address, no history, no thread back to the code. now it points at the expression, names the file and line, draws the caret, and prints the path that got there. the same idea that stamps the span stamps the source and snapshots the stack: write once, innermost wins, on the way out. the error path finally does the thing the whole system was built to do. it tells you what happened.

---

## addendum — effects learn to bend

- the diagnostics were the payoff of the spans. the *next* thing the spans unlock is effects, and I got the first two pieces of it in while the plumbing was warm. it's mid-flight — there's no surface syntax yet — but the machine underneath is built and I want it on the record.
- the thesis for this one: post 8 was "the language learns to say no" — `uses Telemetry` is a gate, and a function that doesn't declare it can't `emit`. that's a *boolean*. you either may perform an effect or you may not. what's missing is the ability to *redirect* an allowed effect — to say "inside this block, `emit` goes somewhere else." that's a handler, and it's the membrane stim's modes will eventually be built from: a mode is just a handler that intercepts keypresses differently. so, effects with handlers, single-shot, no fancy resumable continuations — the tree-walkable subset.

- **first, `uses` grew a span** (this is where the spans pay off again). it was a bare `Vec<String>` of capability names; now it's `Vec<Effect>`, and each `Effect` carries the span of where the capability was declared. same trick as the AST nodes — equality and debug ignore the span, so it's metadata and nothing structural churned. the point is that a future refusal can cite *two* places: where you performed the effect (the diagnostics above) and where — or whether — you declared it. the "you didn't say `uses Telemetry`" error will point at the function signature, not just the `emit` call. the declaration is now a located thing.

- **then the handler stack.** the `Env` gained a dynamically-scoped stack of `(op-name → handler value)`. an effect native — `emit`, `print`, `readByte`, all eight of them — used to go straight to the platform. now it goes through one function first:

```rust
fn perform_effect(op, args, env, ambient) -> Result<Value> {
    match env.handler_for(op) {
        Some(handler) => apply_values(&handler, args, &env.without_top_handler(op)),
        None          => ambient(),   // the platform, as before
    }
}
```

- with no handler installed that's just `ambient()` — so the whole change is invisible, every existing test passes untouched, the platform still gets every effect. install a handler for `emit` and suddenly `emit("x", 1)` calls *your* function with `("x", 1)` instead.
- the detail I like is `without_top_handler`. when the handler runs, its own op is popped off the stack for the duration — so a handler that itself calls `emit` (to log-then-forward, say) doesn't re-enter itself into an infinite loop; the inner `emit` sails past to the *next* handler down, or to the ambient platform. that's what makes a handler able to wrap an effect rather than only replace it. the test I pinned it with installs a handler that re-emits under a different name and checks that the renamed metric reaches the real sink — proving both that the handler intercepted *and* that its forward escaped the membrane.
- and it's dynamically scoped, which is the whole point of a membrane: a function called three layers deep inside a handled block sees the handler, even though it was defined somewhere that never heard of it. that falls out because the handler stack rides through calls on the `Env` the same way the source does — threaded, not reset at the boundary. authority gets *replaced* at each call (least privilege); handlers *flow through* (dynamic extent). same struct, opposite discipline, and that difference is exactly the difference between "what you're allowed to do" and "who's listening while you do it."

- what's not here yet: you can't *write* `handle emit with f { … }` — there's no keyword, the installation only exists through the Rust `Env` API. the surface syntax is the next step, and after it, attenuation — a block that *drops* a capability for its extent, so an effect inside it faults. and when that fault fires it'll be spanned, and cite the `uses` declaration, because both of those are now things a fault can point at. which is the nice part: the error work and the effect work aren't two projects. the effects will break, and when they do, the faults already know how to tell you where.

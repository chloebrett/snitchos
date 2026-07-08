# Stitch 11 — the trampoline

- last post ended on a promise: the depth guard is a backstop, not a solution, and the honest fix for tail recursion is a trampoline. this post is that fix. if you missed the setup — a tail call is a function call that is the *last thing* the current call does, nothing pending on top of it. `count(n) = n == 0 => 0 | count(n - 1)` is tail-recursive: when we reach the recursive case, the call to `count` is the whole answer; there's no `+ 1` to apply afterward, no value to come back and use. a call like that doesn't actually need its own stack frame. you can throw the current one away, put the new arguments in, and jump to the top. the name for this is a trampoline: instead of descending, you bounce.

- the last post landed depth-48 and said it was embarrassing. the trampoline is why that number gets to climb back up.

## the detection problem

- trampolines are easy to describe and annoying to implement in a tree-walker. the textbook version works on bytecode: the compiler emits a `TAIL_CALL` instruction at the right spot, the runtime sees it, loops instead of calls. a tree-walker doesn't have that luxury — there is no compilation pass, just `eval` recursing over an AST. so the question is: when `eval` is about to call a function, how does it know whether that call is in tail position?

- naive answer: intercept every self-call. if you're about to call the same function you're already inside, signal "tail call" instead of recursing. the problem is this is wrong for non-tail self-calls. `f(n) = f(n) + 1` has a self-call that is *not* in tail position — the `+ 1` still has to happen after it. if you trampoline it, you skip the `+ 1` and get a wrong answer. the depth guard was there for a reason; `f(n) = f(n) + 1` should still hit it.

- so you need to actually track tail position. the clean way would be a flag threaded through every `eval` call — `eval(expr, env, in_tail: bool)` — but that touches every call site in a 3000-line file and produces a lot of churn for the semantics. instead I split off a new function: `eval_tail`, which is what `apply_values` calls when it's about to run a closure body. `eval_tail` knows it's in tail position and handles only the nodes that *propagate* tail position: a conditional's branches are in tail position, a block's result expression is in tail position, a match arm's body is in tail position. for everything else — binary ops, field access, string interpolation, everything where the value is used rather than returned — `eval_tail` falls through to plain `eval`. and when `eval_tail` sees a function call whose callee is the function we're currently inside (checked by pointer identity on the `Rc<ClosureData>`), it signals a `TailCall` instead of recursing.

- this keeps the non-tail case clean. `f(n) = f(n) + 1`: the body is `Expr::Binary`, which `eval_tail` doesn't handle, so it falls through to `eval`. `eval` evaluates the binary op normally, evaluates both sides, tries to add them. the left side recurses into `apply_values` again. depth increments. the depth guard fires at 48. the test passes exactly as before.

## the signal

- `TailCall` is a value in the `RuntimeError` enum — not an error you'd ever show a user, but the same propagation mechanism as `Return`, which is already there for the `?` operator. when `eval_tail` identifies a self-tail-call, it collects the new argument values and returns `Err(RuntimeError::TailCall(new_args))`. this unwinds back to `apply_values`, which catches it and loops.

- `apply_values` for a closure is now a loop. each iteration: bind the current args in a fresh environment, mark the closure as "the thing we're inside" (so `eval_tail` can detect it), grab the depth guard, evaluate the body. if the body exits via `TailCall`: drop the guard (depth back down), update the args, go again. if it exits normally: return the value. the depth guard is acquired at the start of each iteration and released at the end — so for tail recursion, depth oscillates between n and n+1 rather than climbing. a million-step tail-recursive countdown runs in bounded stack.

## the test that proves it

- the acceptance criterion was: a self-tail-recursive function with an argument of 1,000,000 returns without faulting. without the trampoline, this faults at depth 48. the test:

```
count(n) = n == 0 => 0 | count(n - 1)  main() = count(1000000)
```

- the result is `0`. that was the whole test, and it took 1.25 seconds to run — which is entirely the million evaluations, not stack overhead.

- I also needed tests for the two other tail-position contexts: block bodies (where the call is the result of a `{ }` block) and match arm bodies. a block-body tail call and a match-arm tail call both get separate tests at the same depth. and I changed the mutual recursion test — it used `isEven(4)` before, which is `true` by accident either way. I changed it to `isOdd(3) = true`. with the `is_self_closure → always true` mutant, `isOdd(3)` gets trapped as a self-call to `isOdd`, descends to `n = 0`, returns `false` from `isOdd`'s base case. wrong answer, caught mutant.

## what it doesn't fix

- mutual tail recursion doesn't get the stack savings. `isEven` and `isOdd` calling each other don't share a trampoline — each call to the *other* function goes through `apply_values` normally. this is fine: the mutual recursion test uses small numbers, and mutual tail recursion requires more machinery (a shared trampoline, a way to detect "this call exits back to the same trampoline frame"). it's not a gap I'm filling here.

- the depth limit is still 48 for non-tail calls. that hasn't changed. the honest fix for that is smaller `eval` stack frames — a Phase C job, when the big `eval` match gets split across a lowering pass and a smaller core evaluator. 48 can climb once frames shrink.

## what's next

- Phase C: the first genuinely structural change to the interpreter. the parser currently does some desugaring on the way in — turning the `cond => then | else` syntax into an `If` node, for example — and the evaluator does more on the way through. the Phase C goal is to separate these: a faithful surface AST that round-trips, a single explicit lowering pass that produces a smaller core IR, and an evaluator that only sees the core IR. node-level spans and symbol interning land here too, in the same churn as the AST reshape — the "don't touch the AST twice" rule from the design review. the bytecode VM, when it comes, compiles this same core IR. the tree-walker just evaluates it instead.

- there's also B5 still outstanding: measure whether the tree-walker actually leaks across repeated evaluations (reference cycles that refcounting can't collect) or just churns. stim is the first genuinely long-running Stitch program, so it'll feel any leak. the measurement is the decision: stay on the tree-walker or wait for the VM and its collector. I know which way I'm betting, but I'd rather have data than a bet.

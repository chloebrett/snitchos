# Post 67 — where the xtask build time actually went

- this started where post 58 left a footnote. that post killed two kinds of build overhead and then admitted, at the very end, that its own "confirm it's green" run took **five minutes** — blamed, in passing, on interleaving `cargo` and `cargo xtask` calls thrashing the cache. i came back to that five minutes because it kept happening, and it turned out the footnote had named a symptom, not the cause. the real one was a build script **watching a directory it writes into** — declaring its own output as an input, and so invalidating itself forever. everything else this session — a crate split that measured to nothing, a pile of contaminated benchmarks, two smaller leaks — orbits that one bug.

## the split that measured to nothing

- the presenting complaint was ordinary: `x itest` spends a long, silent beat on `Compiling xtask`, and it *feels* like the tool is the tax. so i measured it, because the first move is always to refuse the wall clock and read what's underneath. `cargo build --timings` plus `-Ztime-passes` said the `xtask` binary is one **~15k-line terminal crate**, its compile dominated by the **frontend** — type-checking, trait solving, and monomorphising the generics that `clap`'s derive and the dependency graph pour into it. the **linker was 0.18 seconds of eight.** whatever this was, it wasn't the linker, and a faster one (the reflexive first suggestion) would have bought nothing. i wrote that down before touching anything, so the fix couldn't drift toward the easy lever.

- the plausible structural fix was to stop compiling 15k lines as one unit. so i split the tool: `xtask-qemu`, `xtask-snemu`, `xtask-cmds` carved out of the binary, each a sibling crate the thin `xtask` links. clean seams, no cycles, the compiler happy. and then i measured the thing i'd actually set out to speed up — editing a scenario and rebuilding — and it moved by **nothing.** ~8 seconds before, ~8 seconds after. the ~5k lines i'd extracted were *compile-cheap*; the cost lived in the clap-derive expansion and the dependency monomorphisation that stay in the binary no matter how you carve up your own modules. the split is a fine seam, but as a speed fix it was a **plausible story the measurement refused.** i kept it — it's real factoring — but i had to say out loud that it didn't do the job i'd sold it for.

## the real bill: a build script that watched its own output

- the split was never the problem, because the problem wasn't the *tool* rebuilding. it was the **kernel** rebuilding — every single time — with nothing changed. the repro is embarrassingly small: `x build`, then `x itest`, then `x build` again, around a cold codebase, and each one recompiles the kernel from scratch. my first theory was the honest-looking one: `x build` builds the kernel with no features, `x itest` with `--features itest-workloads`, and cargo keeps one artifact slot per crate, so the feature switch evicts. tidy. wrong.

- the fingerprint log said so. `CARGO_LOG=cargo::core::compiler::fingerprint=info` prints *why* each unit is judged dirty, and it didn't say "features." it said the kernel's **build script** was stale because a file's mtime had moved:

  ```
  dirty: ChangedFile { reference: ".fingerprint/kernel-…/output",
                       stale: "…/fs-image",
                       stale_mtime: (newer than the build-script output) }
  ```

- `fs-image/` is the seed tree baked into the userspace filesystem — hand-authored `.st` data *and* a `bin/` full of compiled userspace binaries. `kernel/build.rs` **copies those binaries into `fs-image/bin/`** as part of the build, and then, a few lines later, declares `cargo:rerun-if-changed=fs-image` — the whole directory, `bin/` included. so the build writes into a directory it watches. every build advances `fs-image/bin/`'s mtime; the *next* build under any other profile or feature-set sees its own build-script fingerprint as older than that mtime, re-runs, rewrites the binaries, and advances the mtime again. a **perpetual-motion invalidation**, and it spreads across every configuration that shares the tree: `x build`↔`x itest`, `--opt=low`↔`--opt=mid`, and it would ping-pong with rust-analyzer too. the same-command-twice case stays fast only because a config that rebuilds *owns* the latest mtime; the moment two configs alternate, they take turns staling each other.

- the fix is the asymmetry i'd missed. `fs-image/bin/` is the kernel build's **output**, but it's the fs-server's **input** — the seed baker in `user/fs/build.rs` legitimately watches those bytes, because for it they *are* source. so the correct move is per-consumer: `kernel/build.rs` watches the seed **data** recursively but **prunes `bin/`**, because the binaries' real sources — the userspace crates — are already covered by the metadata-derived dependency walk from [[project_build_watch_derived_from_metadata]]. watching the outputs was redundant *and* self-defeating. after the change, `x build → x itest → x build` goes from a full kernel rebuild each way to **0.08 seconds.** the loop that felt like it was punishing me for switching commands just… stops.

- the shape of this bug is worth keeping past the specifics: **a build script that lists its own output under `rerun-if-changed` never converges.** it's the filesystem version of a feedback loop, and mtime is the wire that closes it. any generator that writes into a watched tree has this latent, and it hides perfectly because a single repeated command looks fine.

## the measurements were lying too

- the honest part of this session is that i spent an hour chasing a thrash that wasn't there, because i benchmarked on a machine i was also hammering. i became convinced `x test`'s snemu recompile was a **dev/test profile eviction** — built dev snemu, ran the test build, watched it recompile, "confirmed" it twice. then i stopped touching things and ran the clean alternation, and dev and test snemu **coexisted, both cached, no eviction.** the earlier "confirmations" were my own `touch`es and a Cargo.toml edit invalidating everything underneath the experiment. and the raw numbers were nonsense in the same way: the identical snemu compile measured **12 seconds** clean and **55–70 seconds** while my own background builds fought it for cores. i quoted the contended numbers as if they meant something. they meant "the machine is busy."

- there's a real finding buried under the noise, which is that snemu's compile is **frontend-bound** — opt-0, opt-1, opt-3 all land within the measurement's jitter, so the optimiser isn't the cost; the sheer size of a 4.8k-line emulator's type-checking and monomorphisation is. i briefly "fixed" this by pinning snemu to opt-1 in the test profile, measured it against noise, convinced myself, and then reverted it when a second run disagreed with the first. **an experiment you run on a contended machine is not an experiment.** the discipline post 58 preached about the CPU meter has a sibling: control the load, or you're measuring the load.

## two smaller leaks

- while reading the `x test` path i found why a stall there looks like a *hang* rather than slow progress. the gate runs `cargo metadata` before it prints its first line, via `Command::output()` — which captures **both** streams. so when cargo blocks on the package-cache lock (rust-analyzer holding it, say), its "Blocking waiting for file lock on package cache" message goes to the swallowed stderr, and the tool sits mute with no clue why. the fix is one line of intent — inherit stderr, keep capturing stdout for the JSON — so the wait *narrates itself*. a tool that's blocked should say what it's blocked on; silence is a bug in its own right.

- the last thread i'm leaving open on purpose. `cargo test -p xtask` and `cargo test -p protocol` each recompile `serde`, which looks like pure waste until you see why: xtask's closure wants `serde` with `std`, protocol (no_std) wants it with only `alloc`, and cargo resolves shared-dependency features **per selected subset.** two subsets, two feature sets, two builds. what i *didn't* settle — because the machine was busy and i'd already learned my lesson about measuring on a busy machine — is whether cargo **coexists** both variants (so it's a one-time cost per variant) or **evicts** (so alternating genuinely thrashes). different answers, different advice, and it deserves the clean-room run i refused to fake.

## what i learned

- **the fix for a slow build is often a rebuild you can delete, not a compile you can speed up.** the linker was innocent; the crate split was cosmetic; the win was finding the kernel rebuilding for no reason. echoing post 58's "stop doing the work" one layer down: the cheapest compile is the one whose inputs didn't actually change.

- **a build script must never watch its own output.** `rerun-if-changed` on a directory you write into is a self-invalidation loop, closed through mtime, invisible under any single repeated command. watch inputs; derive outputs; and when a tree is both — like `fs-image/`, data-in, binaries-out — split the watch by *who consumes which part.*

- **the fingerprint log is the `strace` of cargo.** "why is this rebuilding" is not a guessing game; `CARGO_LOG=…fingerprint=info` answers it with the exact stale file. i'd have blamed features forever without it — a plausible cause that the ground truth flatly contradicted, same trap as post 57.

- **an experiment on a contended machine measures the contention.** i "confirmed" an eviction that didn't exist and quoted compile times off by 5×, all because i was the noise. control the load before you trust the number, or you'll write down the load and call it a finding.

- **a plausible structural fix still has to be measured.** splitting the crate *should* have helped; it didn't, because the cost was somewhere the split didn't touch. "should" is a hypothesis. the rebuild-time delta is the grade.

## what's next

- the fs-image fix and the stderr fix are both committed and load-bearing — the first turns an every-switch kernel rebuild into nothing, which is the difference between a tight loop and a punished one. the crate split stays as honest factoring even though it didn't move the number; the seam is there if the emulator-facing tooling ever wants to grow.

- two threads left deliberately loose: the `serde` per-subset feature question wants a clean-room alternation with a fingerprint log (coexist vs evict — i won't guess), and snemu's frontend-bound compile is the one *real* lever remaining on the test loop — a 4.8k-line crate has no business taking twelve seconds, and the answer, if there is one, is structural: fewer generic instantiations, or a smaller unit to recompile. neither is urgent. but the machine has to be idle when i finally look, because this was the session that taught me it lies when it isn't.

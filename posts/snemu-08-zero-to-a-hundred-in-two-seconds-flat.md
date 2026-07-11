# snemu 08 — zero to a hundred in two seconds flat

the whole fidelity suite — every itest scenario, all 111 of them, each one a real
kernel booted and driven under my own emulator until it proves or disproves a claim
about SnitchOS — now runs in **two seconds**. it was three and a half when this
session started, and the story of that second-and-a-half is not the story i
expected to write. i thought i was building a JIT. i spent most of the time
discovering that the JIT was never the bottleneck.

## the engine that didn't matter

M6 is the block JIT: stop interpreting one guest instruction at a time, compile a
whole basic block once, cache it by PC, re-run the compiled form. backend A is the
portable one — a reified IR (`Vec<Op>` of plain data, so a future native backend can
lower the same ops) walked by an interpreter. i built it, grew its instruction
coverage until real kernel blocks were long, and measured.

parity. 3.4 seconds with the JIT on, 3.5 with it off. the thing i'd built to make
the emulator faster made it, within noise, exactly as fast.

i almost wrote that down as the result. backend A's per-op cost is about the same as
the tree-walker's — the decode cache already removed the redundancy an IR interp
would recover, and after i flattened the CSR file the per-instruction interrupt
probe is cheap. there just wasn't much left to amortize. a fine, honest,
disappointing finding.

## the metric was lying

before shipping that verdict i went to look at *why* the numbers were what they
were, and the slowest-scenario table said something impossible: with the JIT on, the
suite did **2.57× fewer instructions**. a faithful JIT can't change how many
instructions the guest runs. it runs the same program.

it wasn't counting instructions. the column labeled `Minstret` was actually counting
host `step()` *calls* — and a JIT block is many guest instructions per call. so the
"2.57× fewer" was the block collapse, the speedup itself, mislabeled as if the guest
had done less work. for the interpreter, one step is one instruction, so the two
numbers coincided and nobody had noticed the label was a lie.

i fixed the metric to report the real guest clock. and the moment it was honest, the
picture inverted. the JIT *was* doing less host work — a third less across the suite.
it just wasn't showing up as wall-time. the win was real and it was being thrown
away somewhere between the work saved and the clock on the wall.

## the transmission was slipping

that somewhere was the packing. the itest runs scenarios in parallel across worker
threads; a good run keeps every worker busy to the last. with the JIT on, mean
utilization had collapsed to 60% — one worker pinned at 98% while nine others idled
at 55. the JIT had shrunk everything *except* one scenario, and that one now
dominated: `smp-tlb-shootdown-visible`, a negative test that scans for the absence of
a bad frame until its budget runs out.

why did the JIT make the *scanning* scenario relatively bigger? because its budget
was denominated in host step-calls. sixty million steps. pre-JIT, one step was one
instruction, so sixty million steps meant sixty million guest instructions — exactly
what it was tuned for. with the JIT, one step is a whole block, so the same sixty
million steps scanned **two hundred and forty-five** million guest instructions.
four times more work for the identical budget. the faster the emulator got at each
step, the more pointless scanning that scenario did per step. the optimization was
feeding the pole.

the fix is one word: denominate the budget in guest instret, not steps. now the
scan does the same guest work whether interpreted or JIT'd, and the JIT cashes its
speed as *less wall-time* instead of *more scanning*. smp-tlb dropped from 245M back
to 60M, stopped being the pole, packing went 60% → 85%, and the makespan fell 3.4 →
2.0. that's the second-and-a-half. none of it was the engine. all of it was the
transmission — a unit-of-measure bug in the test harness that had been silently
converting the JIT's gains into wasted motion.

## the ruler, a fourth time

with the makespan actually compute-bound now, i tried the textbook next
optimization: cache the block's register file in a host-local array instead of going
through the hart every op. built it, verified it byte-identical on and off, added a
flag so i could A/B it cleanly, and measured.

zero. two runs, the direction flipped between them — pure load noise. the register
file copy-in/out trades evenly against the short, control-flow-heavy blocks the
kernel actually produces. backend A has no more juice in it.

that's the fourth time now the ruler has disproved the optimization i was surest of
— after idle-skip, after the memset helper, after the decode cache told me decode
was never the cost. the pattern is the whole method: build the instrument first,
then the intuition is cheap to check and cheap to discard. i keep the register cache
in, flagged and neutral, because it's the shape backend B — the real native codegen,
the actual remaining speed — will want. but as a backend-A win it measured to
nothing, and i'd rather write that down than pretend.

## and then the tank was too big

one more thing came off, orthogonal to all of it. the machine i was cloning per
scenario was 128 MiB of RAM, deep-copied 111 times a run. so i taught the emulator to
track a write high-water — the highest byte the guest ever touches, which since
snemu is deterministic is an *exact*, reproducible footprint, not a guess.

every scenario but one peaks under 12 MiB. the lone exception is the frame-allocator
OOM test, which leaks frames until the pool is gone and so genuinely fills whatever
you give it — which makes it, conveniently, the one workload i pin *large* to keep
covering the big-machine path. everything else: twelve megabytes, tops. the default
was eight times oversized.

so the default machine is 16 MiB now. the report that found this ships too — a
per-workload footprint table that flags anything with more than 1.5× headroom, so
the next person can right-size in one glance instead of the archaeology i just did.
and it self-guards: if a scenario ever creeps toward its limit, the table warns
before it faults.

## what i learned

- **a mislabeled unit is worse than a missing one.** `Minstret` that was secretly
  step-calls didn't just fail to inform — it actively argued the opposite of the
  truth, and it argued it convincingly enough that i nearly shipped "parity" as the
  verdict. the fix wasn't a faster emulator; it was an honest column header.
- **the win and the throughput are different things.** the JIT reduced work from the
  first day. it took a budget bug — steps where there should have been instructions —
  to stop that work-reduction from leaking out an idle worker.
- **denominate budgets in the thing you actually care about.** a scan bounded by
  "steps" gets more expensive exactly as steps get cheaper. bounded by guest
  instructions, it doesn't care how fast you run it.
- **determinism is a measurement instrument.** an exact reproducible high-water let
  me cut the machine to an eighth without a single anxious "but what if a run spikes"
  — there are no spikes when the run is a function.

## what's next

the harness is wrung out. two seconds is packing at 85%, budgets in the right unit,
and a machine sized to what the guest actually touches — there's maybe three tenths
of a second of online-scheduling slack left, and the counterfactual re-pack i added
this session tells me exactly how much, but chasing it is polishing. the interpreter
tier is done too: register caching was the last thing i could think to try inside
backend A, and it measured to nothing. from here the two real directions both point
away from the thing i spent this session on.

- **backend B.** the reified IR was always a two-backend bet: A is the portable
  interpreter i've been measuring, B is native codegen — copy-and-patch stencils over
  the same ops, real machine code, the throughput a tree-walker structurally cannot
  reach. this is the only remaining lever that speeds the compute tail by a *factor*
  instead of trimming it. and the value-based memory ops i refactored for increment 4
  — the ones that turned out neutral for backend A — are exactly the shape B wants:
  compute an address, hand it a value, keep the registers in host registers. inc 4
  paid for itself after all; just not where i was looking.
- **the browser.** the entire reason backend A is a portable, `unsafe`-free
  interpreter and not a native JIT is so it can run inside a wasm sandbox. that's the
  bet the whole tier structure is placed on: SnitchOS booting in a tab, driven by the
  same emulator that runs these itests, with B as a host-only fast path and A as the
  everywhere tier. two-seconds-flat on my laptop is a nice number; the point of it
  being *this* emulator is that the number comes with me to the browser.

the third direction isn't the emulator at all — it's the harness's own scheduling,
which has more structure in it than i'm using. right now there's one snapshot per
workload: boot to a checkpoint, then every scenario forks that. but a snapshot is
just an execution state frozen mid-run, and states go deeper — after the FS server is
up, after a client has connected, after the first RPC round-trips. scenarios that
share a longer prefix of setup could fork from a *deeper* snapshot and skip
re-running the part they have in common.

the snapshots form a **tree**, not a graph. a state can't be the merge of two others
— you can splice a branch off an existing state, never weld two states together — so
every deeper snapshot descends from exactly one shallower one, all the way back to
cold boot, with scenarios hanging off the nodes as leaves. and here's the part that's
actually a nice problem: to run a leaf you first have to *materialize* its snapshot,
which means running its parent forward to the fork point — that costs time and blocks
the leaf until it's done. moving the frozen state between workers is cheap (a clone,
cheaper still with copy-on-write); the entire cost is the dependency ordering. so it
becomes tree-structured parallel scheduling: given a hundred-plus leaves hanging off
a tree of snapshots, in what order do you compute the internal nodes, and on which
workers, to unblock the most work soonest and land the whole thing fastest? do it
generically, close to optimal, with the tree discovered and saved from previous runs
so the next one starts warm — that's a genuinely interesting problem, and it's the
direct sequel to the packing i spent this session on. packing was scheduling
independent jobs. this is scheduling jobs that first have to grow the branch they
hang from.

and the small stuff, when i want it: work-stealing to take the last of the packing
slack, copy-on-write forks to make every clone near-free regardless of RAM. neither
is urgent at 16 MiB and 85% busy — but the tree scheduling is the one i actually want
to sit down and think about.

## coda

zero to a hundred, two seconds flat. the engine was the part i built and the part
that didn't matter. the speed was already in there; the whole job was to stop
throwing it away.

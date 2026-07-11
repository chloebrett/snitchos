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

zero to a hundred, two seconds flat. the engine was the part i built and the part
that didn't matter. the speed was already in there; the whole job was to stop
throwing it away.

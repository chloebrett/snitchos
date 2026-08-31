# Post 90 — the board bridge: "couldn't reach it" vs "it said nothing"

- this session started as a question — *"what's the state of network support?"* — and ended with a serial bridge, a milestone-closing collector source, and a decision about how the board gets driven for the next year.
- but the thing worth writing down is narrower. the bridge exists to preserve **one distinction**: *"could not reach the board"* must never look like *"reached it, and it said nothing."* on hardware both present as no frames and need opposite fixes. I spent the session building a taxonomy to hold that line — and then kept finding places where the code collapsed it. including code I had just written.
- [post 87](post-87-the-jh7110-gmac-driver-desk-half.md) was about artifacts confirming each other because they shared an ancestor. [post 88](post-88-kitsch-a-stitch-program-draws-to-the-framebuffer.md) was about a proxy that cannot fail. [post 89](post-89-three-observables-that-could-not-fire.md) was about an observable that could not fire. this one is the same family seen from the other side: **two different failures that return the same value**, so no test can tell them apart — and therefore no test fails.
- and one methodological finding I hadn't seen before: **an unkillable mutant is a design smell, not a testing gap.**

## what actually shipped

- **[`plans/board-bridge.md`](../plans/board-bridge.md), [`plans/vf2-gmac-driver.md`](../plans/vf2-gmac-driver.md)**, and an addendum to [`docs/network-telemetry-design.md`](../docs/network-telemetry-design.md) turning a one-line "RX is out of scope" into a scoped gap list with evidence.
- **the collector's `--serial` source** ([uart-telemetry.md](../plans/uart-telemetry.md) step 10) — the last item on B3/M2's critical path, split into a prefactor and the source itself.
- **`collector::serial`** — `call_out_alternative` and `SerialReader` moved out of `native` gating so a second consumer could reuse them without inheriting `ureq` → `ring` and a web server.
- **`xtask-board`** — `reach` (the failure taxonomy), `outcome` (exit codes as an interface), `wire` (the read loop), `echo`, and the `cargo xtask board exec` CLI. `stop`, `split` and `thrash`/`reboot` came from a parallel session in the same crate.
- **`kernel_boot::bootargs::uart_telemetry`** — the kernel now refuses to put telemetry on a UART that a human log is already using, and says so.
- **`cargo xtask test --no-fail-fast`** — small, but it converts "the tree is broken somewhere" into "one module is mid-edit."

## the distinction, and four places it collapsed

the taxonomy is `reach::Unreachable`, and its variants partition by **what the operator must do differently** — kill a process, fix permissions, plug the adapter in. that axis is the whole design. here is where it leaked.

**1. the read loop, which I wrote.** `wire::capture` returned `StopReason::Timeout` for *both* a deadline and a dead transport. every test passed. the flaw was not a missing assertion — it was that the two cases produced the same value, so **no assertion could exist**. the fix splits `Ended::{Stopped, TransportFailed}`, and a mid-capture death now exits `2`, so an unattended loop retries the *host* rather than rebooting a board that was answering fine.

**2. `serialport::ErrorKind::NoDevice`**, by the crate's own documentation, covers both *"in use by another process"* and *"disconnected while performing I/O"*. opposite diagnoses, one variant. resolved with evidence rather than a guess: if `lsof` names a holder, the device plainly exists, so `refine_with_holder` upgrades it to `PortHeld`. a port something has open was never missing.

**3. `OpenFailed { kind: Other }`** now has two causes on record, found a day apart. the board session hit it as a **sandbox denial** — a host permission problem that looked exactly like a board fault. I hit it again trying to open a **pseudo-terminal**, which exists and cannot be driven. `Other` is where unlike things go to look alike, and it is one variant short.

**4. `console=text` plus the UART telemetry sink.** on hardware the UART is the only transport, so frames and `println!` share one wire. in text mode they interleave mid-line and destroy each other:

```text
ph��:� postr-eilpeaise
```

that is worse than either failing alone — the human loses the boot log *and* the collector cannot decode. the kernel now asks before installing, and refuses in text mode with a line saying how to opt in. text stays the default deliberately: bring-up is when the raw log is the only working channel.

## an unkillable mutant is a design smell

this is the part I'd not internalised.

the `Ended` flaw surfaced through mutation testing, but not the usual way. two mutants on the transport-check branch **timed out instead of dying** — the suite hung rather than failed. my first reading was a weak test, so I shortened the capture deadline to make the failure fast. they timed out again.

the second reading was the right one. flipping the guard made the loop absorb a real error as if it were idleness, run to its deadline, and return `Timeout` — *which is what the test expected anyway*, because the honest answer and the wrong answer were the same value. the mutant was unkillable because the code had no distinguishable behaviour to assert on.

surviving mutants say a test is missing. **unkillable ones can say a distinction is missing.** once the split landed, both died immediately.

it caught a second, smaller instance the same way: `MISSED  delete match arm Unreachable::NoSuchDevice{device} in consult_lsof`. the `NoDevice` disambiguation — the thing I was most pleased with — sat inline in glue where no test could reach it. extracted, it is three tests and 0 survivors. twice in one session mutation testing found *design* problems rather than coverage gaps.

## the misread I made

I wrote, in a plan, that a DHCP reservation *"removes the failure class"* of a drifting `serverip` — and used that to demote the step whose entire design is to re-derive the address at run time instead of hardcoding it.

it removed the instance. macOS Private Wi-Fi Address was **rotating** the MAC, and a reservation cannot pin an address the client changes. so `serverip=192.168.0.7` was correct when written and became wrong with nothing changing in the repo, the board, or the doc — which is exactly what makes it a class. it cost most of a board evening.

then I corrected it wrongly a second time: `ifconfig` still showed a locally-administered MAC, and I read that as *"the setting hasn't taken effect"*. it had. the axis is **Fixed vs Rotating**, not private vs not — under Fixed a locally-administered MAC is the expected steady state, and a reservation pins it fine. I had inferred from one observation without knowing what the observation meant.

the demotion was wrong on the merits too: `provision` deriving `serverip` from `ipconfig getifaddr en0` was right *for precisely the reason the session proved*.

## the negative result worth keeping

`serialport` will not open a pseudo-terminal. the pty allocates, passes the `cu.*`/`tty.*` check, and then:

```
board: OpenFailed { device: "/dev/ttys014", kind: Other }
```

it queries termios/ioctls a pty does not provide. so the open→write→capture glue is **genuinely board-gated**, not merely untested — worth writing down before the next person spends the same twenty minutes finding out.

## what is still open

- **nothing has crossed a real UART from the bridge.** `serialport::open` and the live read loop are the untested boundary, and both [uart-telemetry.md](../plans/uart-telemetry.md) step 10b and board-bridge step 4 have acceptance criteria only hardware closes. everything from step 4b onward — interrupting the autoboot countdown — has no host-side oracle at all.
- **`OpenFailed{Other}` wants splitting**, now that it has two recorded causes. a host-side permission denial is neither a held port nor a missing device.
- the on-board steps are batched as §3 of [`docs/next-board-session.md`](../docs/next-board-session.md), including one hypothesis only hardware can settle: what `read()` returns when the adapter is unplugged mid-capture. if it is `Ok(0)` forever rather than an error, `wire::capture` reads a dead cable as a quiet board — the same collapse, one layer down.

## what I'd tell myself

- **when two failures need different fixes, check they can produce different values.** a taxonomy is only as good as the layer beneath it; mine was correct at the top and lossy at the bottom.
- **a mutant that times out instead of dying is telling you something.** twice I read it as a slow test. it was a code path with no observable difference to assert on.
- **"removes the failure class" is a strong claim and I made it casually.** it is worth the extra sentence asking *what mechanism could reintroduce this?* — the answer was a laptop rotating its MAC, and nothing in the repo would ever have shown it.
- **don't infer from an observation you can't interpret.** a locally-administered MAC meant the opposite of what I assumed, and I had a 50/50 guess dressed as a diagnosis.
- **the strongest failure message names what it saw, not what it wanted.** `primed=true, xruns_total=false, frame=true` was [post 89](post-89-three-observables-that-could-not-fire.md)'s; this session's equivalent is exit `1` vs exit `2`, and the whole bridge is built so an unattended loop can tell them apart.

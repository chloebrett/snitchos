# 🎲 Randomness & entropy

*Not a v0.1 concern. Lands around v0.6b. Captured now so the decisions aren't re-litigated later.*

# The core distinction: uniqueness vs. unpredictability
The single most useful idea in this area. Two different properties, two different right tools:

- **Uniqueness** — "this value must never collide with another." A **counter** is the correct tool. Zero cost, zero entropy needed, guaranteed.
- **Unpredictability** — "an attacker must not be able to guess this value." A **CSPRNG** is mandatory. A counter is useless; a statistical PRNG is useless.

Getting the wrong property is a real bug in both directions: using randomness for span IDs is wasteful and needs an entropy source that doesn't exist yet at boot; using a counter (or a statistical PRNG) for a stack canary is decorative security worth nothing.

# What needs which

## Needs only uniqueness — use a counter
- **Span / trace IDs.** Nobody attacks trace IDs; they just need to not collide. SnitchOS has one kernel and one counter, so there is zero collision risk. OpenTelemetry uses random IDs only because many uncoordinated machines mint IDs in a distributed system — not our situation. Decision: span IDs are a per-CPU-partitioned `u64` counter. No RNG involved.
- **Nonces, in many cases.** A nonce is a "number used once" — uniqueness is the requirement, not unpredictability. A counter works fine as a nonce. (Some protocols want random nonces; those draw from the CSPRNG. But counter-nonces are valid and common.)

## Needs unpredictability — requires a CSPRNG
These are all "unpredictability *is* the entire mechanism" defenses. They don't encrypt or authenticate anything — they only deny the attacker knowledge. The instant the secret becomes predictable, the defense is worth zero.

### Stack canary
The compiler places a known secret value on the stack between local buffers and the function's saved return address. Before returning, the function checks the canary is intact. A buffer overflow that smashes the return address must write *through* the canary to reach it — so a corrupted canary reveals the attack and the program aborts instead of returning into attacker-controlled code. **Must be unpredictable:** if the attacker knows the canary value, they simply include the correct value in their overflow payload and the check passes. Generated per boot (often per process/thread) from a CSPRNG.

### ASLR offset (and KASLR for the kernel)
Address Space Layout Randomization places code, stack, and heap at a random offset each load. Many attacks need to know *where* a target is in memory (a return address to jump to, a useful function to reuse); fixed predictable addresses let attackers hardcode targets. The random displacement is the "ASLR offset." **Must be unpredictable, and needs enough entropy:** if the offset is predictable or has too few random bits, the attacker recovers the layout and ASLR is bypassed (32-bit ASLR was brute-forceable for exactly this reason; 64-bit gives far more room).

### Keys and (random) nonces
- A **key** is secret cryptographic material for encrypting or authenticating data.
- A **nonce** keeps a crypto operation unique per use (reusing key+nonce leaks information). Nonces may be random or a counter — see above.

Keys are generated from the CSPRNG or via key derivation. Where SnitchOS uses them — all post-v1.0, tied to roadmap features:

- Filesystem encryption at rest (CoW / content-addressed FS deepening) — block keys + per-block nonces.
- Tamper-evident / authenticated FS history (Merkle work) — signing or MAC-ing snapshots.
- Authenticated remote mounts / networked capabilities (v1.2 networking) — identity proof, traffic encryption.
- Signed capability tokens for cross-machine capability flow — unforgeable caps that travel between machines.
- Sealing / unsealing capabilities, if implemented.

# Algorithm choice

## Mersenne Twister is disqualified for anything security-relevant
MT19937 is a fast, well-distributed *statistical* PRNG — fine for simulations, Monte Carlo, games. But it is trivially predictable: observing 624 consecutive 32-bit outputs reconstructs the entire internal state and predicts all future (and past) output. For canaries, ASLR, or keys it is equivalent to no security at all. It has no place in the kernel except possibly userspace simulation code.

## ChaCha20-based CSPRNG is the right answer
A ChaCha20-based CSPRNG (e.g. the `ChaCha20Rng` construction) is cryptographically secure: given a seed it produces an unpredictable stream that cannot be run backward or predicted forward without the seed. This is the same construction Linux's `getrandom()` uses internally. **A properly seeded ChaCha20 CSPRNG is sufficient for SnitchOS V1 security randomness.**

Note the precision: ChaCha20 as a *stream cipher* is a cipher; a *ChaCha20-based PRNG* is a CSPRNG — a seed expander. Both are cryptographic; the PRNG is what we want for generating canaries/offsets/keys.

# The actual hard problem: seeding
A CSPRNG is deterministic — it expands a seed. If the attacker knows or can guess the seed, ChaCha gives nothing. The load-bearing words are "properly seeded." A kernel at boot has almost no entropy — the classic boot-time entropy hole.

**Seed sources, in descending order of trust:**

1. **Hardware RNG instruction.** RISC-V Zkr extension (the `seed` CSR); aarch64 `RNDR` / `RNDRRS`. Real entropy on demand — best source when the platform has it.
2. **`virtio-rng` device.** QEMU hands entropy from the host RNG. For a QEMU-hosted kernel this is the pragmatic best answer — real entropy, simple driver. Lean on this for V1.
3. **Timing jitter / interrupt timing.** Harvest low bits of the cycle counter at unpredictable moments. Real but slow to accumulate and weak early in boot.
4. **Bootloader / device-tree-provided seed.** Convenient, but the host then knows the seed — fine for a toy OS with a trusted host, useless as a real security boundary.

# Recommendation for SnitchOS
- **v0.1 (now): build no RNG.** Span IDs are counters. No randomness is needed yet.
- **v0.6b (capabilities) onward:** introduce a single kernel CSPRNG instance (ChaCha20-based), seeded at boot from the best available source, behind an `Rng` / `EntropySource` trait so the seeding backend is swappable. Seed priority: `virtio-rng` → RISC-V `seed` CSR → device-tree seed → cycle-counter jitter. Reseed periodically as entropy accumulates.
- **The trait matters more than the algorithm.** Same principle as everywhere else: `Rng`/`EntropySource` is the interface, ChaCha is one implementation, the seed source is pluggable. Porting to aarch64 or real hardware changes only the seeding backend.
- **Scope honesty.** "ChaCha20 CSPRNG + virtio-rng seed + periodic reseed + a trait boundary" is the right amount of engineering for V1. It is *not* a hardened production entropy subsystem (Linux's `random.c` is thousands of lines: entropy estimation, multiple pools, reseed scheduling, premature-use handling). That depth can become its own milestone later if it gets interesting.

# Observability angle (on-brand)
The entropy subsystem should itself be traced — estimated entropy at boot, each reseed event, which source supplied each seed. "Watching the kernel collect entropy" is a genuinely good blog post and an under-explained topic generally. The traced-entropy view is a natural SnitchOS demo.

# Decisions locked
- Span IDs: per-CPU-partitioned `u64` counter. No RNG.
- No randomness subsystem in v0.1.
- CSPRNG arrives ~v0.6b: ChaCha20-based, behind an `Rng`/`EntropySource` trait.
- Seed source priority: virtio-rng → RISC-V `seed` CSR → device-tree seed → timing jitter.
- Mersenne Twister: never used for anything security-relevant.
- Entropy subsystem is traced like everything else.

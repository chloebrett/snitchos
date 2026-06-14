# v0.10 — Badging Model in the FS — Learning Session Log

## Session 1 — 2026-06-14 (~40 min)

**Format:** quiz-first (learner is the author); continuation of the v0.9 badge
session, applied to the filesystem. Grounded in `docs/filesystem-design.md` +
verified against `fs-proto/src/lib.rs` (Badge layout, Op, Request/Response).

**Performance by area:**
- **Two rights namespaces** (endpoint SEND/RECV/MINT kernel-enforced vs file
  READ/WRITE FS-enforced-in-badge): correct mechanics; named principle as
  "abstraction" → sharpened to **mechanism/policy separation** ("kernel never
  learns what a file is"). Analyze.
- **Where `open()` went:** correctly landed on `lookup`; got "open is stateful"
  unprompted — the exact seam. Completed: lookup mints a File cap (open returns a
  *capability*, not an fd); trait is stateless; cursor client-side. Analyze.
- **MINT structure:** asked for it explicitly (genuine gap) — taught with a
  diagram. MINT = right; `MintBadged` = op; mint derives a child naming the same
  endpoint id with chosen badge+rights. Now owns it.
- **Attenuation / two failure modes:** got O/H asymmetric-rights via two minted
  caps. In the Feynman, conflated "unnameable" with "FS rejects" — corrected to the
  two distinct modes (held-cap-wrong-op → reject+snitch; no-cap → unnameable/
  unreachable). The one real error of the session, now fixed.

**Learner-driven insights (the high points):**
- **The access-matrix transpose was the unlock** ("that's the fundamental thing
  that made it click"): ACL = matrix by column (objects/Linux); capabilities =
  matrix by row (subjects/SnitchOS). Same info, transposed.
- Pushed to the deepest question unprompted: *"where is the policy decision
  made?"* — and answered his own follow-up correctly: a shell holding broad
  authority delegates narrower caps to the programs it runs.
- **Proposed a novel explicit-permission shell** — which is exactly the
  Plash/powerbox/Genode launcher-as-policy-point pattern. See memory
  [[project-explicit-authority-shell-idea]].

**Final Feynman (cat /foo):** strong, owned the policy-vs-mechanism +
kernel-vs-FS spine. Two prods: (1) name→cap resolution happens in the *shell* via
`lookup` at spawn time — `cat` gets a cap, never a path; (2) `/etc/secret` is
*unnameable*, not *rejected*. Both corrected.

**Calibration:** entered "a bit of everything / quiz me." Performance: strong
conceptual grasp, reached Create-level (designing a shell model) by the end. The
gaps were precision (mechanism/policy naming, reject-vs-unnameable), not
substance — well-calibrated.

**Tutor accuracy note:** verified `Badge` packing against `fs-proto` and read
`docs/filesystem-design.md` before quizzing (verify-first discipline). Flagged
honestly that the `∩ requested` attenuation is designed but not yet wired (no
requested-rights field in the `Lookup` message; `w3` is the spare home for it).

**Gaps tagged for spaced review:**
- "Where is file policy decided, and what makes a policy server NOT Linux?"
  (delegation-time vs access-time litmus).
- "Reject vs unnameable" — the two cap failure modes.
- "lookup = open = the cap-minting op; what comes back is a cap not an fd."
- Lineage: what SnitchOS takes from seL4 vs deliberately omits (untyped memory).

**Artifacts:** `learning/v0.10-fs-badging/cheat-sheet.md`, this log.
**Next candidates:** the kernel cross-AS copy primitive (option D — the one piece
below the trait); building the `user/fs` front-end (now started); the explicit
-authority observable shell.

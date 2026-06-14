# v0.9 IPC over Capabilities — Learning Session Log

## Session 1 — 2026-06-14 (~30 min)

**Format:** quiz-first (learner is the author), "a bit of everything" across four areas:
rendezvous/blocking, call/reply + reply caps, badges, caps-as-security.

**Performance by area:**
- **Q4 caps / ambient authority:** strong. Named "ambient authority" and "confused
  deputy" unprompted. Reached Evaluate.
- **Q3 badges:** right instinct ("lives in a table, unforgeable") but located the
  protection wrongly (per-process table possession). Corrected to the real three
  legs: badge is a *field of the kernel-held cap* (no send-side badge argument),
  set by the MINT holder at mint, client lacks MINT to forge another. Reached
  Analyze after correction.
- **Q2 reply caps:** (a) partial — had the "one caller, one instance" intuition but
  not the "object encodes entire authority ⇒ nothing to rights-check" framing.
  (b) initially dodged the danger ("only works if caller is waiting"), then on the
  push **got it**: a stale reply hits a *later* call (B) the client is now waiting
  on → silent cross-call corruption; consume + generation-bump refuses the 2nd
  reply. Reached Analyze/Evaluate.
- **Q1 rendezvous:** "not sure" → taught. The elegant bit (enum makes both-sides-
  waiting unrepresentable; logic makes it unreachable) landed. Message-copied-to-
  kernel-at-syscall-time understood.

**Learner-driven insights (unprompted, both correct):**
- Endpoints are per-endpoint independent state machines; the kernel holds a table.
- **Many-to-many / worker-pool pattern:** many receivers on one endpoint = work
  distribution; clients don't pick a server; private endpoint = direct line. (I
  refined: FIFO, not load-aware.)

**Final Feynman:** good endpoint definition; two gaps — said "four bytes" (it's
four u64 *words*, 32B) and walked `send` not `call` (stopped before the reply).
Used the gap as the lead-in to the climax question.

**Climax — learner's question "does RPC need SEND+RECV?":** No. Caller=SEND only,
callee=RECV only; the reply rides the one-shot reply cap, bypassing endpoint rights.
This is the elegance that avoids a bidirectional channel + spoofed replies. Walked
the full call lifecycle to show it.

**Calibration:** entered "a bit of everything / quiz me"; performance showed solid
grasp with two genuine gaps (rendezvous mechanics, reply-cap security argument),
both closed in session. Well-calibrated self-assessment.

**Tutor accuracy note:** verified the rendezvous claims against
`kernel-core/src/user/ipc.rs` before teaching (after two earlier-session errors).
Pure transition fns are `on_send`/`on_receive` returning
`RendezvousAction::{Block, Rendezvous{peer}}`; `send_begin`/`receive_begin` are the
kernel-side wrappers.

**Gaps tagged for spaced review (ask next session):**
- "RPC needs SEND xor RECV, never both — why?" (should answer: reply rides the
  one-shot reply cap, not endpoint rights).
- "Why is a reply cap safe with no rights check?" (object encodes entire authority;
  self-extinguishing via consume + generation bump).
- "How is a badge unforgeable?" (field of kernel-held cap, no send-side argument,
  set at mint, client lacks MINT).

**Artifacts produced:** `learning/v0.9-ipc/cheat-sheet.md`, this log.
**Next candidates:** v0.10 RAMfs (first IPC consumer); notifications (v0.9d);
send-carries-caps + GRANT.

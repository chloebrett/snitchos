# Design: accounts and login in a capability world

**Status**: Vision / design-exploration note (unbuilt, open questions flagged).
What do "user accounts" and "login" mean when authority is capabilities, not
identity? The short answer: the Unix "user" *dissolves* — it bundled five separate
concerns into one uid, and in a cap world they come apart cleanly. Only one of them
is really "login."

## The reframe: capabilities dissolve the user as a security principal

Unix authorization is **ambient and identity-based**: every process runs "as" a
uid, and the kernel checks that uid against ACLs (the owner/group/other rwx bits)
on every access. Your *identity is your authority*. This is the source of the whole
mess — root (uid 0 = ambient god-mode), setuid, the confused-deputy problem,
privilege escalation.

Capabilities are the opposite. Authority is not *who you are*, it's *what caps you
hold*. The kernel never asks "who is this process?" — it asks "does it hold the cap?"
There is no ambient identity to check, no ACL to consult. So the capability model
doesn't handle users differently — **it removes the user from the authorization path
entirely.**

Which raises the real question: if authority is caps, not identity, **what is a
"user" even for?** The answer is that a Unix "user" was five things wearing one uid,
and they separate:

| A Unix "user" bundles… | …which in a cap world becomes |
|---|---|
| **Authentication** (proving a human is who they claim) | a mechanism that converts a proof into a **cap bundle** — this *is* login |
| **Authorization** (what you can do) | **the caps you hold** — no identity needed; not part of "the account" at all |
| **Identity/name** (a handle others refer to you by) | a **name for attribution/provenance/social**, decoupled from authority |
| **Private storage** (your $HOME) | a **cap to your storage root** — privacy by cap-holding, not by ACL-checking |
| **Accountability** (who did X) | a **tag on the observable trace** — the snitch story, per human |

The load-bearing line, straight from the object-capability tradition: **login is the
single moment where authentication is converted into authorization.** You prove who
you are *once*, and in exchange you receive a bag of capabilities. After that instant
the system never asks "who are you?" again — only "do you hold the cap?"

**A user session is `init` for a human.** v0.13's `init` is the delegation-graph root
for the *system* — born with a startup cap bundle, delegating to its children. A
login spawns a **session process** that is `init` for a *person*: born holding that
human's cap bundle, delegating onward to their shell, their editor, their clipboard.
Same bootstrap pattern, one level up.

## What login looks like

Login is a **capability-minting ceremony**, run by a login service (an actor):

1. it takes an authentication proof (password, key, passkey);
2. on success, it obtains the human's cap bundle (see "where do the caps come from");
3. it **spawns a session process** holding that bundle and hands over control (the
   session runs the shell); and
4. the whole thing is a span — `session.login` opening, the cap delegations visible
   on the wire.

**Logout is revocation.** The session's authority is exactly its caps, so logout =
drop/revoke the session's cap bundle (v0.13's transitive `Revoke` already does this),
and the human's authority evaporates — cleanly, transitively, observably. Compare
Unix, where "your processes keep running as your uid" after you log out. Here,
authority has a lifetime that *is* the session.

## Where do the caps come from? (the one real fork)

Two models. This is the main design decision.

**A) A root authority delegates.** A system "root authority" (the ur-cap holder,
like init's parent) holds, per registered human, a cap bundle. A credential store
maps `proof → which human`. On successful auth, the root authority delegates that
human's bundle into the new session. Central, simple, but there's a party that holds
everyone's authority.

**B) Sealed caps — the account *is* an encrypted cap bundle; the password is the
key.** Your capabilities are stored **encrypted at rest**, and authentication
*decrypts* them. Login is literally "decrypt my authority." This is beautiful for
several reasons:
- there's no central table of who-can-do-what — just "can you decrypt your caps?";
- your account is a **portable blob** — you could carry it, back it up, move it;
- it makes caps **survive reboot** (the persistence problem) by construction; and
- it matches the mental model exactly: *your password unlocks your authority.*

It's essentially **Plan 9's factotum / a keychain** raised to the whole account: your
authority is sealed, and one secret unseals it.

**Lean: B (sealed caps), with a small root authority only for bootstrapping and
recovery.** The tension B creates is real and worth stating up front (see open
questions): if your authority *is* your sealed bundle and nobody else holds it, then
"admin resets my password" has nowhere to stand — you can't reset what you can't
decrypt. That's the cap-world version of "not your keys, not your coins," and it
needs a designed-in recovery cap, not an afterthought.

## Sharing, groups, and the absence of root

- **Sharing is delegation, not an ACL edit.** Chloe shares a file with Bob by
  minting Bob an *attenuated* cap to it — read-only, or time-boxed, or revocable —
  not by adding his uid to a permission bit. Finer-grained, safer (no ambient
  authority to be confused about), and **observable** (a `CapEvent` on the wire).
  This is the same act as a clipboard cap-transfer or a shell delegation — one
  primitive.
- **Groups/teams** are not gids. A "team" is a shared endpoint or a role's cap
  bundle that several sessions hold. Membership = holding the cap. No global group
  table.
- **There is no ambient root.** "Admin" is just whoever holds the powerful caps
  (the system root authority). Admin actions are cap-mediated like everything else —
  no setuid, no `sudo`, no escalation, because *there is nothing to escalate to*:
  you either hold the cap or you don't. **The confused-deputy problem — the classic
  reason caps beat ACLs — is designed out**, and it's designed out for humans too,
  not just programs.

## Why this is on-thesis (the SnitchOS payoff)

- **Explicit authority, for humans.** A user's authority is their session's cap
  table — *inspectable*. You can look at a running session and see exactly what that
  person can do, with no hidden uid-based powers. `hold()` writ large. "Watch
  least-authority happen" applies to people, not just processes.
- **Observability.** Login/logout/share are spans; a session's authority and its
  whole delegation subtree are on the wire. Auditing a user is *reading their trace*,
  not grepping a syslog for a uid. The OS already narrates everything; a user is the
  identity you attribute the narration to.
- **The cap-id spine (v0.13).** Every cap holding has a stable id + parent, forming
  a global derivation tree. **A human's authority is a subtree** rooted at their
  session — you can see the entire tree of what a login granted onward, and revoke
  any branch.
- **Identity for provenance, not authority.** The human's *name* is what tags the
  data-flow (the clipboard's "chloe copied this from that") and the trace. Identity
  and authority are finally separate: your name says who to *attribute*, your caps
  say what you can *do*.

## Prior art (worth mining)

- **KeyKOS / EROS / Coyotos** — the canonical capability operating systems. In all
  of them the "user" is a *userspace* construct over a pure-cap kernel; EROS's
  confinement and authority-bootstrapping are the reference for how a session's caps
  come to be. This is the closest ancestor for "accounts in a cap OS."
- **Plan 9 `factotum`** — the auth agent that holds your keys and speaks auth
  protocols on your behalf, so your other programs never see the secrets. The
  sealed-caps model is factotum for your whole authority.
- **Object-capability theory (E, Mark Miller, the ocap community)** — the
  authority-vs-permission distinction and "authentication is converted to
  authorization" come from here. The philosophical backbone.
- **Macaroons (Google)** — attenuatable, delegatable bearer tokens with caveats:
  literally capabilities for distributed systems, and a great concrete model for the
  attenuate-on-share story (`read-only`, `expires-in-1h`).
- **OAuth bearer tokens** — a bearer token *is* a capability (holding it = authority,
  no identity check). The mainstream already backed into caps for authorization;
  SnitchOS just makes it the whole model, coherently.
- **Passkeys / WebAuthn** — the modern *authentication* side (the proof), cleanly
  separable from the authorization it unlocks.

## Open questions (the hard parts)

- **The credential store.** Where auth material lives and how a proof maps to a cap
  bundle. In model B, the bundle is encrypted under a key derived from the secret —
  so the "store" is just the sealed blobs; but the KDF/crypto is the one genuinely
  "normal" part and must be gotten right.
- **Persistence of caps.** Model B needs caps that survive reboot (encrypted at
  rest). This rides the **checkpoint/persistence axis**, which isn't built — so a
  full account system waits on it (or a soft form re-derives caps at each login from
  a root authority, i.e. lean on model A first).
- **Recovery.** The cap-world "forgot my password" tension: if authority is a sealed
  bundle, nobody can reset it without a **recovery capability** designed in from the
  start (a sealed recovery share, a social-recovery quorum, a hardware root). Must be
  a first-class part of the design, not bolted on.
- **Bootstrapping the first account** and the root authority (who holds the ur-caps;
  how account #1 is created).
- **Multi-session / the same human twice** — two concurrent sessions each hold a
  (sub)bundle; revoking one shouldn't kill the other. The cap-id spine's subtree
  model handles this, but the delegation policy needs spelling out.

## Relationship to the rest of the system

- Reuses: `init`/session bootstrap (v0.13), the startup-cap ABI, the cap-id
  derivation spine, transitive `Revoke`, and the observability wire — almost all the
  mechanism already exists; accounts are a *policy* over it.
- Waits on: the **persistence axis** (for sealed caps at rest) — so this is a
  "designed now, built when persistence lands" item, like several stim twists.
- Pairs with: the **clipboard/provenance** work (identity tags data flow) and the
  **shell** (the first thing a session runs; where delegation is a visible verb).

---
*Delete this file if the idea is abandoned or absorbed into another design.*

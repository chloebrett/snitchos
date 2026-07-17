# v0.9 IPC over Capabilities — Cheat Sheet

## Endpoint = a capability-named rendezvous point
- An endpoint is a synchronous meeting point. Named by a cap: `Object::Endpoint { id, badge }`.
- `id` indexes the kernel's table (`ENDPOINTS: Mutex<Vec<Endpoint>>`, `kernel/src/trap/ipc.rs`). Many endpoints, each with its own independent state.
- Pure rendezvous logic lives host-tested in `kernel-core/src/user/ipc.rs` (`on_send`/`on_receive`). Kernel side owns the table, parked messages, block/wake.

## The state machine (one per endpoint)
```
enum EndpointState { Idle, SendersWaiting(VecDeque<TaskId>), ReceiversWaiting(VecDeque<TaskId>) }
```
- **Invariant: never both sides waiting.** Enforced two ways: (1) it's an enum — both-non-empty is *unrepresentable*; (2) `on_send`/`on_receive` deliver-and-pop when the opposite side waits, only enqueue when it's empty — so both-non-empty is *unreachable*.
- **Many-to-many.** Each side is a *queue* — multiple senders AND multiple receivers can park (just not both sides at once). Matching is **FIFO**. Draining the last waiter collapses to `Idle`.
- **Worker pool:** many receivers on one shared endpoint = work distribution (FIFO, not load-aware). Shared endpoint = pool; private endpoint = direct line; the cap selects which.

## Message transfer
- Payload = **4 × u64 words** (`a1..a4`, 32 bytes), register-speed, no buffer copy.
- Copied **out of registers into a kernel `Delivered` struct at syscall time**, before anything blocks. Registers can then be clobbered freely.
- Parked message lives in the kernel keyed under **whichever party is asleep** (the one who got `Block` and will `take_delivered`/`take_reply` on wake).

## Rights (per-process CapTable, no ambient authority)
| Right | Gates | Checked by |
|---|---|---|
| `SEND` | send / call on an endpoint | `invoke_send` |
| `RECV` | receive on an endpoint | `invoke_recv` |
| `MINT` | mint badged child caps | `mint_badged` |
| (none) | use a reply cap | `invoke_reply` — possession is authority |
- Refusals **snitch**: `SyscallRefused { syscall, reason, task_id }` frame + counter. Never silent.

## RPC needs SEND xor RECV — never both
- **Caller** needs `SEND` only. `call` = send + block awaiting reply. Reply comes back via the blocked call being woken (`take_reply` from a kernel stash), NOT via a receive.
- **Callee** needs `RECV` only. Replies via the one-shot reply cap (no rights needed).
- The reply cap is what avoids a bidirectional channel: a naive "server sends back" would force server-SEND + client-RECV + risk of spoofed replies. Instead the kernel mints exactly "answer this one caller, once."

## Reply capability (the subtle one)
- `Object::Reply { caller }`, `Rights::NONE`. Minted by the kernel **at each call rendezvous**, handed only to the receiver of that call (in `a5`).
- **No rights check** because the object encodes the entire authority ("reply to this one caller, once") — nothing to subset.
- **One-shot:** consumed on reply; the slot's **generation** bumps so the stale handle no longer resolves. Possession of a consumed cap = possession of nothing.
- Why one-shot matters: client calls A (replied, resumes), later calls B (blocks). A second reply on A's stale cap would `stash_reply(caller)` + `wake(caller)` while the client waits on **B** → B silently receives A's payload. Consume + gen-bump makes the 2nd reply refuse. (One-shot + generation are the two halves of "self-extinguishing authority.")
- `reply_recv` = fused reply-previous-then-receive-next (the server loop).

## Badges (v0.9c) — demux without trusting sender identity
- A badge is a server-chosen, kernel-opaque `u64` baked into an endpoint cap at mint time.
- **Unforgeable**, three legs: (1) the badge is a *field of the kernel-held cap*, not a send argument (there's no badge register in `send`); (2) it was written by the `MINT` holder at mint; (3) a client lacks `MINT` so can't create a differently-badged cap.
- Delivered to the receiver in `a6`. Server uses it to tell clients apart (e.g. `0xBEE1`, `0xBEE2`) without trusting any sender-supplied identity.

## Wire frames (collector decodes → Tempo/Prometheus)
- `Message { endpoint, from, to, parent_span, t, hart_id }` — and the sender's span becomes the **parent** of the receiver's next span (trace crosses the IPC boundary).
- `CapEvent { kind: Granted|Transferred, cap_id, parent_cap_id, holder, object, rights, badge, ... }` — minting + reply-cap transfer + cap-in-reply.
- `SyscallRefused { syscall, reason, task_id }` — every refusal.
- Metrics: `snitchos.ipc.{messages,blocks,calls,replies}_total` (deferred-emission: atomic bumped in path, drained by heartbeat).

## Status
- v0.9 (send/receive), v0.9b (call/reply + one-shot reply caps), v0.9c (badges + mint + cap-in-reply): **shipped**.
- Deferred: notifications (async), general send-carries-caps + `GRANT`, client re-delegation, v0.10 RAMfs (first big consumer).

## Key files
- `kernel-core/src/user/ipc.rs` (pure rendezvous), `kernel-core/src/cap.rs` (caps/rights/invoke_*)
- `kernel/src/trap/mod.rs` (handle_send/receive/call/reply/reply_recv/mint_badged), `kernel/src/trap/ipc.rs` (tables, pending, stash)
- `protocol/src/lib.rs` (frames), `user/runtime/src/lib.rs` (bindings), `user/hello/src/bin/{ipc,rpc,badge}-*.rs`
- Plans: `plans/legacy/v0.9-ipc.md`, `v0.9b-call-reply.md`, `v0.9c-badges.md`; docs: `docs/ipc-design.md`, `docs/capability-system-design.md`

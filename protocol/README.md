# protocol

The wire contract between the kernel and the host. One `Frame` enum,
postcard-encoded, is the entire vocabulary the kernel uses to describe
what it's doing — spans, metrics, string/thread/hart registrations,
context switches. Everything observable about a running SnitchOS kernel
crosses this boundary as a `Frame`.

This crate is deliberately tiny and dependency-light: just the type
definitions, the postcard derives, and (behind a feature flag) a
host-side stream decoder. It carries no I/O, no transport, no business
logic — it is *only* the shape of the bytes, so both sides can agree on
them.

- **`no_std` by default** — the kernel (`kernel`, `kernel-core`) depends
  on it as-is.
- **`features = ["std"]`** — opts in to `protocol::stream`: the
  `std::io::Read`-driven decoder and the allocating `OwnedFrame`. The
  host crates (`collector`, `xtask`) enable this; the kernel never does.

Bin-adjacent library, `publish = false` — internal to the workspace.

## What's in it

| Item | Role |
|---|---|
| `Frame<'a>` | The wire enum. Every variant is one telemetry event. Borrows `&str` for zero-copy decode. |
| `StringId` / `SpanId` | `#[serde(transparent)]` newtypes over `u32` / `u64` — just integers on the wire, named in code. |
| `MetricKind`, `SwitchReason`, `HartRole` | Small enums carried inside frames. |
| `PROTOCOL_VERSION` | Emitted in `Hello`; bumped on every breaking layout change (history in the doc comment). |
| `stream::OwnedFrame` | `std`-only owned twin of `Frame<'a>` (`String` instead of `&str`). |
| `stream::decode_stream` / `try_decode_frame` | `std`-only stream + single-frame decoders. |

## Surprising details worth knowing

**Postcard encodes enum discriminants positionally — so variant order is
the wire format.** Adding a variant is safe *only at the end* of the
enum. Reordering or inserting in the middle silently shifts every later
discriminant and breaks decode of every previously captured stream. This
is the single most important rule in the crate; the `Frame` definition
carries a comment saying so. Bump `PROTOCOL_VERSION` on any breaking
change.

**There is no length prefix — the schema *is* the framing.** Frames are
written back-to-back with no outer length field. The decoder finds each
boundary by *parsing*: it reads the varint discriminant (which selects the
variant, hence the exact fields that follow), then consumes each field by
its type's own rule — integers are self-terminating LEB128 varints,
`&str`/`&[u8]` carry their own varint length, fixed scalars are a known
size. When the selected variant's last field is consumed, the frame is
done; `take_from_bytes` reports how many bytes that took and hands back
the remainder. So a frame's length is *discovered*, never declared.

```text
Frame::SpanEnd { id: SpanId(511), t: 1234 }  encodes as 5 bytes:
  0x02         discriminant 2 → SpanEnd, expect { id: u64, t: u64 }
  0xFF 0x03    id = 511   (varint: 0xFF high bit set → continue; 0x03 → stop)
  0xD2 0x09    t  = 1234  (varint: 0xD2 continue; 0x09 stop)
```

The catch: this is **not self-describing** (unlike JSON/CBOR). You cannot
decode without the matching `Frame` type, and a desync — reordered
variants, a dropped byte, a changed field type — silently misreads the
following bytes against the wrong schema rather than failing cleanly.
There's no length field to resync on, which is *why* the append-only rule
and `PROTOCOL_VERSION` are load-bearing, not bureaucracy. A truncated tail
is the one clean signal: postcard returns
`DeserializeUnexpectedEnd`, which the stream decoder treats as "read more
bytes," not an error.

**`Frame<'a>` borrows; `OwnedFrame` owns — and that split is structural,
not incidental.** The kernel is `no_std` with no allocator on the emit
path, so `Frame` holds `&str` (e.g. `StringRegister { value: &'a str }`)
and decodes zero-copy from the read buffer. But the host reader thread
needs to ship decoded frames through a channel that outlives that buffer,
so `OwnedFrame` is the same enum with `String` in place of `&str`.
`OwnedFrame::from_borrowed` bridges them. The match there is exhaustive
on purpose: **add a `Frame` variant and that match fails to compile**,
which is the intended reminder to update the host side.

**`try_decode_frame` returns `Result`, not `Option`, deliberately.**
`Option` would conflate "buffer ended mid-frame, wait for more" with "the
bytes aren't valid protocol." The `Result` keeps them distinct:
`DeserializeUnexpectedEnd` means read more; any other error means the
stream is corrupt or desynced and is worth surfacing rather than spinning
on forever.

**Some variants are defined ahead of their producers — on purpose.**
`Frame::Event` has no kernel emitter yet (profiling will ride on it per
the design doc), and `SwitchReason::{Preempt, Blocked, Exit}` are reserved
for preemption / blocking / task-exit that the cooperative v0.5 scheduler
doesn't do yet. They're locked into the wire format now so adding the
producer later needs no protocol change. "No in-repo producer" here means
*reserved*, not *dead* — see [docs/observability-design.md](../docs/observability-design.md).

**Id widths encode intent.** `StringId` is `u32` (the intern table has few
entries); `SpanId` is `u64` because span ids are a per-CPU-partitioned
counter expected to run for a long time across many harts.

**Histograms aren't a frame type.** `MetricKind::Histogram` declares a
metric's kind once (via `MetricRegister`); the actual observations come
over as ordinary `Metric` frames and are bucketed host-side by the
`collector`. The wire stays minimal.

## Tests

`cargo test -p protocol --features std`. The `lib.rs` tests roundtrip
every `Frame` variant through postcard (encode → decode → assert equal),
including non-zero values for fields an "always 0" mutant could otherwise
slip past. The `stream` tests cover partial reads, trailing bytes, and
multi-frame streams. Pure host tests — no kernel, no QEMU.

## See also

- [docs/observability-design.md](../docs/observability-design.md) — span semantics, the emit/decode split, why these primitives
- [collector](../collector) — the host consumer that decodes these frames into Tempo / Loki / Prometheus
- CLAUDE.md → "When changing the wire format" — the checklist for adding or changing a variant

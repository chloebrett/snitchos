# TX_STAGING race — captured evidence

Three failure captures from `sched-span-survives-yield` runs **before** the
fix, preserved here because `.itest-runs/` is gitignored and prunable.
They are the on-wire fingerprint of the dropped-guard race in
`virtio_console::send`. Full writeup: [`../tx-staging-cross-hart-race.md`].

| File | Origin run | outcome |
|------|------------|---------|
| `wedge-A.capture.json` | `.itest-runs/2026-06-08T05-19-42Z/fail-sched-span-survives-yield-3.capture.json` | disconnected |
| `wedge-B.capture.json` | `.itest-runs/2026-06-08T05-37-06Z/fail-sched-span-survives-yield-8.capture.json` | disconnected |
| `wedge-C.capture.json` | `.itest-runs/2026-06-08T05-37-28Z/fail-sched-span-survives-yield-380.capture.json` | disconnected |

## The fingerprint

The last `transcript` frame before the socket disconnect, in each:

```
wedge-A / wedge-B:  StringRegister { StringId(62) = "snitchos.tas\u{6}>(snitchos.task.hart_1_main" }
wedge-C:            StringRegister { StringId(63) = "snitchos.tas\u{6}?$snitchos.task.hart_1_" }
```

Two `StringRegister` payloads interleaved in the shared `TX_STAGING[256]`
buffer: `snitchos.tas` (truncated) + a control byte (`\u{6}`) + 2 garbage
bytes + `snitchos.task.hart_1_*`. The collision is deterministic at
hart-1 task registration (the one window where both harts emit at once),
which is why all three corrupt at the same string with the same seam.

Each file's summary fields (`outcome: disconnected`, `frames_seen ~140`,
`last_t_per_hart`, `frame_histogram`) and the full `transcript` are the
raw `FailureCapture` JSON written by the harness at `--capture tail`.

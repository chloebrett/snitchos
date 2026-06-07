# toy-virtqueue — exercises

How the kernel hands data to a device. Two exercises: the **driver** side
(publish a buffer) and the **device** side (consume it). Together they teach the
one rule behind the whole thing: **a device has no MMU, so it reads buffers by
*physical* address.**

```bash
cd learning
cargo test -p toy-virtqueue
```

Maps to **`kernel/src/virtio_console.rs`** (`transmit` + the descriptor ring).

---

## The model

- `Machine::phys` is a `Vec<u8>` standing in for **physical RAM**.
- `place_buffer(bytes)` copies bytes into `phys` and hands you back a
  **virtual address** (`pa + KERNEL_OFFSET`) — what the driver actually holds.
- The fake device indexes `phys` **directly** by the descriptor's `addr`. That
  direct index *is* "the device has no MMU": whatever number you put in `addr`,
  it reads `phys` at exactly that spot.

A virtqueue is three rings: the **descriptor table** (`{addr, len, …}`), the
**available ring** (driver → device), and the **used ring** (device → driver).

---

## Exercise 1 — `publish` (driver side) ★ the DMA rule

Fill a descriptor for the buffer at VA `va`, and make it available:

1. Pick descriptor id = `avail.idx as usize % QSIZE`.
2. Write `desc[id]` with **`addr = va_to_pa(va)`** (the physical address — *not*
   `va`), `len`, `flags = 0`, `next = 0`.
3. Push `id` into `avail.ring[avail.idx % QSIZE]`, then `avail.idx += 1` (wrapping).

The whole exercise hinges on step 2. `publish_writes_the_physical_address_into_the_descriptor`
asserts `desc.addr == va_to_pa(va)` and `!= va` — forget the translation and it
fails, the same way a real driver would silently DMA garbage.

**Maps to:** the `addr: crate::mmu::va_to_pa(...)` line in `virtio_console::transmit`.

**Done when:** the two `publish_*` tests pass.

---

## Exercise 2 — `device_poll` (device side)

Drain everything the driver made available. While `used.idx != avail.idx`:

1. Next descriptor id = `avail.ring[used.idx % QSIZE]`.
2. **DMA** the buffer: read `&phys[addr .. addr + len]` — indexing `phys`
   *directly* by `addr` — and append to `transmitted`.
3. Post completion: `used.ring[used.idx % QSIZE] = UsedElem { id, len }`, then
   `used.idx += 1` (wrapping).

Step 2 is where the VA/PA rule bites: a VA in `addr` would index `phys` far out
of range (panic) or at the wrong bytes. With the correct PA from Exercise 1, it
lands exactly on the buffer.

**Maps to:** the device's side of the ring (QEMU does this for real; here you
play the device).

**Done when:** `round_trip_transmits_the_buffer`, `multiple_buffers_transmit_in_order`,
`available_ring_wraps_past_qsize`, the noop test, and the proptest
`published_buffers_arrive_concatenated` all pass.

---

## Stretch goals (no tests — for understanding)

1. **The `TX_STAGING` hazard (the real v0.5 bug).** Add a second VA region —
   a "heap" at a *different* base, say `HEAP_BASE = 0x2_0000_0000`, whose VA does
   **not** equal `pa + KERNEL_OFFSET`. Place a buffer there and `publish` it:
   `va_to_pa` (which only knows `KERNEL_OFFSET`) mistranslates, so the device
   DMAs the wrong physical bytes. Then fix it the way the kernel does — copy the
   heap buffer into a *known kernel-region* staging buffer first, and publish
   that. This is exactly why `virtio_console::send` stages through `TX_STAGING`.
2. **Descriptor chaining.** Use `flags = NEXT` and the `next` field to describe
   one logical buffer as two descriptors, and make `device_poll` follow the
   chain. This is how real drivers scatter-gather.
3. **Notify suppression.** Real rings use `avail.flags` / `used.flags` to skip
   interrupts/notifications when the other side is already polling. Model a
   `notify_needed()` check.

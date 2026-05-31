# Post 4 — Closing the loop

- last post ended with a Hello frame on the wire and `xxd` reading hex out the other end. this post: a proper reader, properly orchestrated, end-to-end with `cargo xtask up` + `cargo xtask reader`.

## TDD the reader

- the obvious testable seam wasn't `connect`/`read_frames`/`print_frame` (all I/O or stdout — annoying to test) but the **byte decoder** in the middle. extracted `try_decode_frame(buf) -> ...`. bytes in, frame out, pure.
- once decode is pure, `read_frames` becomes "loop: read bytes into a buffer, drain frames out of the buffer." the hard part is the pure function; the loop is mechanical.
- first test: encode a Hello, feed bytes to `try_decode_frame`, assert the frame and consumed length come back.
- I wrote try_decode_frame to stay close to the ground with the actual coding; llm wrote most of the rest.

## Option vs Result for the decoder

- first cut returned `Option<(Frame, usize)>`. "None means try later."
- problem: **None silently absorbs everything.** "not enough data" (legitimate, wait for more) and "the bytes don't match the protocol" (real bug, spin forever waiting for impossible bytes) look identical to the caller.
- switched to `Result<(Frame, usize), postcard::Error>`. postcard has a specific `DeserializeUnexpectedEnd` variant for "ran out of bytes mid-frame" — the caller pattern-matches on it and loops, treats anything else as a hard error.
- moral: **make distinguishable failures distinguishable.** the cost is the caller has to handle one extra arm; the win is the kernel can't lock the host-reader into a silent infinite wait.

## the chain idiom

- first impl was let-else, fine but clunky:
  ```rust
  let Some((frame, rest)) = postcard::take_from_bytes(buf).ok() else { return None; };
  Some((frame, buf.len() - rest.len()))
  ```
- prompted myself for cleaner ideas. the chain reads better:
  ```rust
  postcard::take_from_bytes(buf)
      .map(|(frame, rest)| (frame, buf.len() - rest.len()))
  ```
- when failure is just "propagate" with no additional logic, `let-else` is overkill. when failure has a body (log, increment a counter, set a flag), `let-else` shines. picking the right shape per case.

## the extra tests

- two properties worth locking down beyond the happy path:
  - **truncated input → `DeserializeUnexpectedEnd`** (specifically that variant, not any other postcard error). this is the "need more bytes" signal the streaming loop will branch on.
  - **trailing bytes → consumed = exact frame length**, not buf.len(). caller advances exactly past the frame and starts decoding the next one at the right offset.
- both passed against the existing impl immediately (no impl change). TDD doesn't require red — it requires "test first." here the tests exist to prevent regressions, not to drive new code.

## decode_stream: the real loop

- generalized over `R: Read` so tests can pass `Cursor<Vec<u8>>` instead of `UnixStream`.
- took a callback `FnMut(&Frame)` instead of calling `print_frame` directly. tests assert inside the callback; production passes `print_frame`.
- shape:
  ```
  buf = []
  loop:
      drain all frames out of buf (try_decode_frame in a loop)
          Ok(frame, consumed) -> on_frame(&frame); buf.drain(..consumed);
          Err(UnexpectedEnd) -> break inner loop, read more bytes
          Err(other) -> bail with io::Error
      read from stream into buf
      n == 0 -> EOF, return Ok
  ```
- nested loops: outer reads, inner drains. invariant: when the outer loop reads, buf doesn't contain a complete frame.

## the borrow-checker tussle

- `try_decode_frame(&buf)` returns a `Frame<'_>` borrowing from `buf`.
- want to call `on_frame(&frame)` then `buf.drain(..consumed)`. but drain mutates `buf` while the borrow's still alive.
- in older Rust this would have needed an explicit scope to drop the frame first. NLL (non-lexical lifetimes) sees that `frame` isn't used after `on_frame(&frame)` and ends the borrow there. drain works.
- Rust 2024 makes this even more obvious — but it's still worth knowing what happens when NLL doesn't save you.

## partial-read test

- TCP and Unix sockets routinely return short reads. the loop needs to handle "got 3 bytes, frame needs 11, accumulate and read again."
- wrote a `ChunkedReader` test helper — wraps a `Vec<u8>` and hands out `chunk_size` bytes per `read()` call.
- with `chunk_size = 1`, the test forces the decode loop to accumulate one byte at a time. impl passed unchanged.
- moral: write the worst-case test for the loop discipline you're claiming.

## print_frame: the {:?} realization

- first cut was a 40-line manual `match` formatting every variant by hand that the agent wrote. Aligned columns, etc.
- "we can't just use Debug?" answer: yes. `Frame` derives `Debug`; `println!("{frame:?}")` is one line.
- bonus: when we add the 8th variant, no print code changes. the manual match would have rotted.
- moral: **don't write code that derives can write.**

## --pretty (and clap)

- added a `--pretty` flag for multi-line `{frame:#?}` output (the original "useless for kernel consoles" argument from way back, now relevant for ad-hoc inspection).
- first impl: `std::env::args().any(|a| a == "--pretty")`. crude but works.
- then: switched to **clap with derive**. proper `--help`, `--version`, typo detection. one struct + one derive line:
  ```rust
  #[derive(Parser)]
  struct Args { #[arg(long)] pretty: bool }
  ```
- did xtask too. subcommands as a `#[derive(Subcommand)]` enum. the `Reader` variant gets a trailing `Vec<String>` with `trailing_var_arg = true, allow_hyphen_values = true` — clap-idiomatic way to forward args opaquely:
  ```
  cargo xtask reader -- --pretty
  ```

## xtask: the over-engineering I had to back out

- agent got tempted to make `cargo xtask up` spawn BOTH QEMU and host-reader as children, with QEMU's chardev `wait=on` blocking until host-reader connected.
- agent thought it was nice: one command, full pipeline.
- actually a mess: interleaved stdout, QEMU's `-nographic` taking over the terminal, "kill QEMU when host-reader exits" was fragile.
- backed out. clean shape: **two subcommands, two terminals.** `cargo xtask up` runs QEMU; `cargo xtask reader` runs host-reader. they meet on the socket.
- moral: just because you can orchestrate doesn't mean you should. let unix-y things stay unix-y.

## what i learned

- **make failures distinguishable in the type.** Option says "missing"; Result says "and here's why." `postcard::Error::DeserializeUnexpectedEnd` vs "anything else" turned out to be a load-bearing distinction for the loop.
- **don't write what derives can write.** `print_frame`'s 40-line match was busy-work I could have avoided. Debug-derive is the v0.1-appropriate amount of polish for ad-hoc debug output.
- **a callback + a Read impl is the right testing seam** for any "read bytes, do something" loop. tests pass `Cursor` + assertion closures; production passes a real stream + `print_frame`.
- **resist single-command orchestration.** convenient until it isn't. two terminals + `wait=on` is actually the cleaner shape; the chardev's blocking semantics are doing the synchronization for us.
- **NLL is doing real work.** the `frame` borrow → `drain` mutation sequence would have been a compile error a few years ago; now it just works. nice when the borrow checker stops getting in your way without you noticing.

## next

- send a real span tree from the kernel at boot — `kernel.boot` with nested children (`serial_init`, `telemetry_init`, etc.) plus a heartbeat loop. string registration with the StringRegister frame. that's the v0.1 finale.
- then a polish pass (the `probe_all_slots` dead-code warning, the missing `gp` global pointer setup we filed for later, etc.) plus a screen recording, and v0.1 is done.

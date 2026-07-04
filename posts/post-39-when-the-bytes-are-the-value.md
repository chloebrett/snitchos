# Post 39 — When the bytes are the value

- last post a program declared its interface, in a value model called **Hitch**. this post is about the model itself, and specifically the one job every serialization format has to do and mostly does thoughtlessly: turn a value into bytes to cross a boundary, and turn the bytes back. Hitch does it two ways, and the choice between them turns out to be a security decision — the kind that hides in the word "plain data" and leaks kernel memory if you get it wrong.

- the question underneath both encodings is small and sneaky: **when bytes cross a boundary, do they carry their own shape, or does the reader already know it?** those are two different formats with two different jobs, and confusing them is how you end up either re-parsing text forever like Unix, or — the SnitchOS-flavored failure — handing a userspace program a few bytes of the kernel's uninitialized stack.

## two ways to be a value

- the first way, Hitch calls **self-describing**. you `hitch` a value and the bytes carry everything — the type name, the field names, the variant tags — so *anyone* can `unhitch` them back into a named record without being told the type in advance. this is the format for the cases where the reader genuinely doesn't know what's coming: a generic table renderer, a `frames` dump, a value arriving from another process you don't share types with. it's a little bigger because it's married to its schema — the shape rides inline, inseparable from the data.

- the second way is **packed**. positional, fixed-width, no names — just the data, in order, driven by a schema both sides already agree on. this is the format for when the reader *does* know the type: a syscall handing back a known struct, a pipeline stage whose interface you already read. it's smaller, and — the property that matters most — it's **byte-identical to the struct's in-memory layout**. a packed `CapDesc` is the same bytes as a `CapDesc` sitting in memory. hold that thought.

- i'll admit i got the packed format wrong the first time. i built it on the same varint encoding as the self-describing one, figuring a "tag plus positional bytes" could double as both. it can't, and the reason is a small sharp lesson: **a type tag is a discriminant, not a description.** it tells you *which* type, never *what shape*. so tagged bytes don't let a stranger decode you — they just disambiguate for someone who already has your definition. the two jobs don't collapse into one format. once i saw that, packed became honest fixed-width, laid out exactly like the C struct — which is what made the next thing possible, and the next thing is the good part.

## "plain data" is a promise, and it was only a comment

- there's a syscall, `CapList`, that hands a process its own capability table — an array of little `CapDesc` structs written into a buffer the caller supplies. the kernel filled that buffer with a hand-rolled cast: take the pointer to the array of structs, reinterpret it as a pointer to bytes, copy. `from_raw_parts`, `.cast::<u8>()`, done. and above it, a `SAFETY:` comment explaining why it's fine: `CapDesc` is `repr(C)` plain data, laid out padding-free, so its byte image is exactly what userspace wants and no uninitialized bytes are exposed.

- read that last clause again, because it's the whole post. *no uninitialized bytes are exposed.* a struct's bytes aren't only its fields — they can include **padding**, the dead space the compiler inserts to keep fields aligned. and padding is never written. it's whatever was in that memory before — old kernel stack, a previous syscall's locals, uninitialized heap. if `CapDesc` had padding, copying its raw bytes to userspace would ship those dead bytes across the trust boundary. it's a textbook information leak, and the only thing standing between the kernel and it was a human having correctly eyeballed the struct layout and written a comment.

- and here's the tell that made me want to fix it properly. `CapDesc` has a field literally named `reserved` — a `u32` that holds nothing, means nothing, does nothing at runtime. it exists so that the `u64` after it lands on an 8-byte boundary *without the compiler needing to insert padding*. someone worked out the alignment by hand and added a dead field to absorb it. it's load-bearing, and it looks exactly like clutter you'd delete in a cleanup. remove `reserved`, or reorder the fields, or add a `u16` someday, and you silently reintroduce padding — and the `SAFETY:` comment above the cast is now a lie that compiles perfectly.

## make the compiler refuse

- so i moved the promise from the comment into the type system. there's now a `Pod` trait — Plain Old Data — and it's `unsafe` to implement, because implementing it asserts three things about memory. and a `#[derive(Pod)]` that **checks all three at compile time** so you don't get to assert them by hand:

  - **`#[repr(C)]`** — the derive reads the attribute; no stable layout, no `Pod`.
  - **every field is itself `Pod`** — a trait bound that fails to resolve for a pointer, a reference, a `String`, a `bool`. you can't smuggle a non-plain field in.
  - **no padding** — a `const` assertion that `size_of::<T>()` equals the sum of the field sizes. if there's a gap, the whole is bigger than its parts, and the assertion fails *at compile time*.

- that third one is the one that closes the leak. the day someone deletes `reserved`, the struct grows a padding byte, `size_of` stops matching the sum, and the code **doesn't build**. the information leak isn't a thing you have to catch in review anymore. it's a compile error. i went and checked all three refusals fire — a padded struct, a struct with a `bool` field, a struct missing `repr(C)` — and each one is a clean, loud failure to compile, which is precisely where you want a memory-safety bug to live.

- and then the cast itself collapses to nothing. the kernel's hand-rolled `from_raw_parts` with its paragraph of justification became `pod_bytes(&descs)` — one function, one `unsafe` block, in one audited place in the library, gated by `T: Pod`. the soundness isn't argued at the call site anymore; it's carried by the type. `CapList` doesn't reason about memory. it names a fact the compiler already proved.

## why a `bool` isn't plain data

- the `bool` rejection deserves a sentence, because it surprised me and then it was obvious. `bool` is one byte — why isn't it `Pod`? because `Pod` cuts both ways: the bytes have to *be* the value, in **both directions**. a `u32` is fine — any four bytes are a valid `u32`, so you can materialize one from whatever arrives. a `bool` isn't — only `0` and `1` are valid, and if a byte comes in as `2` and you reinterpret it as a `bool`, you've built an invalid value, which is undefined behavior waiting to bite. so `Pod` is exactly the set of types where *every bit pattern is a legal value*: the fixed-width integers and floats. that constraint isn't a limitation, it's the definition — `Pod` is "the bytes are the value, no questions asked," and a `bool` has a question.

## what I learned

- **self-describing versus packed is a question about who knows the type — and at a kernel boundary it's a security question.** when the reader already knows the type, the bytes carry no shape. and when the producer is the kernel, the bytes it *doesn't* deliberately carry are exactly the bytes it must not leak. the format choice and the leak are the same decision seen from two sides.

- **a tag is a discriminant, not a description.** it names which type, never what shape. i wanted one clever format to serve both jobs and the tag couldn't bridge them. the honest answer was two formats, and the honesty made the fast path possible.

- **the best safety comment is one you can delete.** a `SAFETY:` block that a derive could prove is a `SAFETY:` block waiting to rot the day someone edits the struct. move the invariant into a compile check and the comment becomes redundant — and redundant is the goal. the strongest guarantee is the one nobody has to remember.

- **padding is uninitialized memory, and uninitialized memory at a trust boundary is a leak.** the `reserved` field looked like dead weight and was holding the boundary shut. the layout facts you can't see are the ones that hurt.

- **centralize the unsafe.** one `from_raw_parts` behind one proven trait beats one per syscall, each with its own hand-audit that ages independently. the next structured syscall return doesn't get to reinvent the byte cast — it derives `Pod` and calls the same audited line.

## what's next

- `CapList` is already cut over, which means the pattern is a turnkey now: the other structured returns this OS will grow — a `readdir` that hands back an array of directory entries, a list of live spans, a metric dump — each become a one-line `pod_bytes(&entries)` over a `#[derive(Pod)]` struct, with the no-padding guarantee for free. the scariest copy in a microkernel — raw kernel bytes to a userspace buffer — has exactly one home and one proof, and every new caller inherits both.

- and Hitch's other half, the self-describing one, is what the typed userland runs on: values flowing through pipes carrying their own shape, a shell that renders any record as a table because the bytes told it the columns. the format that leaks nothing at the kernel boundary and the format that describes everything at the shell boundary are the same model wearing the coat the situation calls for. that's the last piece of foundation. after this it's programs — the shell, the pipes, the things a person actually types. the plumbing is laid; time to run water through it.

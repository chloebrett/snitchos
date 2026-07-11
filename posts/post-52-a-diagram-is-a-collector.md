# Post 52 — A diagram is a collector

- I wanted pictures of the subsystems — the memory map, the cap tree, how the crates hang together. the obvious move is to open a drawing tool and draw them. but for an OS whose entire pitch is that it *narrates itself* on a wire, hand-drawing felt like cheating, and worse, like a lie with a shelf life: the moment I looked away, the drawing and the system would part ways. so I split the problem, and the split turned out to be the whole idea.

## two kinds of diagram

- a diagram is either a **claim you make** about the system or a **projection of something the system already emits**. the memory map, the boot handoff, the context-switch dance — those are claims: a person decides what matters and draws it, and it drifts slowly, on human time. fine. those stay hand-drawn, one mermaid block per doc, updated when the design moves.

- the interesting ones are the projections. the crate graph is a projection of `cargo metadata`. the scenario matrix is a projection of the itest registry. and the good ones — the ones worth the whole exercise — are projections of the *telemetry the kernel already puts on the wire*. for an observability OS, half the diagrams write themselves, because the facts are already leaving the building.

## the collector I didn't have to write

- the cap derivation tree is the showcase. the kernel emits a `CapEvent` frame every time authority is created or handed off — `cap_id`, `parent_cap_id`, holder, object, rights. the collector already decodes those into Tempo. folding them into a graph instead — parent points to child, root grants at the top — is the *same read, a different write*. I reused `OwnedFrame` verbatim; the "diagram generator" is just another consumer of the frame stream. the system snitches its own structure; I only drew what it said.

- same trick for the span call-graph (`SpanStart` frames, collapsed by name) and the scheduler's hand-off graph (`ContextSwitch` frames, counted). three diagrams, zero new instrumentation. the wire already carried them.

## what the picture told me

- **the least-authority story became visible.** I designed init to hold `RECV|MINT` on its endpoint, hand the client a bare `SEND`, hand the server the full `RECV|MINT`, and let the server mint a fresh `SEND` per connection. I'd *written* that. I had never *seen* it — until the tree drew it, rights and all: `init [RECV|MINT] → fs-client [SEND]` down one branch, the server minting `SEND`s down the other. the diagram didn't teach me the design; it let me *check* it at a glance, which is a different and better thing.

- **my spans are flat.** I expected the trace to be a deep tree. it isn't — it's five top-level spans and one shallow nest (`kernel.boot → console_init`). SnitchOS opens most spans at top level, not under a parent. I'd have told you otherwise before the fold corrected me. so I stopped pretending it was a tree and put the occurrence count on each node (`fs.serve ×13`, `heartbeat ×23`) — a flat profile is honest, and more useful than a fake hierarchy.

- **the crate graph was a hairball** until I grouped it into layers — kernel, shared, userspace, tooling. clustered, the shape reads in a second: everything funnels into the shared protocol/abi. the information was always there; the *layout* was the whole readability.

## the picture that rendered empty

- a good half-day sink: I rendered the hand-drawn diagrams to SVG, rasterized them to look, and got **empty boxes** — every label gone. I nearly "fixed" the diagrams. the diagrams were fine. mermaid emits flowchart labels as `<foreignObject>` HTML, which a browser renders and a plain rasterizer silently drops. I was auditing the pictures through a tool that couldn't see half of each one. render straight to PNG through the real renderer and they're perfect. the lesson is dull and permanent: when the artifact looks broken, suspect the thing you're viewing it with.

## reproducible because deterministic

- the runtime diagrams are snapshots of a boot, which sounds fragile. it isn't, and the reason is a tool from another thread: snemu, the from-scratch interpreter, is deterministic. the same boot folds the same frames into the same tree, byte for byte — I lean on that when I refactor the diagram code and diff the output to prove I changed nothing. (the one exception is the scheduler graph, whose fine cross-hart switch *counts* wobble with timing while the coarse structure holds — an honest reminder of which telemetry is deterministic and which is only mostly.)

## what I learned

- **for an observability OS, a diagram should be a collector, not a drawing.** the truth is already on the wire; the work is folding it, not inventing it. the drawing you make by hand is the exception, reserved for the claims the system can't emit about itself.

- **a projected diagram is a mirror.** it doesn't teach you the design — you wrote the design. it lets you *see* it, and seeing catches the gap between what you wrote and what you meant. the least-authority tree confirmed a design; the flat trace corrected a belief. both were free.

- **split contract from snapshot.** projections of static truth (crates, syscalls, the wire format) are *contracts* — gate them, so a diagram that drifts from the code fails CI. projections of a run are *snapshots* — date them, don't gate them. same machinery, opposite discipline.

## what's next

- the one projection I haven't drawn yet is the most contract-shaped: the wire format itself, `protocol::Frame`, rendered from the source and `--check`ed so the picture of the contract can't drift from the contract. after that the loop closes the other way — pulling the Grafana views out by API instead of by screenshot, so the live metrics join the still ones.

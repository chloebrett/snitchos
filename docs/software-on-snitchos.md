# 📦 Software on SnitchOS — exploration

*Exploration, not commitment. What software could run on SnitchOS, the difficulty ladder, and notes on a few specific targets. Nothing here is scheduled.*

# The governing principle
**Portability cost = how much of a program is OS-boundary touch vs. pure computation.** Computation ports cheaply; every OS-boundary interaction is a porting cost. An editor is maximally OS-boundary; a CLI computation tool is nearly all computation.

Compatibility story (already decided): Rust source portability + WASM, explicitly *not* POSIX/Linux-ABI. C software needs a C toolchain + a ported libc — a separate major effort, not the planned path.

# The difficulty ladder
- **Rung 1 — near-recompile.** Pure computation, stdin/stdout only: JSON formatter, calculator, grep-shaped tools, regex tester, chess engine. Rust staying inside portable `std` → recompile against the SnitchOS `std` port and run. The first userspace programs.
- **Rung 2 — needs the filesystem.** Computation + file I/O by path: static site generator, archiver, log analyzer, embedded DB. Adds the FS dependency, no UI.
- **Rung 3 — needs a terminal.** TUI programs: file manager, system monitor, the Slay the Spire clone's ratatui frontend. The jump is the *prerequisite* — SnitchOS needs a terminal emulator + PTY-equivalent, and the program's terminal-backend crate ported off `std::os::unix`. Build the terminal once, unlock the category.
- **Rung 4 — needs the network.** HTTP client, IRC client, chat server, Gemini browser. The v0.10 metrics workload lives here. Gated on v1.2 networking.
- **Rung 5 — needs the GUI stack.** Paint program, image viewer, GUI card game. Gated on compositor + virtio-gpu + window protocol. Text-rendering monster lives here.
- **Rung 6 — the bosses.** Browser, full IDE, video player, 3D game. Each is an OS's-worth of subsystems. Aspire, do not schedule.

# Categories that fit SnitchOS especially well
- **Self-referential tooling.** Telemetry dashboard, trace explorer, a htop-for-SnitchOS, a capability inspector. Eats the OS's own observability/capability data; doubles as debugging tooling; the screenshots *are* the project showing itself off. The most on-theme software possible. Strong flagship pick: a native observability-explorer app — Rung 3, useful, and no other OS's version reads traces this rich.
- **Servers over clients.** A capability-IPC microkernel is a better host for backend services than interactive apps — no GUI dependency, concurrency model fits. Metrics store, KV store, file server, job queue. Where SnitchOS is genuinely *good*.
- **WASM-delivered software.** Compiling to WASM sidesteps the language problem and rides the runtime being built anyway. Grows on its own as the WASM userspace matures.

SnitchOS's comfort zone: Rungs 1–4 + self-referential tools + servers. A coherent identity, same shape as Plan 9's or a unikernel's sweet spot.

# `std` on SnitchOS
Possible, and a legitimate post-v1.0 goal. `std` is portable by design — it already runs on non-POSIX Windows. Porting means writing a new internal backend (`sys`-layer) that maps `std`'s generic needs onto SnitchOS primitives: `File` → a file capability via the FS service; `thread::spawn` → SnitchOS threads; `Mutex`/channels → notifications + IPC; clock → the monotonic clock. None of this needs POSIX — it needs SnitchOS to *have* files, threads, time, blocking, which it does. Redox has done exactly this.

What does not port: `fork` (no `fork` by design — but `fork` is not in `std`'s public API; `std::process::Command` maps fine) and `std::os::unix` extensions (replaced by a `std::os::snitch` with capability-flavored extensions). The semantic mismatch — `std::fs` thinks in global paths, SnitchOS thinks in capabilities — is a translation layer (paths resolve relative to a filesystem-root capability), works but not free.

The `std` port *is* what makes "Rust source portability" real for non-trivial programs.

# Editors (helix vs neovim)
Neovim is C → needs the C toolchain + libc port; not the path. A Rust modal editor (helix) rides the `std` port — far more portable. But not "recompile": helix needs (a) the `std` port, (b) a terminal emulator on SnitchOS, (c) a PTY-equivalent, (d) its terminal-backend crate ported off `std::os::unix`. A multi-milestone arc, well post-v1.0. A good eventual flagship — getting helix running proves FS + terminal + PTY + processes + `std` port together.

# SSH into SnitchOS
Three layers: (1) **transport** — a network stack that accepts inbound TCP and runs a listening server (v1.2-ish; the metrics workload already pushes toward being a network server). (2) **the SSH protocol** — substantial, heavily cryptographic; do not write it, port `russh` (pure-Rust SSH; needs an async runtime on SnitchOS, leans on the entropy subsystem for key material). (3) **what the session connects to** — normally a PTY + shell, i.e. the whole interactive-terminal stack.

The SnitchOS-flavored opportunity: SSH's connection layer is channel-multiplexed; a "channel" can carry anything, not just a shell. So:

- **Minimum viable SSH-in:** networking + port `russh` + wire the session channel to the existing debug/telemetry REPL. No PTY, no Unix shell. Remote access to the live kernel — a great post-v1.2 milestone.
- **SSH-in to a real shell:** the above + PTY-equivalent + a shell program. Bigger.
- **The interesting framing:** SSH authentication as *capability acquisition* — channels bound to the capabilities your key is authorized for, not an ambient root shell. "Watch capability grants happen as an SSH session authenticates" — a striking demo of both pillars.

# The Gemini protocol
A deliberately tiny internet protocol — a conscious reaction against web complexity, sitting between Gopher and HTTP. TCP + mandatory TLS (used in a stripped-down, trust-on-first-use way, no CA apparatus). The request is *one line* (a URL); the response is one status/metadata line then the body, then the connection closes. Content is "gemtext" — a markup simpler than Markdown: text, three heading levels, list items, blockquotes, preformatted blocks, and link lines (a link must be on its own line; no inline links, no images, no styling, no scripts).

Why it matters here: Gemini is the anti-browser. A Gemini client is open-a-TLS-socket, send-a-line, parse-a-line-oriented-format, render-as-styled-text — a weekend-scale project once SnitchOS has networking + TLS, at ~1% the cost of a web browser. It exercises networking, TLS (and thus the entropy subsystem), and the text/terminal path; produces a great demo; never threatens to become an unbounded sinkhole. The sweet-spot "browse the internet on my OS" feature — all upside.

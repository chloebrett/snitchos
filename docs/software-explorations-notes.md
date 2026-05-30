# 🔎 Software explorations — notes

*Notes from exploration of significant software pieces, viewed through the SnitchOS lens. Companion to the "Software to explore — backlog" page. Not build plans — understanding that informs the design.*

# How QEMU works
QEMU is a **machine emulator** — a normal user-space program that pretends to be an entire computer (CPU, RAM, devices). SnitchOS's "physical RAM" is a buffer inside the QEMU process; its "device registers" are addresses QEMU watches.

**Two ways to run guest code:**

- **TCG (Tiny Code Generator) — emulation by dynamic translation.** Used when guest architecture ≠ host (RISC-V SnitchOS on an aarch64 Mac). QEMU reads a block of guest instructions, translates the whole block to host machine code, and *caches* it — a JIT whose source language is guest machine code. Translation goes guest → architecture-neutral IR → host, so N guests + M hosts need N+M translators, not N×M (the HAL principle).
- **KVM / HVF — virtualization, near-native.** Used when guest architecture = host (aarch64 SnitchOS on an aarch64 Mac). The host CPU runs guest instructions directly via hardware virtualization support (Apple exposes it as the Hypervisor framework / HVF) until the guest does something privileged, which **traps** to QEMU — *trap-and-emulate*, the same trap concept one level up. Privilege levels all the way down: U → S → M → hypervisor.

**Device emulation is always software**, in both modes. A device register address is marked as a special region, not RAM; a guest access there transfers into QEMU's device-model code. SnitchOS's telemetry pipeline rests on this: the telemetry virtio-console is wired (`-chardev socket`) to a Unix socket; the kernel writing a frame "to the device" sends bytes out the socket to the host-reader.

**virtio** is a family of devices *designed to be virtual* — the guest knows it is virtualized and cooperates, using a shared-memory ring-buffer interface (the virtqueue) to minimize traps. The honest fast path for virtual I/O; why SnitchOS uses virtio devices throughout.

Other QEMU pieces touching SnitchOS: QEMU **generates the device tree (DTB)** for the `virt` machine; the **GDB stub** (`-s -S`) works because QEMU *is* the CPU and knows all guest state; **snapshots** (`savevm`/`loadvm`) and **record/replay** (`-icount`) work because the whole machine is serializable data — the foundation for time-travel debugging; a **debug-exit device** lets a guest signal a test result via QEMU's exit code. The emulator being a fiction you can pause/inspect/serialize is itself one of the project's most powerful instruments.

# Devices and drivers
QEMU presents *virtual* devices, so SnitchOS writes **one driver per virtual device** regardless of physical hardware — QEMU translates real host input into the device's standardized format. `virtio-input` covers keyboard/mouse/tablet; one driver, "keyboard" is just a configuration.

Writing a driver (keyboard example): (1) **discovery** — walk the DTB to find the device's MMIO base; (2) **virtio transport** — config registers for a one-time setup handshake (feature negotiation, set up the virtqueue), then the **virtqueue**, a shared-memory ring buffer: the driver posts empty buffers, QEMU fills them with events and raises an interrupt, the driver drains them and recycles. Bulk data moves through shared memory with no traps; traps happen only for the "poke" (interrupt + notification) — *exactly the SnitchOS IPC pattern* (shared memory for data, notification for "something happened"). (3) **input events** are small `{type, code, value}` structs (evdev format) — the device reports physical key up/down, not characters; a keymap turns keycodes + modifier state into text.

On SnitchOS the driver is a **userspace component** holding a capability to the device's MMIO region and a capability to receive its interrupt (delivered as a notification). No ambient hardware access — a compromised keyboard driver cannot touch the disk. Every keystroke can be a traced event.

# Terminals
"Terminal" usually means a **terminal emulator** — software emulating a 1970s physical terminal (the DEC VT100): a dumb screen-and-keyboard device that sent/received bytes over a serial line. Modern emulators still speak VT100 because every command-line program expects it — a frozen interface.

The terminal does two things: **program → screen** (the byte stream is mostly literal characters, but some byte sequences are **ANSI escape codes** — commands: move cursor, set color, clear screen; the terminal is a state-machine interpreter executing them against a grid of character cells) and **keyboard → program** (keys become bytes; special keys become escape sequences too).

The terminal is **not the shell.** The terminal is the dumb screen-and-keyboard transport+display; the shell is a separate program that happens to be connected to it. They are joined by a **PTY (pseudoterminal)** — a kernel object faking the old serial cable: a bidirectional byte channel, one end held by the emulator, one by the shell, so the shell thinks it is talking to a real terminal. The PTY-equivalent is the prerequisite that kept appearing for running editors/SSH on SnitchOS.

**PuTTY** = a terminal emulator + an SSH client bundled. On a normal machine the terminal's channel is a local PTY to a local shell; PuTTY's channel is the *network* to a *remote* shell. Notable mainly because Windows historically shipped no good built-in terminal or SSH client.

**TUIs (ratatui) are escape codes.** A TUI does not do graphics — it emits escape codes; the terminal interprets them into its cell grid. The program is *write-blind* (it cannot read the screen), so a TUI framework keeps its own shadow model of the desired screen, **diffs** it against the previous frame, and emits escape codes only for changed cells — a virtual-DOM-for-text (same render-the-diff pattern as a compositor's damage tracking, and as content-addressed diffing). Consequence for SnitchOS: a TUI app needs almost nothing from the OS — a byte channel out, a byte channel in, and a terminal emulator *somewhere*. SnitchOS already has a serial channel, so the *host's* terminal can be the emulator — TUI apps can run strikingly early, before any native display stack. Text UI is portable to anything that can move bytes.

# Window managers and compositors
A window manager exists because many graphical programs share one framebuffer and someone must arbitrate. **Display server / compositor** = the *mechanism* (owns the framebuffer and input devices, routes input, gets pixels on screen). **Window manager** = the *policy* (where windows go, sizing, focus, tiling vs floating). Classic X11 kept them as separate programs; modern Wayland fuses them into one "compositor."

**Two eras:** X11 did **server-side rendering** — clients sent drawing *commands*, the server drew; this gave free network transparency but a huge protocol nobody ended up using. Wayland does **client-side rendering** — each client renders its *own* window into its *own* buffer and hands finished pixels to the compositor, whose job shrinks to **compositing** (stacking/blending the buffers into the final image). The client buffers are **shared memory** (zero-copy) + a notification for "new frame ready" — *exactly the SnitchOS IPC pattern again*. A SnitchOS compositor is just a privileged userspace component holding the GPU + input capabilities, speaking IPC to clients. Because the compositor mediates all input routing, one window cannot see another's keystrokes — no ambient keylogging; the capability principle in the display layer (X11's lack of this was a notorious hole).

# Frame timing
The display refreshes on its own fixed schedule (~60 Hz). Rendering and scan-out are two independent clocks that do not naturally align — the source of every timing problem.

- **Tearing**: modifying the framebuffer while the display is scanning it out shows parts of two frames with a visible seam. Fix: **double buffering** — display scans the front buffer, compositor draws the back buffer, then they **swap**.
- The swap must happen during **vblank / vsync** (the gap between finishing one scan and starting the next), or it still tears.
- The compositor is then a **periodic soft-real-time task**: each vblank, ~16.67 ms to composite the next frame. Miss the deadline → a **dropped frame** (the old frame shown twice) — the visual analog of an audio XRun.
- **Frame pacing**: even spacing matters more than average throughput — a lumpy 60 fps looks worse than a steady 50. The determinism-vs-speed theme: variance and worst case are what humans perceive.
- **Triple buffering** absorbs a slow frame at the cost of a frame of latency — the latency-vs-smoothness trade (same as audio buffer sizing).
- **Damage tracking**: recomposite only changed regions — render-the-diff again.
- **Variable refresh (VRR)** inverts it: the display waits for the software — a feedback signal replacing a fixed timer (same idea as Nagle-vs-debounce).

Key point: the compositor is the audio real-time problem wearing a graphical hat — it reuses the same scheduler support (bounded latency, priority, no hot-path allocation) and the same observability story (every frame a span; a Grafana frame-time panel with the deadline drawn as a line is the visual twin of audio XRun forensics).

# Databases and query planning
A database answers: how to store data so it can be written durably and found quickly, when it exceeds memory and the machine can crash. Three masters that pull against each other: **durability**, **speed**, **consistency**.

- **Durability** — the **write-ahead log (WAL)**: append a record of the intended change to a sequential log and flush *that* before touching the main structures; replay the log on crash recovery. Same crash-consistency tool as a journaling/log-structured filesystem.
- **Speed** — **indexes**. The **B-tree** dominates: wide nodes sized to a storage block, so the tree is shallow (3–4 block reads vs ~23 pointer-chases for a binary tree). Not a cleverer algorithm — the same algorithm reshaped to respect hardware cost. **LSM-trees** are the write-optimized alternative (buffer in memory, flush sorted batches sequentially, merge in background) — relevant because metrics ingestion is write-heavy.
- **Consistency** — **MVCC** (multi-version concurrency control): never overwrite a row, write a new version; each transaction sees a consistent snapshot; readers never block writers. This is **copy-on-write again** — CoW filesystem snapshots, Git, and MVCC are the same idea (third sighting of the pattern).

**Query planning** is what makes a database a database. SQL is *declarative* (what); the machine needs a *how*. For any non-trivial query, many correct "hows" exist with order-of-magnitude different performance. The planner chooses among access paths (scan vs index — depends on selectivity, hence on the *data*), join order, and join algorithm (nested loop / hash / merge). It depends on **statistics and cardinality estimation** — and the deep truth: the algorithms are sound; the *estimates* are the soft underbelly. Bad production query performance is usually a bad *estimate* (stale stats, skew, correlated predicates assumed independent). A query planner is structurally a **scheduler for data operations** — a system making predictions to schedule work, whose quality is bounded by its information about the future.

For SnitchOS v0.10: the metrics workload is a write-heavy time-series database — favors LSM-style storage, needs a WAL (composes with the already-log-structured/CoW filesystem), is append-mostly and queried by time range (a range scan over time-ordered data, simple indexing), and likely needs *no* query planner at first (one access pattern → one "how"). Start at the narrow end. Every query a span.

# Container runtimes (Docker, and SnitchOS)
**A container is not a lightweight VM.** A VM virtualizes hardware (guest kernel, virtualized CPU/RAM). A container is **just a normal host process**, on the host's kernel, on real hardware — a process the kernel has been told to *lie to*, running in restricted *views* so it believes it has a machine to itself. The "container" is a process plus a bundle of kernel-enforced illusions.

Three Linux kernel features supply the illusions:

- **Namespaces** — the illusion of being alone. A process gets its own private instance of a normally-global resource (PIDs, network stack, mounts, hostname). Other processes are not denied — they are *invisible*, outside the namespace. Answers "what can it *see*."
- **cgroups** — the resource budget. Limits/accounts CPU, memory, I/O for a group of processes. Answers "how much can it *consume*." (Same mechanism systemd uses.)
- **Root filesystem swap** — the container *image* is a packaged userland filesystem; the process is given it via the mount namespace + a `pivot_root`. A container can "be Ubuntu" on a Fedora host because the *kernel* is shared (one host kernel) and only the *userland filesystem* differs — the kernel-isn't-the-OS point made useful.

**Docker** is not the mechanism (the kernel is) — it is the tooling: **images as a content-addressed, layered build/distribution format** (layers identified by content hash — dedup, share base layers, pull only missing layers — the same content-addressed Merkle store as Git and the SnitchOS CoW FS) and the workflow (`build`/`run`/`push`/`pull` + registries). Running is delegated down a standardized stack: Docker → containerd → runc, with OCI specs as the swappable interface in the middle (HAL pattern again).

**Weakness of the Linux model:** a container is a host process sharing the host kernel, so the isolation boundary is the *entire* kernel syscall surface — a kernel bug a container can reach allows escape. Weaker than a VM (small hardware-virtualization boundary). Hence VM-per-container (Kata, Firecracker).

**A container runtime on SnitchOS — the interesting result.** Linux containers are a *retrofit*: namespaces carve private views out of a system that was global by default. SnitchOS inverts the starting point — **no ambient authority, no global namespace**; a process already sees only the capabilities it holds. So **SnitchOS does not need namespaces** — the private-world view that namespaces laboriously construct is SnitchOS's baseline state for every process. A "container" on SnitchOS is just **a process given a specific curated set of capabilities** (a filesystem-subtree capability for its image, capabilities to the services it may use, a resource-limit capability, nothing else). Isolation is not an added feature — it is the absence of capabilities not granted. And the escape weakness *inverts*: a microkernel's trusted core is tiny, so a SnitchOS container's attack surface is the small capability-invocation surface plus explicitly-granted services — closer to VM-grade isolation at container-grade weight, structurally.

What SnitchOS would still need to build: **resource limits** (a cgroup-equivalent — ties to the scheduler's QoS / Borg-style tiers) and an **image format** (worth copying Docker's content-addressed layered model directly — a SnitchOS image is a content-addressed subtree of the Merkle filesystem, so image layers, dedup, and snapshots fall out of the FS design already chosen). Containers are almost a non-feature on SnitchOS — and that is the interesting result.

/**
 * The in-tab emulator as a {@link FrameSource}.
 *
 * The only file that knows `snemu-wasm` exists. Everything above it is written
 * against `FrameSource`, so a socket-, serial- or replay-backed source is a sibling
 * of this file rather than a change to anything that consumes it.
 */

import type { FrameSource, FrameView, Slice, Status } from "./frames";
import init, { Handle } from "./pkg/snemu_wasm.js";
// The wasm is fetched as an asset, not imported as a module: it is several MB, and
// this keeps it a separate file the browser can instantiate by streaming. See the
// `assetsInlineLimit` note in `vite.config.ts`.
import wasmUrl from "./pkg/snemu_wasm_bg.wasm?url";

/** What `cargo xtask web` records about the kernel it staged. */
export interface BuildManifest {
  kernel_bytes: number;
  kernel_fingerprint: string;
  git_rev: string;
}

/** QEMU `virt`'s default RAM. The guest's DTB describes this much, so it must agree. */
export const RAM_BYTES = 128 * 1024 * 1024;

let ready: Promise<void> | undefined;

/** Instantiate the wasm module once, however many sources are created. */
function load(): Promise<void> {
  ready ??= init({ module_or_path: wasmUrl }).then(() => undefined);
  return ready;
}

/** A booted guest, exposed as a pull-based source of console text and frames. */
export class SnemuSource implements FrameSource {
  readonly label: string;
  #handle: Handle;

  private constructor(handle: Handle, label: string) {
    this.#handle = handle;
    this.label = label;
  }

  /**
   * Load `elf` into a fresh machine booting `workload`, or the kernel's default when
   * that is empty. Rejects if it is not a loadable RV64 ELF.
   */
  static async boot(elf: Uint8Array, workload = ""): Promise<SnemuSource> {
    await load();
    const label = workload ? `snemu · ${workload}` : "snemu · default";
    return new SnemuSource(new Handle(elf, RAM_BYTES, workload), label);
  }

  /** Fast-forwards seen at the end of the previous slice. */
  #fastForwards = 0;

  advance(budget: number): Slice {
    const status = JSON.parse(this.#handle.step_budget(BigInt(budget))) as Status;
    const text = this.#handle.drain_uart();
    const frames = JSON.parse(this.#handle.drain_frames()) as FrameView[];

    // Idle means "waited for something during this slice", not "is parked right
    // now". The instantaneous check reads false almost always — idle-skip jumps
    // through the wait and resumes within the slice — and using it measured as 100%
    // of a core, exactly the cost pacing exists to avoid.
    const fastForwards = Number(this.#handle.fast_forwards());
    const idle = fastForwards > this.#fastForwards;
    this.#fastForwards = fastForwards;

    return { status, text, frames, instret: Number(this.#handle.instret()), idle };
  }
}

/**
 * Fetch the staged kernel and the manifest describing it.
 *
 * The manifest is optional: the page is still useful without it, and failing the
 * whole boot because a description was missing would be the wrong trade. A missing
 * *kernel* is fatal, and says how to fix it — that error is the one someone will hit
 * on a fresh clone.
 */
export async function fetchKernel(): Promise<{
  elf: Uint8Array;
  manifest: BuildManifest | null;
}> {
  const [manifest, elf] = await Promise.all([
    fetch("build.json")
      .then((r) => (r.ok ? (r.json() as Promise<BuildManifest>) : null))
      .catch(() => null),
    fetch("kernel.elf").then((r) => {
      if (!r.ok) {
        throw new Error(`kernel.elf: ${r.status} — run \`cargo xtask web\` to stage it`);
      }
      return r.arrayBuffer();
    }),
  ]);
  return { elf: new Uint8Array(elf), manifest };
}

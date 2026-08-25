import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { useEffect, useRef } from "react";

/**
 * The guest's UART, in a real terminal emulator.
 *
 * xterm.js rather than a `<pre>` because `uart_output()` is a *terminal* byte
 * stream: the kernel emits ANSI colour and cursor motion, and the Stitch renderer
 * emits emoji. A `<pre>` shows the escape bytes as garbage.
 *
 * Imperative on purpose. xterm owns its own DOM and its own scrollback, so the React
 * side hands over a container once and then only pushes bytes at it — re-rendering a
 * terminal through React would fight it for control of the same nodes.
 */
export function Console({
  onReady,
}: {
  onReady: (write: (text: string) => void) => void;
}) {
  const host = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = host.current;
    if (!el) return;

    const term = new Terminal({
      theme: { background: "#000000", foreground: "#d6dae0", cursor: "#38bdf8" },
      fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace',
      fontSize: 13,
      scrollback: 10_000,
      // The kernel emits bare `\n`; without this every line stair-steps rightward.
      convertEol: true,
      disableStdin: true,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();

    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(el);

    onReady((text) => term.write(text));

    return () => {
      observer.disconnect();
      term.dispose();
    };
  }, [onReady]);

  return (
    <div
      ref={host}
      data-testid="console"
      className="min-h-0 min-w-0 flex-[1.4] overflow-hidden rounded-md border border-neutral-800 bg-black p-2"
    />
  );
}

/**
 * Turning what a terminal emits into what the guest expects.
 *
 * xterm.js reports a keystroke as the bytes a real terminal would send, and a real
 * terminal sends **carriage return** for Enter. SnitchOS's console expects a
 * **newline** — every itest that drives the REPL sends `b"...\n"`. Without a
 * translation the guest sees a `\r` it does nothing with, and pressing Enter appears
 * to be ignored: the REPL looks broken while being perfectly healthy.
 *
 * That is the entire content of this module, and it is here rather than inline in a
 * keystroke handler because it is a *decision about a protocol* — the kind that is
 * cheap to test and expensive to debug through a browser.
 */

/**
 * Encode one chunk of terminal input for the guest.
 *
 * Everything but the line ending passes through untouched, deliberately: control
 * characters are how a terminal says Ctrl-C, and escape sequences are how it says
 * "arrow key". Neither is ours to interpret — the guest has its own opinions and is
 * entitled to them.
 */
export function encodeInput(data: string): string {
  // `\r\n` first, so a terminal that sends both does not yield two newlines.
  return data.replace(/\r\n|\r/g, "\n");
}

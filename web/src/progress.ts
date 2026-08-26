/**
 * Describing a download in flight.
 *
 * The kernel is a 6.4 MB fetch. On a warm cache that is invisible; on a cold one it
 * is several seconds of a page that says `loading…` and nothing else, which is
 * exactly the first impression a visitor gets.
 */

/** Bytes as a human-readable size, one decimal place. */
function mb(bytes: number): string {
  return `${(bytes / 1e6).toFixed(1)} MB`;
}

/**
 * A label for a download that has received `received` of `total` bytes.
 *
 * `total` is `null` when the server did not say — a `Content-Length` is not
 * guaranteed, and inventing a denominator would mean inventing a percentage.
 *
 * The percentage is clamped, which is not defensive padding: a gzipped response
 * reports the **compressed** length in `Content-Length` while the stream hands back
 * **decompressed** bytes, so `received` legitimately overshoots `total` and an
 * unclamped percentage climbs past 100.
 */
export function progressLabel(received: number, total: number | null): string {
  if (total === null || total <= 0) {
    return `loading kernel — ${mb(received)}`;
  }
  const percent = Math.min(100, Math.round((received / total) * 100));
  return `loading kernel — ${mb(received)} of ${mb(total)} (${percent}%)`;
}

/**
 * Read a response body, reporting progress as it arrives.
 *
 * Streams rather than awaiting `arrayBuffer()`, because the whole point is to say
 * something before the download finishes. Falls back to `arrayBuffer()` when the
 * body is not a readable stream — which is the case in jsdom, and would otherwise
 * make this untestable in the unit suite for no benefit to a real browser.
 */
export async function readWithProgress(
  response: Response,
  onProgress: (received: number, total: number | null) => void,
): Promise<Uint8Array> {
  const header = response.headers.get("content-length");
  const total = header === null ? null : Number.parseInt(header, 10);

  if (!response.body) {
    const buffer = new Uint8Array(await response.arrayBuffer());
    onProgress(buffer.length, total);
    return buffer;
  }

  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let received = 0;

  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    received += value.length;
    onProgress(received, total);
  }

  const out = new Uint8Array(received);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

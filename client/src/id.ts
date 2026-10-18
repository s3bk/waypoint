// Time-ordered UUIDs (version 7). Ids sort by creation time, which is what
// makes the server's short keys (#1, #2, ...) follow creation order.

let lastMs = 0;

export function uuidv7(): string {
  // Never go backwards, so ids from one client always sort in creation order.
  const ms = Math.max(Date.now(), lastMs + 1);
  lastMs = ms;
  const b = crypto.getRandomValues(new Uint8Array(16));
  // 48-bit big-endian millisecond timestamp.
  let t = ms;
  for (let i = 5; i >= 0; i--) {
    b[i] = t % 256;
    t = Math.floor(t / 256);
  }
  b[6] = (b[6] & 0x0f) | 0x70; // version 7
  b[8] = (b[8] & 0x3f) | 0x80; // variant 10
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}

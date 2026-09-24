// What the terminal and the service worker pass on their port (`ssf-term`,
// #491): a port carries JSON, so terminal bytes go as base64.

/// `bytes` (a Uint8Array) as base64.
export function toBase64(bytes) {
  let text = "";
  const CHUNK = 0x8000;
  for (let at = 0; at < bytes.length; at += CHUNK) {
    text += String.fromCharCode(...bytes.subarray(at, at + CHUNK));
  }
  return btoa(text);
}

/// The bytes `text` (base64) stands for.
export function fromBase64(text) {
  const binary = atob(String(text ?? ""));
  const bytes = new Uint8Array(binary.length);
  for (let at = 0; at < binary.length; at += 1) bytes[at] = binary.charCodeAt(at);
  return bytes;
}

/// The factory's resize message for a terminal of `cols` x `rows`, or null
/// for a size it would not take (1 to 1000 each).
export function resizeMessage(cols, rows) {
  const ok = (n) => Number.isInteger(n) && n >= 1 && n <= 1000;
  if (!ok(cols) || !ok(rows)) return null;
  return JSON.stringify({ type: "resize", cols, rows });
}

/// What a page's message to the terminal sends down the socket: the typed
/// bytes of `input`, the JSON text of `resize`, or null -- for anything else,
/// and for both while the factory's Writes switch (`writes`) is off, since a
/// view-only terminal neither types nor resizes the session.
export function termSend(message, writes) {
  if (!writes) return null;
  if (message?.type === "input") return fromBase64(message.data);
  if (message?.type === "resize") return String(message.data);
  return null;
}

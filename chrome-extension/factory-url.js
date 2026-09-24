// The capability URL `ssf-server` logs, shared by the options page and the
// service worker so both agree on what a factory URL is.

/// The factory's capability base URL in the one canonical form the rest of the
/// extension uses (`http://<bind>:<port>/<secret>/`), or null when `value` is
/// not a usable factory URL.
///
/// The capability path is the secret, so a bare origin is refused: it would ask
/// the factory for a path it never serves. A pasted `/api/events` or
/// `/api/status` suffix is dropped rather than doubled.
export function factoryUrl(value) {
  const text = String(value ?? "").trim();
  if (!text) return null;
  let url;
  try {
    url = new URL(text);
  } catch {
    return null;
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  if (url.search || url.hash) return null;
  const path = url.pathname
    .replace(/\/+$/, "")
    .replace(/\/api\/(?:events|status)$/, "");
  if (!path) return null;
  return `${url.origin}${path}/`;
}

/// The `chrome.permissions` origin pattern covering a factory URL.
export function originPattern(url) {
  return `${new URL(url).origin}/*`;
}

/// The factory endpoint under the capability path.
export function endpoint(url, path) {
  return `${url}api/${path}`;
}

/// What to show for a factory the options page has no label for.
export function factoryLabel(url) {
  return new URL(url).host;
}

/// The WebSocket address of a scratch session's terminal (`api/term/<session>`,
/// docs/dashboard.md): `ws://` for an `http://` factory, `wss://` for `https://`.
export function termUrl(url, session) {
  const address = new URL(endpoint(url, `term/${encodeURIComponent(String(session))}`));
  address.protocol = address.protocol === "https:" ? "wss:" : "ws:";
  return address.href;
}

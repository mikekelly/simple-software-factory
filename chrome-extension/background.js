// Live factory snapshots for the content script.
//
// Each configured factory is one EventSource on the capability URL
// `ssf-server` logs. That endpoint sends the canonical dashboard snapshot as an
// `event: status` frame on connect and on every change, an `event: error` frame
// when a snapshot cannot be read, and a keepalive comment while nothing
// changes (see docs/dashboard.md), so a healthy-but-idle factory and a broken
// one are distinguishable: a stream that stops delivering is stale, never
// silently empty.
//
// A service worker sleeps. The content script holds a port open and pings it,
// which keeps this worker awake while a github.com tab is open; if the worker
// is stopped anyway, the next port event starts it again and the module body
// below rebuilds every stream. Nothing here depends on running continuously.
import { endpoint, factoryUrl, factoryLabel, originPattern } from "./factory-url.js";

const BACKOFF_MIN_MS = 1000;
const BACKOFF_MAX_MS = 60000;
// Three missed 25s keepalives: the stream is gone even though nothing errored.
const STALE_AFTER_MS = 90000;

/// url -> entry, one per configured factory.
const factories = new Map();
/// Open ports from content scripts.
const ports = new Set();

/// `{label, url, source, timer, attempts, snapshot, error, lastFrameAt}`
function newEntry(url, label) {
  return {
    url,
    label,
    source: null,
    timer: null,
    attempts: 0,
    snapshot: null,
    error: null,
    lastFrameAt: 0,
  };
}

/// Whether the factory answered recently, or why it did not.
function state(entry, now = Date.now()) {
  if (!entry.snapshot) return entry.error ? "error" : "connecting";
  if (entry.error || now - entry.lastFrameAt > STALE_AFTER_MS) return "stale";
  return "live";
}

/// The merged snapshot every content script gets: each factory's own state so
/// one unreachable factory cannot make the others look unavailable.
function payload() {
  const now = Date.now();
  return {
    factories: [...factories.values()].map((entry) => ({
      label: entry.label,
      url: entry.url,
      state: state(entry, now),
      error: entry.error,
      lastFrameAt: entry.lastFrameAt || null,
      refreshedAt: entry.snapshot?.refreshed_at ?? null,
      warning: entry.snapshot?.warning ?? null,
      cards: entry.snapshot?.cards ?? [],
      monitoredItems: entry.snapshot?.monitored_items ?? [],
    })),
  };
}

function send(port) {
  try {
    port.postMessage({ type: "snapshot", payload: payload() });
  } catch {
    ports.delete(port);
  }
}

function broadcast() {
  for (const port of ports) send(port);
}

function stop(entry) {
  entry.source?.close();
  entry.source = null;
  if (entry.timer !== null) clearTimeout(entry.timer);
  entry.timer = null;
}

async function open(entry) {
  stop(entry);
  const source = new EventSource(endpoint(entry.url, "events"));
  entry.source = source;
  source.addEventListener("status", (event) => {
    try {
      entry.snapshot = JSON.parse(event.data);
      entry.lastFrameAt = Date.now();
      entry.attempts = 0;
      entry.error = null;
    } catch (error) {
      entry.error = `the factory sent a snapshot that is not JSON (${error})`;
    }
    broadcast();
  });
  source.addEventListener("error", async (event) => {
    // A server `event: error` frame carries data and leaves the stream open; a
    // transport failure has none and ends this connection.
    if (typeof event.data === "string" && event.data) {
      entry.error = errorText(event.data);
      broadcast();
      return;
    }
    const granted = await chrome.permissions.contains({
      origins: [originPattern(entry.url)],
    });
    entry.error = granted
      ? "the factory stopped sending events (is ssf-server running, and is this device on the tailnet?)"
      : "this factory is not allowed yet; grant its address on the extension's options page";
    retry(entry);
  });
}

function retry(entry) {
  stop(entry);
  entry.attempts += 1;
  const delay = Math.min(BACKOFF_MIN_MS * 2 ** (entry.attempts - 1), BACKOFF_MAX_MS);
  const wait = delay + Math.random() * delay * 0.25;
  entry.timer = setTimeout(() => {
    entry.timer = null;
    open(entry);
  }, wait);
  broadcast();
}

/// The `{"error": ...}` body of a server error frame, as plain text.
function errorText(data) {
  try {
    const parsed = JSON.parse(data);
    const detail = String(parsed?.error ?? "").trim();
    if (detail) return detail;
  } catch {
    // Not JSON; the raw text is the best description available.
  }
  return String(data).slice(0, 300);
}

/// Bring the running streams in line with the stored factory list. Called on
/// every worker start and whenever the options page saves.
async function configure() {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  const wanted = new Map();
  for (const item of stored) {
    const url = factoryUrl(item?.url);
    if (!url) continue;
    wanted.set(url, { label: String(item?.label ?? "").trim() || factoryLabel(url) });
  }
  for (const [url, entry] of factories) {
    if (wanted.has(url)) continue;
    stop(entry);
    factories.delete(url);
  }
  for (const [url, wanted_] of wanted) {
    const entry = factories.get(url);
    if (entry) {
      entry.label = wanted_.label;
      continue;
    }
    const fresh = newEntry(url, wanted_.label);
    factories.set(url, fresh);
    open(fresh);
  }
  broadcast();
}

chrome.runtime.onInstalled.addListener(configure);
chrome.runtime.onStartup.addListener(configure);
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.factories) configure();
});

chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "ssf-overlay") return;
  ports.add(port);
  port.onDisconnect.addListener(() => ports.delete(port));
  port.onMessage.addListener((message) => {
    if (message?.type === "ping") send(port);
  });
  send(port);
});

chrome.runtime.onMessage.addListener((message, _sender, respond) => {
  if (message?.type !== "ssf:snapshot") return false;
  respond(payload());
  return false;
});

// A factory the user has just allowed can be read at once, rather than after
// the backoff of its failed attempts runs out.
chrome.permissions.onAdded.addListener((added) => {
  for (const entry of factories.values()) {
    if (entry.timer === null) continue;
    if (!added.origins?.includes(originPattern(entry.url))) continue;
    clearTimeout(entry.timer);
    entry.timer = null;
    entry.attempts = 0;
    open(entry);
  }
});

chrome.action.onClicked.addListener(() => chrome.runtime.openOptionsPage());

configure();

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
//
// This worker also carries every write. A factory accepts one only from an
// extension origin (docs/dashboard.md), and a content script running on
// github.com has none, so `api/assign` and the assign form's listings are sent
// from here; the same rule is why the read streams live here too.
import { endpoint, factoryUrl, factoryLabel, originPattern } from "./factory-url.js";

const BACKOFF_MIN_MS = 1000;
const BACKOFF_MAX_MS = 60000;
// Three missed 25s keepalives: the stream is gone even though nothing errored.
const STALE_AFTER_MS = 90000;
// A write and a listing are answered or given up on. The factory's own
// deadlines are narrower than this -- a minute for a write, thirty seconds for
// a listing, docs/dashboard.md -- so this only stops the form waiting on a
// connection that was accepted and then went quiet, and never reports a
// factory's own slow answer as a failure.
const REQUEST_TIMEOUT_MS = 90000;

/// url -> entry, one per configured factory.
const factories = new Map();
/// Open ports from content scripts.
const ports = new Set();

/// `{label, url, writes, source, timer, attempts, snapshot, error, lastFrameAt}`
function newEntry(url, label) {
  return {
    url,
    label,
    /// The options page's per-factory switch. Off means no write goes out and
    /// the content script draws no form.
    writes: true,
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
      writes: entry.writes,
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

/// The error text out of a response body, in the words the server used.
function errorBody(parsed, text, status) {
  const detail = String(parsed?.error ?? "").trim();
  if (detail) return detail;
  const raw = String(text ?? "").trim();
  if (raw) return raw.slice(0, 300);
  return `the factory answered ${status} with no error text`;
}

/// Why a request did not get an answer: a factory that is not there at all, and
/// one that took the request and went quiet are different problems.
function reachError(error, entry) {
  if (error?.name === "TimeoutError") {
    return `the factory did not answer within ${REQUEST_TIMEOUT_MS / 1000} seconds (${entry.url})`;
  }
  return `could not reach the factory (${error})`;
}

/// A write to one factory. Sent from here and never from a content script: the
/// server accepts a write only from an extension origin, and a request from a
/// github.com page would carry `https://github.com` and be refused.
///
/// One request, no retry. A refused assign belongs to the person who asked for
/// it, who sees the server's own words and decides whether to repeat it.
async function post(entry, path, body) {
  let response;
  try {
    response = await fetch(endpoint(entry.url, path), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
  } catch (error) {
    return { ok: false, error: reachError(error, entry) };
  }
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    // Not JSON; the raw text is the best description available.
  }
  if (!response.ok) {
    return { ok: false, error: errorBody(parsed, text, response.status) };
  }
  return { ok: true, result: parsed ?? text };
}

/// A read of one of a factory's own listings, for the assign form's pickers.
/// Read from here for the same reason writes are sent from here.
async function get(entry, path) {
  let response;
  try {
    response = await fetch(endpoint(entry.url, path), {
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
  } catch (error) {
    return { ok: false, error: reachError(error, entry) };
  }
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    // Not JSON; the raw text is the best description available.
  }
  if (!response.ok) {
    return { ok: false, error: errorBody(parsed, text, response.status) };
  }
  return { ok: true, body: parsed ?? text };
}

/// `api/assign` for the item the content script names: the same write `ssf
/// assign` makes, with the factory chosen by the caller.
async function assign(message) {
  const entry = factories.get(message.url);
  if (!entry) return { ok: false, error: "that factory is no longer configured" };
  if (!entry.writes) {
    return {
      ok: false,
      error: "writes are turned off for this factory on the extension's options page",
    };
  }
  const body = {
    repo: message.repo,
    number: message.number,
    harness: message.harness,
  };
  // Model and effort are the factory's own defaults when left out, so an
  // unset picker sends no key at all rather than an empty one.
  if (message.model) body.model = message.model;
  if (message.effort) body.effort = message.effort;
  return post(entry, "assign", body);
}

async function listing(message, path) {
  const entry = factories.get(message.url);
  if (!entry) return { ok: false, error: "that factory is no longer configured" };
  return get(entry, path);
}

/// What the content script may ask this worker to do to a factory. Nothing
/// else is routed, and the content script holds no factory fetch of its own.
const HANDLERS = {
  "ssf:assign": assign,
  "ssf:agents": (message) => listing(message, "agents"),
  "ssf:models": (message) =>
    listing(message, `models/${encodeURIComponent(message.harness)}`),
};

/// Bring the running streams in line with the stored factory list. Called on
/// every worker start and whenever the options page saves.
async function configure() {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  const wanted = new Map();
  for (const item of stored) {
    const url = factoryUrl(item?.url);
    if (!url) continue;
    wanted.set(url, {
      label: String(item?.label ?? "").trim() || factoryLabel(url),
      // Absent means on: the switch is a deliberate refusal, not a default.
      writes: item?.writes !== false,
    });
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
      entry.writes = wanted_.writes;
      continue;
    }
    const fresh = newEntry(url, wanted_.label);
    fresh.writes = wanted_.writes;
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
  const handler = HANDLERS[message?.type];
  if (handler) {
    handler(message).then(respond, (error) => {
      respond({ ok: false, error: String(error) });
    });
    // Kept open for the async reply above.
    return true;
  }
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

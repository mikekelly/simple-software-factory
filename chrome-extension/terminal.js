// The pane mirror (#414): one session's agent pane, drawn with xterm.js and
// typed into from the keyboard.
//
// The factory reads the pane's visible screen a few times a second while
// someone watches it and sends a frame only when it changed
// (`api/pane/<session>`, docs/dashboard.md); each frame is the whole screen,
// drawn over the last one. What is typed goes to the service worker, which
// sends it as the write `api/pane/input` -- a write like assign, so the
// factory's Writes switch applies to it -- one request at a time, in order.
//
// This page is the extension's own, opened by the worker in a tab of its own,
// so the stream it reads carries the extension's origin and the permission the
// options page granted for the factory.
import { Terminal } from "./vendor/xterm/xterm.mjs";
import { endpoint, factoryUrl } from "./factory-url.js";

/// The body bound of a write is 4096 bytes and a control character is six
/// once it is JSON, so typed text goes in pieces well under it.
const CHUNK = 500;

const params = new URLSearchParams(location.search);
const url = factoryUrl(params.get("factory"));
const session = String(params.get("session") ?? "");
const stateLine = document.getElementById("state");
document.getElementById("session").textContent = session;
document.title = `${session} · ssf`;

function say(text, problem = false) {
  stateLine.textContent = text;
  stateLine.dataset.problem = String(problem);
}

/// The width a line takes on screen, its escape sequences aside.
function visibleWidth(line) {
  const bare = line
    .replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, "")
    .replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "")
    .replace(/\x1b[@-_]/g, "");
  return [...bare].length;
}

async function start() {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  // Only a factory the options page holds is read: this page takes its
  // factory from its own address.
  if (!url || !session || !stored.some((item) => factoryUrl(item?.url) === url)) {
    say("this terminal names no configured factory or no session", true);
    return;
  }
  const term = new Terminal({
    cols: 100,
    rows: 30,
    scrollback: 0,
    cursorBlink: false,
    fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
    fontSize: 13,
  });
  term.open(document.getElementById("screen"));
  term.focus();

  /// Draw one frame over the last: home, every line with the rest of it
  /// cleared, and everything below the last line cleared. The terminal takes
  /// the pane's own size, growing to its widest line.
  function draw(screen) {
    const lines = screen.replace(/\r?\n$/, "").split(/\r?\n/);
    const cols = Math.max(term.cols, ...lines.map(visibleWidth));
    const rows = Math.max(1, lines.length);
    if (cols !== term.cols || rows !== term.rows) term.resize(cols, rows);
    term.write(
      "\x1b[?25l\x1b[H" + lines.map((line) => `${line}\x1b[0m\x1b[K`).join("\r\n") + "\x1b[0m\x1b[J",
    );
  }

  const source = new EventSource(endpoint(url, `pane/${encodeURIComponent(session)}`));
  source.addEventListener("screen", (event) => {
    try {
      draw(JSON.parse(event.data).screen ?? "");
      say("live");
    } catch (error) {
      say(`the factory sent a frame that could not be read (${error})`, true);
    }
  });
  source.addEventListener("error", (event) => {
    if (typeof event.data === "string" && event.data) {
      let detail = event.data;
      try {
        detail = JSON.parse(event.data).error ?? detail;
      } catch {
        // Not JSON; the raw text is the best description available.
      }
      say(detail, true);
      return;
    }
    // EventSource tries again by itself.
    say("the stream stopped; reconnecting…", true);
  });

  // What is typed, in order: one request in the air at a time, and whatever
  // was typed meanwhile goes in the next.
  let queued = "";
  let sending = false;
  async function flush() {
    if (sending || !queued) return;
    sending = true;
    while (queued) {
      const text = queued.slice(0, CHUNK);
      queued = queued.slice(CHUNK);
      let reply;
      try {
        reply = await chrome.runtime.sendMessage({ type: "ssf:pane-input", url, session, text });
      } catch (error) {
        reply = { ok: false, error: `the extension's service worker did not answer (${error})` };
      }
      if (!reply?.ok) {
        say(`not typed: ${reply?.error ?? "the factory did not answer"}`, true);
        queued = "";
      }
    }
    sending = false;
  }
  term.onData((data) => {
    queued += data;
    flush();
  });
}

start().catch((error) => say(String(error), true));

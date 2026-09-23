// The pane mirror (#414): one session's agent pane, drawn with xterm.js and
// typed into from the keyboard. Only a scratch session (`owner/repo~id`) takes
// typing: an item's agent is spoken to by commenting on the item (#439), so
// its pane is shown and nothing typed is sent.
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

/// Failures in a row, with no frame between them, before the page stops
/// asking: a reader that cannot start is not asked again every few seconds.
const MAX_FAILURES = 3;

/// The next piece of `text` to send: at most CHUNK characters, never ending
/// inside an escape sequence, which the pane would read as two keys.
function nextChunk(text) {
  if (text.length <= CHUNK) return text;
  const esc = text.lastIndexOf("\x1b", CHUNK - 1);
  // An escape sequence a terminal sends is short; one that began within
  // the last 32 characters may run past the cut, so the cut goes before it.
  return esc > 0 && CHUNK - esc < 32 ? text.slice(0, esc) : text.slice(0, CHUNK);
}

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

  const typing = /^[^/]+\/[^/~#]+~[^/~#]+$/.test(session);
  const reconnect = document.getElementById("reconnect");
  let source = null;
  let failures = 0;

  /// Stop reading: the factory is not asked again until the person says so.
  function stop(text) {
    source?.close();
    source = null;
    say(text, true);
    reconnect.hidden = false;
  }

  function connect() {
    reconnect.hidden = true;
    failures = 0;
    say("connecting…");
    source = new EventSource(endpoint(url, `pane/${encodeURIComponent(session)}`));
    source.addEventListener("screen", (event) => {
      failures = 0;
      try {
        draw(JSON.parse(event.data).screen ?? "");
        say(typing ? "live" : "live · view only: comment on the item to speak to its agent");
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
        // The factory's reader has stopped; reconnecting would start it
        // again only to hear the same.
        stop(detail);
        return;
      }
      if (!source) return;
      failures += 1;
      if (failures >= MAX_FAILURES) {
        stop("the stream stopped");
        return;
      }
      // EventSource tries again by itself.
      say("the stream stopped; reconnecting…", true);
    });
  }
  reconnect.addEventListener("click", connect);
  connect();

  if (!typing) return;

  // What is typed, in order: one request in the air at a time, and whatever
  // was typed meanwhile goes in the next.
  let queued = "";
  let sending = false;
  async function flush() {
    if (sending || !queued) return;
    sending = true;
    while (queued) {
      const text = nextChunk(queued);
      queued = queued.slice(text.length);
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

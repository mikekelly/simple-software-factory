// The pane mirror (#414): one session's agent pane, drawn with xterm.js and
// typed into from the keyboard where the factory allows it: the snapshot's
// `pane_input` for the session, passed in the page address. A scratch session
// always takes typing; an item's only where the factory's `item_pane_input`
// is on (#439: its agent is otherwise spoken to by commenting on the item).
// The factory enforces the same rule on every request.
//
// The factory reads the pane's visible screen a few times a second while
// someone watches it and sends a frame only when it changed
// (`api/pane/<session>`, docs/dashboard.md); each frame is the whole screen,
// drawn over the last one. Every few seconds it also sends the history above
// the screen, which is kept as the terminal's scrollback: the wheel scrolls
// back through it, never typing into the pane. The whole pane is fitted into
// the window, its font made smaller where it has to be, so none of it is cut
// off. What is typed goes to the service worker, which
// sends it as the write `api/pane/input` -- a write like assign, so the
// factory's Writes switch applies to it -- one request at a time, in order.
//
// This page is the extension's own, framed over the GitHub page by Open
// (pane-overlay.js, #477), and it talks to no factory itself: a frame under
// github.com is where Chrome's local-network rules can hold a request to a
// factory on a private or tailnet address. The service worker reads the stream
// with the extension's permission and passes it on a port (`ssf-pane`), as it
// sends what is typed.
import { Terminal } from "./vendor/xterm/xterm.mjs";
import { factoryUrl } from "./factory-url.js";

/// The body bound of a write is 4096 bytes and a control character is six
/// once it is JSON, so typed text goes in pieces well under it.
const CHUNK = 500;

/// Failures in a row, with no frame between them, before the page stops
/// asking: a reader that cannot start is not asked again every few seconds.
const MAX_FAILURES = 3;

/// How long a stream that failed waits before it is asked for again.
const RETRY_MS = 3000;

/// The port carries the stream, and a port that is used is what keeps the
/// service worker holding it awake: the frames it sends do not.
const PING_MS = 20000;

/// The terminal's scrollback, in rows: the factory sends at most 1000 rows
/// of history, the screen's own among them.
const SCROLLBACK = 1000;

/// The font size the pane is drawn at when it fits, and the smallest it is
/// made to fit: below that a pane is cut off rather than made unreadable.
const MAX_FONT = 13;
const MIN_FONT = 5;

/// Room kept at the right for the terminal's scrollbar, in pixels.
const SCROLLBAR = 14;

/// The next piece of `text` to send: at most CHUNK characters, never ending
/// inside an escape sequence, which the pane would read as two keys.
function nextChunk(text) {
  // Enter goes on its own: a harness reads a `\r` that arrives in the same
  // write as text before it as part of a paste, and does not submit. A
  // bracketed paste is kept whole, since its newlines are meant as text.
  if (!text.startsWith("\x1b[200~")) {
    const enter = text.indexOf("\r");
    if (enter === 0) return "\r";
    if (enter > 0) text = text.slice(0, enter);
  } else {
    const end = text.indexOf("\x1b[201~");
    if (end >= 0) text = text.slice(0, end + 6);
  }
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

// Esc closes the overlay this page is framed in, whose own keys the frame keeps
// from it -- except in a pane that takes typing, where Esc is a key the agent
// reads, and the overlay's close button is the way out.
if (params.get("input") !== "1" && window.parent !== window) {
  addEventListener(
    "keydown",
    (event) => {
      if (event.key !== "Escape") return;
      window.parent.postMessage({ type: "ssf:pane-close" }, "https://github.com");
    },
    true,
  );
}

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
    scrollback: SCROLLBACK,
    cursorBlink: false,
    fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace',
    fontSize: MAX_FONT,
  });
  const box = document.getElementById("screen");
  term.open(box);
  term.focus();

  /// The largest font size, up to MAX_FONT, at which the whole pane --
  /// every one of its columns and rows -- fits in the window. A cell does not
  /// grow exactly with its font, so the first guess is stepped down until
  /// it fits.
  function fit() {
    const drawn = term.element?.querySelector(".xterm-screen");
    if (!drawn?.offsetWidth || !drawn.offsetHeight) return;
    const style = getComputedStyle(box);
    const width =
      box.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight) - SCROLLBAR;
    const height = box.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
    if (width <= 0 || height <= 0) return;
    const now = term.options.fontSize;
    const scale = Math.min(width / drawn.offsetWidth, height / drawn.offsetHeight);
    let size = Math.max(MIN_FONT, Math.min(MAX_FONT, Math.floor(now * scale * 2) / 2));
    if (size !== now) term.options.fontSize = size;
    while (size > MIN_FONT && (drawn.offsetWidth > width || drawn.offsetHeight > height)) {
      size -= 0.5;
      term.options.fontSize = size;
    }
  }
  new ResizeObserver(fit).observe(box);

  // What the pane shows, as lines: the history above the screen, and the
  // screen. A history that has not been drawn waits while the person is
  // scrolled back through the last one, so what they read does not move
  // under them, and is drawn once they are back at the bottom.
  let history = [];
  let screen = [];
  let stale = false;
  const lines = (text) => (text ? text.replace(/\r?\n$/, "").split(/\r?\n/) : []);
  const atBottom = () => term.buffer.active.viewportY >= term.buffer.active.baseY;

  /// Draw the history and the screen afresh: a reset, then every line, the
  /// history's scrolling off the top into the scrollback.
  function redraw() {
    stale = false;
    term.write(
      "\x1bc\x1b[?25l" + [...history, ...screen].map((line) => `${line}\x1b[0m`).join("\r\n"),
    );
  }

  /// Draw one screen over the last: home, every line with the rest of it
  /// cleared, and everything below the last line cleared. The scrollback is
  /// left as it is, and so is the view of someone scrolled back through it.
  /// The terminal takes the pane's own size, growing to its widest line.
  function draw() {
    const cols = Math.max(term.cols, ...screen.map(visibleWidth));
    const rows = Math.max(1, screen.length);
    if (cols !== term.cols || rows !== term.rows) {
      term.resize(cols, rows);
      fit();
      // A resize moves rows between the screen and the scrollback.
      stale = true;
    }
    if (stale && atBottom()) {
      redraw();
      return;
    }
    term.write(
      "\x1b[?25l\x1b[H" + screen.map((line) => `${line}\x1b[0m\x1b[K`).join("\r\n") + "\x1b[0m\x1b[J",
    );
  }
  term.onScroll(() => {
    if (stale && atBottom()) redraw();
  });

  const typing = params.get("input") === "1";
  // An item's agent has somewhere else to be spoken to; a scratch session's
  // pane is view-only only where this factory takes no writes.
  const viewOnly = session.includes("~")
    ? "live · view only"
    : "live · view only: comment on the item to speak to its agent";
  const reconnect = document.getElementById("reconnect");
  let port = null;
  let retry = null;
  let failures = 0;

  function hangUp() {
    clearTimeout(retry);
    const held = port;
    port = null;
    held?.disconnect();
  }

  /// Stop reading: the factory is not asked again until the person says so.
  function stop(text) {
    hangUp();
    say(text, true);
    reconnect.hidden = false;
  }

  /// A read that failed or ended, asked for again a few times before the page
  /// gives up and offers Reconnect.
  function dropped(error) {
    failures += 1;
    if (failures >= MAX_FAILURES) {
      stop(error ? `the stream stopped (${error})` : "the stream stopped");
      return;
    }
    say("the stream stopped; reconnecting…", true);
    retry = setTimeout(watch, RETRY_MS);
  }

  function heard(message) {
    if (message?.type === "screen" || message?.type === "history") {
      failures = 0;
      try {
        const frame = JSON.parse(message.data);
        if (message.type === "history") {
          history = lines(frame.history ?? "");
          stale = true;
          // It comes before its screen; one that changed on its own is
          // drawn with the screen already shown.
          if (screen.length && atBottom()) redraw();
        } else {
          screen = lines(frame.screen ?? "");
          draw();
          say(typing ? "live" : viewOnly);
        }
      } catch (error) {
        say(`the factory sent a frame that could not be read (${error})`, true);
      }
    } else if (message?.type === "refused") {
      // The factory said no, or its reader has stopped; asking again would
      // only hear the same.
      stop(String(message.error ?? "the factory refused the stream"));
    } else if (message?.type === "dropped") {
      dropped(message.error);
    }
  }

  /// Ask the service worker for the stream, on a port of its own: a worker
  /// that is stopped anyway takes the port with it, which is a dropped read.
  function watch() {
    if (!port) {
      const opened = chrome.runtime.connect({ name: "ssf-pane" });
      port = opened;
      opened.onMessage.addListener(heard);
      opened.onDisconnect.addListener(() => {
        if (port !== opened) return;
        port = null;
        dropped("the extension's service worker stopped");
      });
    }
    port.postMessage({ type: "watch", url, session });
  }

  function connect() {
    hangUp();
    reconnect.hidden = true;
    failures = 0;
    say("connecting…");
    watch();
  }
  reconnect.addEventListener("click", connect);
  connect();
  setInterval(() => {
    try {
      port?.postMessage({ type: "ping" });
    } catch {
      // The port is gone; its disconnect has already been heard.
    }
  }, PING_MS);

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

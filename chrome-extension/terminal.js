// The terminal page: a scratch session's live terminal (term-xterm.js, #491),
// or else the pane mirror.
//
// The pane mirror (#414): one item session's agent pane, drawn as styled text
// (pane-render.js, after collie) rather than by a terminal emulator, and typed
// into where the factory allows it: the snapshot's `pane_input` for the
// session, passed in the page address: an item's only where the factory's
// `item_pane_input` is on (#439: its agent is otherwise spoken to by
// commenting on the item). The factory enforces the
// same rule on every request.
//
// The factory reads the pane's visible screen a few times a second while
// someone watches it and sends a frame only when it changed
// (`api/pane/<session>`, docs/dashboard.md); each frame is the whole screen,
// drawn over the last one. Every few seconds it also sends the history above
// the screen, drawn above it in the same scroller: the wheel scrolls back
// through it. The text is drawn at a fixed size and wraps at the panel's
// width, so the pane is never shrunk to fit and never resized.
//
// Typing is a mode the person turns on (Type) and that turns itself off when
// the picture stops being one they are watching: the tab hidden, the stream
// stopped, a write refused, or half an hour with nothing done here. What is
// typed goes a keystroke at a time as herdr key names (pane-keys.js), and a
// paste as text, to the service worker, which sends it as the write
// `api/pane/input` -- a write like assign, so the factory's Writes switch
// applies to it -- one request at a time, in order.
//
// This page is the extension's own, framed over the GitHub page by Open
// (pane-overlay.js, #477), and it talks to no factory itself: a frame under
// github.com is where Chrome's local-network rules can hold a request to a
// factory on a private or tailnet address. The service worker reads the stream
// with the extension's permission and passes it on a port (`ssf-pane`), as it
// sends what is typed.
import { factoryLabel, factoryUrl } from "./factory-url.js";
import { render } from "./pane-render.js";
import { fitsWrite, keyForInputType, keyForKeyDown, pasteBody, textToKeys } from "./pane-keys.js";

/// The body bound of a write is 4096 bytes: typed keys go in batches of at
/// most this many keys. A paste is one write, or none (pasteBody).
const MAX_KEYS = 200;

/// Failures in a row, with no frame between them, before the page stops
/// asking: a reader that cannot start is not asked again every few seconds.
const MAX_FAILURES = 3;

/// How long a stream that failed waits before it is asked for again.
const RETRY_MS = 3000;

/// The port carries the stream, and a port that is used is what keeps the
/// service worker holding it awake: the frames it sends do not.
const PING_MS = 20000;

/// How long typing stays on with nothing done in the terminal: collie's idle
/// pause.
const IDLE_MS = 30 * 60 * 1000;

/// How close to the bottom, in pixels, still counts as at the bottom.
const BOTTOM_SLACK = 4;

const params = new URLSearchParams(location.search);
const url = factoryUrl(params.get("factory"));
const session = String(params.get("session") ?? "");
const takesInput = params.get("input") === "1";
const stateLine = document.getElementById("state");
const typeButton = document.getElementById("type");
const field = document.getElementById("keys");
const scroller = document.getElementById("scroller");
const historyBox = document.getElementById("history");
const screenBox = document.getElementById("screen");
document.getElementById("session").textContent = session;
// Framed in a floating window, whose title bar already names the session.
document.getElementById("session").hidden = window.top !== window;
document.title = `${session} · ssf`;

/// Whether what is typed goes to the pane: Type is on.
let typing = false;

function say(text, problem = false) {
  stateLine.textContent = text;
  stateLine.dataset.problem = String(problem);
}

// The view follows the live screen while it is at the bottom, and stays where
// the person put it once they scroll up.
let pinned = true;
/// Where following last put the view: the scroll event that says so is not
/// the person scrolling, even if the layout moved the bottom meanwhile.
let followed = -1;
const atBottom = () =>
  scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= BOTTOM_SLACK;
function follow() {
  if (!pinned) return;
  scroller.scrollTop = scroller.scrollHeight;
  followed = scroller.scrollTop;
}

/// Whether the person has text selected in `box`: redrawing it would lose
/// the selection, so it waits until they are done.
function selecting(box) {
  const selection = getSelection();
  return (
    selection !== null &&
    !selection.isCollapsed &&
    selection.rangeCount > 0 &&
    selection.getRangeAt(0).intersectsNode(box)
  );
}

/// Draw `text` into `box`, each table's box keeping how far it was panned.
function paint(box, text) {
  const panned = new Map();
  for (const run of box.querySelectorAll(".table-run")) {
    if (run.scrollLeft) panned.set(run.dataset.run, run.scrollLeft);
  }
  box.replaceChildren(render(document, text));
  for (const run of box.querySelectorAll(".table-run")) {
    const left = panned.get(run.dataset.run);
    if (left) run.scrollLeft = left;
  }
}

// What is waiting to be drawn: a history while the person is scrolled back
// through the last one, so what they read does not move under them; and
// either one while they have text selected in it.
let pendingHistory = null;
let pendingScreen = null;

function drawHistory(text) {
  if (!pinned || selecting(historyBox)) {
    pendingHistory = text;
    return;
  }
  pendingHistory = null;
  paint(historyBox, text);
  follow();
}

function drawScreen(text) {
  if (selecting(screenBox)) {
    pendingScreen = text;
    return;
  }
  pendingScreen = null;
  paint(screenBox, text);
  follow();
}

function drawPending() {
  if (pendingHistory !== null) drawHistory(pendingHistory);
  if (pendingScreen !== null) drawScreen(pendingScreen);
}

scroller.addEventListener("scroll", () => {
  if (scroller.scrollTop === followed) return;
  followed = -1;
  pinned = atBottom();
  if (pinned) drawPending();
});
document.addEventListener("selectionchange", () => {
  if (getSelection()?.isCollapsed) drawPending();
});
// A narrower or wider panel wraps the text anew.
new ResizeObserver(follow).observe(scroller);

async function start() {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  // Only a factory the options page holds is read: this page takes its
  // factory from its own address.
  if (!url || !session || !stored.some((item) => factoryUrl(item?.url) === url)) {
    say("this terminal names no configured factory or no session", true);
    return;
  }

  // An item's agent has somewhere else to be spoken to; a scratch session's
  // pane is view-only only where this factory takes no writes.
  // Which factory the session runs on: the options page's name for it, or its
  // host.
  const item = stored.find((one) => factoryUrl(one?.url) === url);
  const on = ` \u00b7 on ${String(item?.label ?? "").trim() || factoryLabel(url)}`;
  const viewOnly = session.includes("~")
    ? "live · view only"
    : "live · view only: comment on the item to speak to its agent";
  const reconnect = document.getElementById("reconnect");
  let port = null;
  let retry = null;
  let failures = 0;
  let live = false;
  /// Why typing last turned itself off, said until it is turned on again.
  let notice = "";

  function showLive() {
    live = true;
    typeButton.hidden = !takesInput;
    if (!takesInput) say(viewOnly + on);
    else if (typing) say(`live${on} · typing into the pane`);
    else if (notice) say(`live${on} · typing turned off: ${notice}`, true);
    else say(`live${on}`);
  }

  function notLive() {
    live = false;
    stopTyping();
    typeButton.hidden = true;
  }

  function hangUp() {
    clearTimeout(retry);
    const held = port;
    port = null;
    held?.disconnect();
  }

  /// Stop reading: the factory is not asked again until the person says so.
  function stop(text) {
    hangUp();
    notLive();
    say(text, true);
    reconnect.hidden = false;
  }

  /// A read that failed or ended, asked for again a few times before the page
  /// gives up and offers Reconnect.
  function dropped(error) {
    notLive();
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
          drawHistory(String(frame.history ?? ""));
        } else {
          drawScreen(String(frame.screen ?? ""));
          showLive();
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

  // Typing. What is typed, in order: one request in the air at a time, and
  // whatever was typed meanwhile goes in the next.
  let queue = [];
  let sending = false;
  let composing = false;
  let lastActivity = Date.now();

  function startTyping() {
    if (!takesInput || !live) return;
    typing = true;
    notice = "";
    lastActivity = Date.now();
    typeButton.setAttribute("aria-pressed", "true");
    field.value = "";
    field.focus({ preventScroll: true });
    showLive();
  }

  /// Turn typing off; `why`, when it was not the person who did, is said.
  function stopTyping(why) {
    if (!typing) return;
    typing = false;
    composing = false;
    queue = [];
    field.value = "";
    field.blur();
    typeButton.setAttribute("aria-pressed", "false");
    notice = why ?? "";
    if (live) showLive();
  }

  async function flush() {
    if (sending) return;
    sending = true;
    while (queue.length) {
      const input = queue.shift();
      let reply;
      try {
        reply = await chrome.runtime.sendMessage({ type: "ssf:pane-input", url, session, ...input });
      } catch (error) {
        reply = { ok: false, error: `the extension's service worker did not answer (${error})` };
      }
      if (!reply?.ok) {
        // Typing on into a pane that is not taking it would be typing blind.
        stopTyping(`not typed (${reply?.error ?? "the factory did not answer"})`);
      }
    }
    sending = false;
  }

  function sendKeys(keys) {
    if (!typing || keys.length === 0) return;
    lastActivity = Date.now();
    for (const key of keys) {
      const last = queue.at(-1);
      if (last?.keys && last.keys.length < MAX_KEYS) last.keys.push(key);
      else queue.push({ keys: [key] });
    }
    flush();
  }

  typeButton.addEventListener("click", () => (typing ? stopTyping() : startTyping()));
  field.addEventListener("keydown", (event) => {
    if (event.isComposing || event.keyCode === 229) return;
    const key = keyForKeyDown(event);
    if (key === undefined) return;
    event.preventDefault();
    sendKeys([key]);
  });
  field.addEventListener("beforeinput", (event) => {
    const key = keyForInputType(event.inputType);
    if (key === null) return;
    // Backspace inside an IME edits the candidate, not the pane.
    if (event.isComposing && key === "Backspace") return;
    event.preventDefault();
    sendKeys([key]);
  });
  field.addEventListener("input", (event) => {
    if (event.isComposing || composing) return;
    const text = field.value;
    field.value = "";
    sendKeys(textToKeys(text));
  });
  // An IME's composition is sent once, when it is committed.
  field.addEventListener("compositionstart", () => {
    composing = true;
  });
  field.addEventListener("compositionend", (event) => {
    composing = false;
    const text = field.value || event.data || "";
    field.value = "";
    sendKeys(textToKeys(text));
  });
  field.addEventListener("paste", (event) => {
    event.preventDefault();
    if (!typing) return;
    lastActivity = Date.now();
    const body = pasteBody(session, event.clipboardData?.getData("text/plain") ?? "");
    if (!body) return;
    // A paste too long for one write is not sent in part: said, and typing
    // turned off, so the person sees it did not go.
    if (!fitsWrite(body)) return stopTyping("the paste is longer than the factory takes in one write");
    queue.push({ text: body.text });
    flush();
  });
  // A click in the terminal gives typing its field back, unless it was to
  // select text.
  scroller.addEventListener("mouseup", () => {
    if (typing && getSelection()?.isCollapsed) field.focus({ preventScroll: true });
  });
  for (const name of ["pointerdown", "wheel", "keydown"]) {
    addEventListener(name, () => (lastActivity = Date.now()), { capture: true, passive: true });
  }
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") stopTyping("the tab was hidden");
  });
  setInterval(() => {
    if (typing && Date.now() - lastActivity >= IDLE_MS) stopTyping("nothing was typed for a while");
  }, 30000);
}

// Spike (#561): an item session's herdr pane is a live terminal too
// (`herdr terminal session control`), unless the address says `live=0`.
if (session.includes("~") || params.get("live") !== "0") {
  // A scratch session is a live terminal of its own (term-xterm.js, #491); an
  // item's stays the mirror above.
  scroller.hidden = true;
  startTerm().catch((error) => say(String(error), true));
} else {
  start().catch((error) => say(String(error), true));
}

async function startTerm() {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  if (!url || !stored.some((item) => factoryUrl(item?.url) === url)) {
    say("this terminal names no configured factory or no session", true);
    return;
  }
  const { run } = await import("./term-xterm.js");
  run({
    url,
    session,
    takesInput,
    say,
    box: document.getElementById("xterm"),
    reconnect: document.getElementById("reconnect"),
    resume: document.getElementById("resume"),
  });
}

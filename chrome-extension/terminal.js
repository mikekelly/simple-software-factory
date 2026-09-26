// The terminal page: a scratch session's live terminal (term-xterm.js, #491),
// or an item session's pane (#563): read-only, the pane mirror below; with
// Type on, the live terminal (term-xterm.js runItem), which takes control of
// the pane.
//
// The pane mirror (#414): one item session's agent pane, drawn as styled text
// (pane-render.js, after collie) rather than by a terminal emulator, and
// redacted by the factory. Type is offered where the factory allows typing:
// the snapshot's `pane_input` for the session, passed in the page address
// (`item_pane_input`, #439: its agent is otherwise spoken to by commenting on
// the item), and the factory's Writes switch on the options page. The factory
// enforces the same rule on every request.
//
// The factory reads the pane's visible screen a few times a second while
// someone watches it and sends a frame only when it changed
// (`api/pane/<session>`, docs/dashboard.md); each frame is the whole screen,
// drawn over the last one. Every few seconds it also sends the history above
// the screen, drawn above it in the same scroller: the wheel scrolls back
// through it. The text is drawn at a fixed size and wraps at the panel's
// width, so the pane is never shrunk to fit and never resized.
//
// Typing is a mode the person turns on (Type), which swaps the mirror for the
// live terminal until Type turns off: by the person, or by itself when the
// tab is hidden, nothing is typed for half an hour, Writes goes off, or the
// factory refuses control or the pane is taken over elsewhere. Every one of
// those gives the pane back and shows the mirror again. A key or a paste
// while read-only is not sent anywhere: a notice says so.
//
// This page is the extension's own, framed over the GitHub page by Open
// (pane-overlay.js, #477), and it talks to no factory itself: a frame under
// github.com is where Chrome's local-network rules can hold a request to a
// factory on a private or tailnet address. The service worker reads the stream
// with the extension's permission and passes it on a port (`ssf-pane`), as it
// passes the live terminal's socket (`ssf-term`).
import { factoryLabel, factoryUrl } from "./factory-url.js";
import { render } from "./pane-render.js";

/// Failures in a row, with no frame between them, before the page stops
/// asking: a reader that cannot start is not asked again every few seconds.
const MAX_FAILURES = 3;

/// How long a stream that failed waits before it is asked for again.
const RETRY_MS = 3000;

/// The port carries the stream, and a port that is used is what keeps the
/// service worker holding it awake: the frames it sends do not.
const PING_MS = 20000;

/// How close to the bottom, in pixels, still counts as at the bottom.
const BOTTOM_SLACK = 4;

const params = new URLSearchParams(location.search);
const url = factoryUrl(params.get("factory"));
const session = String(params.get("session") ?? "");
const takesInput = params.get("input") === "1";
const stateLine = document.getElementById("state");
const typeButton = document.getElementById("type");
const noticeNode = document.getElementById("notice");
const scroller = document.getElementById("scroller");
const historyBox = document.getElementById("history");
const screenBox = document.getElementById("screen");
document.getElementById("session").textContent = session;
// Framed in a floating window, whose title bar already names the session.
document.getElementById("session").hidden = window.top !== window;
document.title = `${session} · ssf`;

function say(text, problem = false) {
  stateLine.textContent = text;
  stateLine.dataset.problem = String(problem);
}

let noticeTimer = null;
/// A line under the header: why Type went off (kept), or that a key was not
/// sent (for a few seconds).
function notice(text, sticky = false) {
  clearTimeout(noticeTimer);
  noticeNode.textContent = text;
  noticeNode.hidden = !text;
  if (text && !sticky) noticeTimer = setTimeout(() => (noticeNode.hidden = true), 6000);
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

async function start(stored) {
  // Which factory the session runs on: the options page's name for it, or its
  // host.
  const item = stored.find((one) => factoryUrl(one?.url) === url);
  const on = ` \u00b7 on ${String(item?.label ?? "").trim() || factoryLabel(url)}`;
  const reconnect = document.getElementById("reconnect");
  const { runItem } = await import("./term-xterm.js");
  const live = runItem({
    url,
    session,
    say,
    box: document.getElementById("xterm"),
    notice,
    onLeave: (why) => {
      scroller.hidden = false;
      follow();
      if (why) notice(`Type is off: ${why}.`, true);
      shown();
    },
  });
  let port = null;
  let retry = null;
  let failures = 0;
  let streaming = false;
  /// The factory's Writes switch, followed as the options page changes it:
  /// absent is on, as the service worker reads it.
  let writes = item?.writes !== false;
  const canType = () => takesInput && writes;

  function shown() {
    typeButton.hidden = !canType() || !(streaming || live.typing());
    typeButton.setAttribute("aria-pressed", String(live.typing()));
    if (live.typing()) return;
    if (streaming) say(`live${on} \u00b7 read-only`);
  }

  function notLive() {
    streaming = false;
    shown();
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
    if (!live.typing()) say(text, true);
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
    if (!live.typing()) say("the stream stopped; reconnecting…", true);
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
          streaming = true;
          shown();
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

  typeButton.addEventListener("click", () => {
    if (live.typing()) return live.leave();
    if (!canType()) return;
    notice("");
    scroller.hidden = true;
    live.enter();
    shown();
  });
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area !== "local" || !changes.factories) return;
    const now = (changes.factories.newValue ?? []).find((one) => factoryUrl(one?.url) === url);
    writes = now !== undefined && now?.writes !== false;
    if (!writes) live.leave("writes were turned off for this factory");
    shown();
  });

  /// A key or a paste while read-only goes nowhere: said, so it is not
  /// mistaken for typing. Copying (Ctrl or Cmd with a key) is not typing.
  function readOnly() {
    notice(
      "Read-only: nothing was sent. " +
        (canType()
          ? "Turn on Type to type into this pane."
          : !takesInput
            ? "Comment on the item to speak to its agent."
            : "Writes are off for this factory on the options page."),
    );
  }
  addEventListener("keydown", (event) => {
    if (live.typing() || event.ctrlKey || event.metaKey || event.altKey) return;
    if (event.target instanceof HTMLButtonElement && (event.key === "Enter" || event.key === " ")) return;
    if (event.key.length === 1 || ["Enter", "Backspace", "Tab", "Delete"].includes(event.key)) readOnly();
  });
  addEventListener("paste", () => live.typing() || readOnly());
}

function startScratch() {
  scroller.hidden = true;
  import("./term-xterm.js").then(({ run }) =>
    run({
      url,
      session,
      takesInput,
      say,
      box: document.getElementById("xterm"),
      reconnect: document.getElementById("reconnect"),
      resume: document.getElementById("resume"),
    }),
  );
}

(async () => {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  // Only a factory the options page holds is read: this page takes its
  // factory from its own address.
  if (!url || !session || !stored.some((item) => factoryUrl(item?.url) === url)) {
    say("this terminal names no configured factory or no session", true);
    return;
  }
  // A scratch session is a live terminal of its own (#491); an item's is the
  // mirror, and the live terminal while Type is on (#563).
  if (session.includes("~")) startScratch();
  else await start(stored);
})().catch((error) => say(String(error), true));

// An item session's pane (#563). Read-only, it is the pane mirror: the
// server's `api/pane/<session>` stream of the pane's screen and the history
// above it, redacted, drawn as styled text by the extension's
// pane-render.js (served from the extension's copy), wrapped at the window's
// width, with the wheel scrolling back through the history. Pressing a key or
// pasting there sends nothing: a notice says so.
//
// Type swaps the mirror for the live terminal: xterm.js on the server's
// `api/term/<session>` WebSocket, which streams the pane through `herdr
// terminal session observe|control`, and asks the server for control, which
// it gives only where it and the factory allow typing
// (`dashboard.terminal_input`, `item_pane_input`); in control the pane takes
// this terminal's size. Type turns itself off when the tab is hidden, nothing
// is typed for a while, control is refused or taken over, or the socket
// closes; each gives the pane back and shows the mirror again.
//
// herdr keeps the scrollback and does not tell the viewer the pane's modes,
// so xterm keeps no scrollback, each wheel notch is one scroll message, and
// a paste is always sent as a bracketed paste.
import { Terminal } from "./xterm.mjs";
import { FitAddon } from "./addon-fit.mjs";
import { render } from "./pane-render.js";

const IDLE_MS = 30 * 60 * 1000;
/// Pixels of a smooth (trackpad) scroll that count as one wheel notch.
const NOTCH_PX = 50;

const session = new URLSearchParams(location.search).get("session") ?? "";
const box = document.getElementById("box");
const statusNode = document.getElementById("status");
const noticeNode = document.getElementById("notice");
const typeButton = document.getElementById("type");
const reconnectButton = document.getElementById("reconnect");
const scroller = document.getElementById("mirror");
const historyBox = document.getElementById("history");
const screenBox = document.getElementById("screen");
const base = location.pathname.replace(/[^/]*$/, "");
document.getElementById("title").textContent = session;
document.title = `${session} · SSF terminal`;

const term = new Terminal({
  cursorBlink: true,
  fontFamily:
    '"JetBrains Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
  fontSize: 13,
  macOptionIsMeta: true,
  scrollback: 0,
  theme: { background: "#0d1117", foreground: "#e6edf3" },
});
const fit = new FitAddon();
term.loadAddon(fit);
term.open(box);
box.hidden = true;

const encoder = new TextEncoder();
let ws = null;
let control = false;
/// Whether this server lets this page type (`dashboard.terminal_input`),
/// from the page itself: the Type button is offered only then.
const mayType = document.body.dataset.terminalInput === "true";
let streaming = false;
/// Why the mirror stopped, said while it is shown.
let stopped = "";
let lastActivity = Date.now();
let wheel = 0;
let noticeTimer = null;

function notice(text, sticky = false) {
  clearTimeout(noticeTimer);
  noticeNode.textContent = text;
  noticeNode.hidden = !text;
  if (text && !sticky) noticeTimer = setTimeout(() => (noticeNode.hidden = true), 6000);
}

const typing = () => ws !== null;

function shown() {
  if (typing()) statusNode.textContent = control ? "live · typing" : "waiting for control of the pane…";
  else statusNode.textContent = streaming ? "live · read-only" : stopped || "connecting…";
  typeButton.hidden = !mayType;
  typeButton.setAttribute("aria-pressed", String(typing()));
}

function send(message) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(message);
}

/// The size this page gives the pane in control: the box's, in cells.
function sendSize() {
  const size = fit.proposeDimensions();
  if (control && size && size.cols > 0 && size.rows > 0) {
    send(JSON.stringify({ type: "resize", cols: size.cols, rows: size.rows }));
  }
}

/// Bytes typed at the pane; nothing is sent until control is given.
function type(bytes) {
  if (!control) return notice("Nothing was sent: waiting for control of the pane.");
  lastActivity = Date.now();
  send(bytes);
}

// The mirror. The view follows the live screen while it is at the bottom,
// and stays where the person put it once they scroll up; a history that
// arrives meanwhile waits until they are back at the bottom.
let pinned = true;
let pendingHistory = null;
const atBottom = () => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight <= 4;
function follow() {
  if (pinned) scroller.scrollTop = scroller.scrollHeight;
}
function drawHistory(text) {
  if (!pinned) {
    pendingHistory = text;
    return;
  }
  pendingHistory = null;
  historyBox.replaceChildren(render(document, text));
  follow();
}
scroller.addEventListener("scroll", () => {
  pinned = atBottom();
  if (pinned && pendingHistory !== null) drawHistory(pendingHistory);
});
new ResizeObserver(follow).observe(scroller);

function watch() {
  const source = new EventSource(`${base}api/pane/${encodeURIComponent(session)}`);
  source.addEventListener("screen", (event) => {
    screenBox.replaceChildren(render(document, String(JSON.parse(event.data).screen ?? "")));
    follow();
    streaming = true;
    shown();
  });
  source.addEventListener("history", (event) => drawHistory(String(JSON.parse(event.data).history ?? "")));
  // The server's reader stopped, and says why; EventSource reconnects itself
  // after a dropped stream, but not after this.
  source.addEventListener("error", (event) => {
    if (typeof event.data !== "string") return;
    source.close();
    streaming = false;
    let said = event.data;
    try {
      said = JSON.parse(event.data).error ?? said;
    } catch {
      // Not JSON: the text is the reason.
    }
    stopped = `the mirror stopped (${said})`;
    shown();
    reconnectButton.hidden = false;
  });
}

/// Give the pane back and show the mirror; `why`, when it was not the person
/// who turned Type off, is said.
function leave(why) {
  if (!ws) return;
  send(JSON.stringify({ type: "release" }));
  const socket = ws;
  ws = null;
  control = false;
  socket.close();
  box.hidden = true;
  scroller.hidden = false;
  follow();
  if (why) notice(`Type is off: ${why}.`, true);
  shown();
}

function enter() {
  if (ws || !mayType) return;
  notice("");
  scroller.hidden = true;
  box.hidden = false;
  lastActivity = Date.now();
  try {
    fit.fit();
  } catch {
    // A box with no size has nothing to fit.
  }
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(`${scheme}://${location.host}${base}api/term/${encodeURIComponent(session)}`);
  socket.binaryType = "arraybuffer";
  ws = socket;
  shown();
  socket.onopen = () => term.reset();
  socket.onmessage = (event) => {
    if (ws !== socket) return;
    if (typeof event.data !== "string") {
      term.write(new Uint8Array(event.data));
      return;
    }
    let message;
    try {
      message = JSON.parse(event.data);
    } catch {
      return;
    }
    if (message.type === "mode") {
      control = message.control === true;
      if (message.may_control !== true) return leave("this server does not take typing from this page");
      // Control is asked for as the stream starts, and again when it comes
      // back read-only (the pane was found again).
      if (!control) send(JSON.stringify({ type: "control" }));
      else {
        try {
          fit.fit();
        } catch {
          // As above.
        }
        sendSize();
        term.focus();
      }
      shown();
    } else if (message.type === "size" && !control) {
      term.resize(message.cols, message.rows);
    } else if (message.type === "refused") {
      leave(String(message.reason ?? "control was refused"));
    } else if (message.type === "notice") {
      notice(message.text);
    }
  };
  socket.onclose = () => {
    if (ws === socket) leave("the terminal closed");
  };
}

term.onData((text) => type(encoder.encode(text)));
term.onBinary((text) => type(Uint8Array.from(text, (c) => c.charCodeAt(0) & 0xff)));
term.attachCustomKeyEventHandler((event) => {
  // xterm sends a plain CR for Shift+Enter; harnesses read ESC CR as a
  // newline that does not submit.
  if (event.key === "Enter" && event.shiftKey && !event.ctrlKey && !event.altKey && !event.metaKey) {
    if (event.type === "keydown") type(encoder.encode("\x1b\r"));
    return false;
  }
  return true;
});
// Ahead of xterm's own paste handling, which brackets a paste only when the
// app asked for it -- and herdr never says whether it did.
box.addEventListener(
  "paste",
  (event) => {
    event.preventDefault();
    event.stopPropagation();
    const text = (event.clipboardData?.getData("text/plain") ?? "")
      .replace(/\x1b\[20[01]~/g, "")
      .replace(/\r?\n/g, "\r");
    if (text) type(encoder.encode(`\x1b[200~${text}\x1b[201~`));
  },
  true,
);
term.attachCustomWheelEventHandler((event) => {
  if (!control) return false;
  // A mouse wheel moves in lines or whole notches; a trackpad in pixels.
  const notches =
    event.deltaMode === 0 ? Math.trunc((wheel += event.deltaY) / NOTCH_PX) : Math.sign(event.deltaY);
  if (event.deltaMode === 0) wheel -= notches * NOTCH_PX;
  for (let i = 0; i < Math.abs(notches); i += 1) {
    send(JSON.stringify({ type: "scroll", direction: notches < 0 ? "up" : "down" }));
  }
  lastActivity = Date.now();
  return false;
});

let fitTimer = null;
new ResizeObserver(() => {
  clearTimeout(fitTimer);
  fitTimer = setTimeout(() => {
    if (control) {
      try {
        fit.fit();
      } catch {
        // As above.
      }
    }
    sendSize();
  }, 100);
}).observe(box);
term.onResize(({ cols, rows }) => {
  if (control) send(JSON.stringify({ type: "resize", cols, rows }));
});

typeButton.addEventListener("click", () => (typing() ? leave() : enter()));
reconnectButton.addEventListener("click", () => {
  reconnectButton.hidden = true;
  stopped = "";
  shown();
  watch();
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") leave("the tab was hidden");
});
setInterval(() => {
  if (typing() && Date.now() - lastActivity >= IDLE_MS) leave("nothing was typed for a while");
}, 30000);

// A key or a paste while read-only goes nowhere: said, so it is not mistaken
// for typing. Copying (Ctrl or Cmd with a key) is not typing.
function readOnly() {
  notice(
    mayType
      ? "Read-only: nothing was sent. Turn on Type to type into this pane."
      : "Read-only: nothing was sent. This server does not take typing from this page.",
  );
}
addEventListener("keydown", (event) => {
  if (typing() || event.ctrlKey || event.metaKey || event.altKey) return;
  if (event.target instanceof HTMLButtonElement && (event.key === "Enter" || event.key === " ")) return;
  if (event.key.length === 1 || ["Enter", "Backspace", "Tab", "Delete"].includes(event.key)) readOnly();
});
addEventListener("paste", () => typing() || readOnly());

shown();
if (session) watch();
else statusNode.textContent = "no session named: open this from a card on the dashboard";

// An item session's live terminal (#563): xterm.js on the server's
// `api/term/<session>` WebSocket, which streams the pane through `herdr
// terminal session observe|control`. It opens read-only at the pane's own
// size. Type asks the server for control, which it gives only where it and
// the factory allow typing (`dashboard.terminal_input`, `item_pane_input`);
// in control the pane takes this terminal's size. Type turns itself off when
// the tab is hidden or nothing is typed for a while, which gives the pane
// back.
//
// herdr keeps the scrollback and does not tell the viewer the pane's modes,
// so xterm keeps no scrollback, each wheel notch is one scroll message, and
// a paste is always sent as a bracketed paste.
import { Terminal } from "./xterm.mjs";
import { FitAddon } from "./addon-fit.mjs";

const IDLE_MS = 30 * 60 * 1000;
/// Pixels of a smooth (trackpad) scroll that count as one wheel notch.
const NOTCH_PX = 50;

const session = new URLSearchParams(location.search).get("session") ?? "";
const box = document.getElementById("box");
const statusNode = document.getElementById("status");
const noticeNode = document.getElementById("notice");
const typeButton = document.getElementById("type");
const reconnectButton = document.getElementById("reconnect");
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

const encoder = new TextEncoder();
let ws = null;
let control = false;
let mayControl = false;
let typing = false;
let lastActivity = Date.now();
let wheel = 0;
let noticeTimer = null;

function notice(text, sticky = false) {
  clearTimeout(noticeTimer);
  noticeNode.textContent = text;
  noticeNode.hidden = !text;
  if (text && !sticky) noticeTimer = setTimeout(() => (noticeNode.hidden = true), 6000);
}

function shown() {
  statusNode.textContent = control ? "live · typing" : "live · read-only";
  typeButton.hidden = !mayControl;
  typeButton.setAttribute("aria-pressed", String(typing));
}

function send(message) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(message);
}

/// The size this page would give the pane: the box's, in cells.
function sendSize() {
  const size = fit.proposeDimensions();
  if (size && size.cols > 0 && size.rows > 0) {
    send(JSON.stringify({ type: "resize", cols: size.cols, rows: size.rows }));
  }
}

function readOnly() {
  notice(
    mayControl
      ? "Read-only: nothing was sent. Turn on Type to type into this pane."
      : "Read-only: nothing was sent. This server does not take typing from this page.",
  );
}

/// Bytes typed at the pane, or the read-only notice instead.
function type(bytes) {
  if (!control) return readOnly();
  lastActivity = Date.now();
  send(bytes);
}

function setTyping(on, why) {
  if (typing === on) return;
  typing = on;
  lastActivity = Date.now();
  send(JSON.stringify({ type: on ? "control" : "release" }));
  if (!on && why) notice(`Type is off: ${why}.`);
  shown();
}

function connect() {
  reconnectButton.hidden = true;
  statusNode.textContent = "connecting…";
  const base = location.pathname.replace(/[^/]*$/, "");
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  const socket = new WebSocket(`${scheme}://${location.host}${base}api/term/${encodeURIComponent(session)}`);
  socket.binaryType = "arraybuffer";
  ws = socket;
  socket.onopen = () => {
    term.reset();
    sendSize();
  };
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
      mayControl = message.may_control === true;
      // A stream that came back as read-only (the pane was found again)
      // is asked for control again while Type is on.
      if (typing && !control && mayControl) send(JSON.stringify({ type: "control" }));
      if (control) {
        notice("");
        try {
          fit.fit();
        } catch {
          // A box with no size has nothing to fit.
        }
        sendSize();
      }
      shown();
    } else if (message.type === "size" && !control) {
      term.resize(message.cols, message.rows);
    } else if (message.type === "refused") {
      typing = false;
      shown();
      notice(`Not typing: ${message.reason}`, true);
    } else if (message.type === "notice") {
      notice(message.text);
    }
  };
  socket.onclose = () => {
    if (ws !== socket) return;
    ws = null;
    control = false;
    typing = false;
    shown();
    statusNode.textContent = "disconnected";
    reconnectButton.hidden = false;
    term.write("\r\n\x1b[2m[the terminal closed]\x1b[0m\r\n");
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

typeButton.addEventListener("click", () => {
  setTyping(!typing);
  term.focus();
});
reconnectButton.addEventListener("click", connect);
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") setTyping(false, "the tab was hidden");
});
setInterval(() => {
  if (typing && Date.now() - lastActivity >= IDLE_MS) setTyping(false, "nothing was typed for a while");
}, 30000);

if (session) connect();
else statusNode.textContent = "no session named: open this from a card on the dashboard";
term.focus();

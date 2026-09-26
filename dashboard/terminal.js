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
//
// Read-only, the font shrinks (never past its normal size) so the pane's
// cells fit the window, and herdr takes no scroll from an observer: the wheel
// up opens a history view instead, a second, input-less xterm over this one
// holding the pane's recent output read once from the mirror stream
// (`api/pane/<session>`: the history above the screen, and the screen). It
// sends nothing; scrolling it to the bottom, or Esc, goes back to live.
import { Terminal } from "./xterm.mjs";
import { FitAddon } from "./addon-fit.mjs";

const IDLE_MS = 30 * 60 * 1000;
/// Pixels of a smooth (trackpad) scroll that count as one wheel notch.
const NOTCH_PX = 50;
/// The font's normal size; read-only shrinks it to fit the pane.
const FONT = 13;
/// The smallest a read-only font gets.
const MIN_FONT = 4;
const FONTS =
  '"JetBrains Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace';
const THEME = { background: "#0d1117", foreground: "#e6edf3" };

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
  fontFamily: FONTS,
  fontSize: FONT,
  macOptionIsMeta: true,
  scrollback: 0,
  theme: THEME,
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
  statusNode.textContent = control
    ? "live · typing"
    : history
      ? "history · read-only: scroll to the bottom, or Esc, for live"
      : "live · read-only";
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

/// Read-only: the font that fits the pane's cells in the box, at most FONT.
/// xterm measures a new font's cells on a later frame, so it is found a step
/// a frame.
let fontFrame = 0;
function fitFont(steps = 12) {
  cancelAnimationFrame(fontFrame);
  if (control || steps <= 0) return;
  // The box's content against the cells as drawn now.
  const style = getComputedStyle(box);
  const width = box.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
  const height = box.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
  const screen = term.element?.querySelector(".xterm-screen");
  if (!screen?.offsetWidth || !screen.offsetHeight || width <= 0 || height <= 0) return;
  const font = term.options.fontSize;
  const scale = Math.min(width / screen.offsetWidth, height / screen.offsetHeight);
  let next = Math.floor(font * scale * 2) / 2;
  if (scale < 1 && next >= font) next = font - 0.5;
  next = Math.max(MIN_FONT, Math.min(FONT, next));
  if (next === font) return;
  term.options.fontSize = next;
  fontFrame = requestAnimationFrame(() => fitFont(steps - 1));
}

/// Control: the normal font, and the pane sized to the box.
function fitBox() {
  cancelAnimationFrame(fontFrame);
  term.options.fontSize = FONT;
  try {
    fit.fit();
  } catch {
    // A box with no size has nothing to fit.
  }
}

// The history view: its own box over the terminal, and its own xterm.
const historyBox = document.createElement("div");
historyBox.className = "term-history";
historyBox.hidden = true;
box.append(historyBox);
let history = null;
let historySource = null;

/// Open the history view, filled once from the mirror stream. Nothing is
/// sent to the pane.
function openHistory() {
  if (history) return;
  historyBox.hidden = false;
  const view = new Terminal({
    cols: term.cols,
    rows: term.rows,
    cursorBlink: false,
    cursorInactiveStyle: "none",
    disableStdin: true,
    fontFamily: FONTS,
    fontSize: term.options.fontSize,
    scrollback: 5000,
    theme: THEME,
  });
  history = view;
  view.open(historyBox);
  view.write("\x1b[2m[reading the pane's history…]\x1b[0m");
  view.attachCustomKeyEventHandler((event) => {
    if (event.key === "Escape" && event.type === "keydown") setTimeout(closeHistory);
    return false;
  });
  view.attachCustomWheelEventHandler((event) => {
    const buffer = view.buffer.active;
    if (event.deltaY > 0 && buffer.viewportY >= buffer.baseY) setTimeout(closeHistory);
    return true;
  });
  view.focus();
  shown();
  const base = location.pathname.replace(/[^/]*$/, "");
  const source = new EventSource(`${base}api/pane/${encodeURIComponent(session)}`);
  historySource = source;
  let above = null;
  let drawn = false;
  const draw = (screen) => {
    if (drawn || history !== view) return;
    drawn = true;
    source.close();
    view.reset();
    view.write(`${above ? `${above}\r\n` : ""}${screen}\x1b[?25l`, () => {
      if (history !== view) return;
      view.scrollLines(-3);
      view.onScroll((top) => top >= view.buffer.active.baseY && setTimeout(closeHistory));
    });
  };
  source.addEventListener("history", (event) => {
    try {
      above = String(JSON.parse(event.data).history ?? "");
    } catch {
      above = "";
    }
  });
  source.addEventListener("screen", (event) => {
    let screen = "";
    try {
      screen = String(JSON.parse(event.data).screen ?? "");
    } catch {
      // Drawn empty.
    }
    // The history comes first; a reader that has none yet is not waited on long.
    if (above !== null) draw(screen);
    else setTimeout(() => draw(screen), 1500);
  });
  source.addEventListener("error", () => {
    if (drawn || history !== view) return;
    source.close();
    view.reset();
    view.write("\x1b[2m[the pane's history could not be read; Esc for live]\x1b[0m");
  });
}

function closeHistory() {
  if (!history) return;
  historySource?.close();
  historySource = null;
  const view = history;
  history = null;
  historyBox.hidden = true;
  view.dispose();
  shown();
  term.focus();
}

function readOnly() {
  notice(
    typing
      ? "Nothing was sent: waiting for control of the pane."
      : mayControl
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
        closeHistory();
        notice("");
        fitBox();
        sendSize();
      } else fitFont();
      shown();
    } else if (message.type === "size" && !control) {
      term.resize(message.cols, message.rows);
      fitFont();
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
  // herdr takes no scroll from an observer: the wheel up reads history here.
  if (!control) {
    if (event.deltaY < 0) openHistory();
    return false;
  }
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
    if (control) fitBox();
    else fitFont();
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

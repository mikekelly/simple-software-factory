// An item session's live terminal (#563), mountable anywhere (#574): xterm.js on the server's
// `api/term/<session>` WebSocket. The server holds one `herdr terminal session
// control` stream per pane and shares it among every viewer, like a shared
// tmux session: everyone sees the pane. One viewer at a time holds control:
// only its typing, paste and wheel reach the pane (the server drops the
// others'), and the pane takes its size; everyone else sees it at that size,
// the font shrunk to fit, and has **Take control** in the title bar. The
// first to open a pane no one controls holds control; when its holder leaves
// no one does until someone takes it. The title bar lists who is watching. The page is offered only where this server
// and the factory take typing from it (`dashboard.terminal_input`,
// `item_pane_input`), and the server refuses it otherwise. Half an hour with
// nothing typed here closes this page's connection (Reconnect opens it again).
//
// herdr keeps the scrollback and does not tell the viewer the pane's modes,
// so xterm keeps no scrollback, each wheel notch is one scroll message, and
// a paste is always sent as a bracketed paste.
import { Terminal } from "./xterm.mjs";

const IDLE_MS = 30 * 60 * 1000;
/// Pixels of a smooth (trackpad) scroll that count as one wheel notch.
const NOTCH_PX = 50;
/// The font size this page draws at when the pane fits, and the smallest it
/// shrinks to for a pane larger than the window.
const FONT_PX = 13;
const MIN_FONT_PX = 4;

const BAR = `<div class="term-bar">
  <h1 class="term-title"></h1>
  <span class="status" aria-live="polite">connecting…</span>
  <span class="viewers" title="Watching this pane" aria-live="polite"></span>
  <button class="take" type="button" hidden>Take control</button>
  <button class="reconnect" type="button" hidden>Reconnect</button>
</div>
<p class="term-notice" role="status" hidden></p>
<div class="term-box"></div>`;

/// Mount `session`'s live terminal, with its status bar, into `host`, and
/// answer a handle whose `dispose()` closes the socket and frees xterm.
export function mountTerminal(host, session) {
  host.classList.add("term-host");
  host.innerHTML = BAR;
  const box = host.querySelector(".term-box");
  const statusNode = host.querySelector(".status");
  const viewersNode = host.querySelector(".viewers");
  const noticeNode = host.querySelector(".term-notice");
  const reconnectButton = host.querySelector(".reconnect");
  const takeButton = host.querySelector(".take");
  host.querySelector(".term-title").textContent = session;

  const term = new Terminal({
    cursorBlink: true,
    fontFamily:
      '"JetBrains Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
    fontSize: FONT_PX,
    macOptionIsMeta: true,
    scrollback: 0,
    theme: { background: "#0a1112", foreground: "#e6edf3" },
  });
  term.open(box);

  const encoder = new TextEncoder();
  let ws = null;
  /// The pane's size, as the server last said it.
  let pane = null;
  let lastActivity = Date.now();
  let wheel = 0;
  let noticeTimer = null;
  /// Whether this page holds control of the pane, as the server last said.
  let control = false;

  function notice(text, sticky = false) {
    clearTimeout(noticeTimer);
    noticeNode.textContent = text;
    noticeNode.hidden = !text;
    if (text && !sticky) noticeTimer = setTimeout(() => (noticeNode.hidden = true), 6000);
  }

  function send(message) {
    if (ws?.readyState === WebSocket.OPEN) ws.send(message);
  }

  /// The room the box has for the terminal, and a cell's size at FONT_PX.
  function measure() {
    const style = getComputedStyle(box);
    const px = (name) => parseFloat(style.getPropertyValue(name)) || 0;
    const w = box.clientWidth - px("padding-left") - px("padding-right");
    const h = box.clientHeight - px("padding-top") - px("padding-bottom");
    const cell = term._core?._renderService?.dimensions?.css?.cell;
    if (!cell?.width || !cell?.height || w <= 0 || h <= 0) return null;
    const k = FONT_PX / term.options.fontSize;
    return { w, h, cw: cell.width * k, ch: cell.height * k };
  }

  /// The size this page would give the pane: the box's, in cells at FONT_PX.
  function mySize() {
    const m = measure();
    if (!m) return null;
    const cols = Math.max(1, Math.min(1000, Math.floor(m.w / m.cw)));
    const rows = Math.max(1, Math.min(1000, Math.floor(m.h / m.ch)));
    return { cols, rows };
  }

  /// Draw the pane at its size, the font shrunk until it fits the box.
  function fitPane() {
    if (!pane) return;
    if (term.cols !== pane.cols || term.rows !== pane.rows) term.resize(pane.cols, pane.rows);
    const m = measure();
    if (!m) return;
    const scale = Math.min(m.w / (pane.cols * m.cw), m.h / (pane.rows * m.ch));
    const font = Math.max(MIN_FONT_PX, Math.min(FONT_PX, Math.floor(FONT_PX * scale)));
    if (term.options.fontSize !== font) term.options.fontSize = font;
  }

  /// Tell the server this page's size: the pane takes it while this page
  /// holds control, and the server keeps it for when it takes control.
  function claimSize() {
    const size = mySize();
    if (size && (size.cols !== pane?.cols || size.rows !== pane?.rows)) {
      send(JSON.stringify({ type: "resize", ...size }));
    }
  }

  function type(bytes) {
    if (ws?.readyState !== WebSocket.OPEN) return;
    if (!control) {
      notice("View only: Take control to type here.");
      return;
    }
    lastActivity = Date.now();
    claimSize();
    send(bytes);
  }

  function connect() {
    reconnectButton.hidden = true;
    statusNode.textContent = "connecting…";
    const base = location.pathname.replace(/[^/]*$/, "");
    const scheme = location.protocol === "https:" ? "wss" : "ws";
    const socket = new WebSocket(`${scheme}://${location.host}${base}api/term/${encodeURIComponent(session)}`);
    socket.binaryType = "arraybuffer";
    ws = socket;
    lastActivity = Date.now();
    socket.onopen = () => {
      term.reset();
      statusNode.textContent = "joining…";
      // The name is for the viewer list only; the size is the pane's only if
      // no one else is watching it yet.
      send(JSON.stringify({ type: "hello", name: "dashboard", ...(mySize() ?? {}) }));
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
      if (message.type === "size") {
        pane = { cols: message.cols, rows: message.rows };
        fitPane();
        // The controller's window is the pane's size.
        if (control) claimSize();
      } else if (message.type === "viewers") {
        const had = control;
        control = message.control === true;
        const holder = typeof message.controller === "string" ? message.controller : null;
        viewersNode.textContent = (message.names ?? []).map(String).join(", ");
        statusNode.textContent = control ? "live · in control" : `view only · ${holder ?? "no one"} in control`;
        host.classList.toggle("term-view-only", !control);
        takeButton.hidden = control;
        if (control && !had) {
          notice("");
          claimSize();
        } else if (had && !control) {
          notice(`${holder ?? "Someone"} took control: this terminal is view only.`);
        }
      } else if (message.type === "notice") {
        notice(message.text);
      }
    };
    socket.onclose = () => {
      if (ws !== socket) return;
      ws = null;
      viewersNode.textContent = "";
      control = false;
      takeButton.hidden = true;
      host.classList.remove("term-view-only");
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
    // A mouse wheel moves in lines or whole notches; a trackpad in pixels.
    const notches =
      event.deltaMode === 0 ? Math.trunc((wheel += event.deltaY) / NOTCH_PX) : Math.sign(event.deltaY);
    if (event.deltaMode === 0) wheel -= notches * NOTCH_PX;
    if (!control) return false;
    for (let i = 0; i < Math.abs(notches); i += 1) {
      send(JSON.stringify({ type: "scroll", direction: notches < 0 ? "up" : "down" }));
    }
    if (notches) lastActivity = Date.now();
    return false;
  });

  // A resize of this window is a resize of the pane; the first call is the
  // observer starting, not a resize.
  let fitTimer = null;
  let observed = false;
  const observer = new ResizeObserver(() => {
    clearTimeout(fitTimer);
    fitTimer = setTimeout(() => {
      if (!observed) {
        observed = true;
        return;
      }
      claimSize();
      fitPane();
    }, 100);
  });
  observer.observe(box);

  reconnectButton.addEventListener("click", connect);
  takeButton.addEventListener("click", () => {
    lastActivity = Date.now();
    send(JSON.stringify({ type: "take" }));
    term.focus();
  });
  const idleTimer = setInterval(() => {
    if (ws && Date.now() - lastActivity >= IDLE_MS) {
      notice("Disconnected: nothing was typed for a while.", true);
      ws.close();
    }
  }, 30000);

  if (session) connect();
  else statusNode.textContent = "no session named: open this from a card on the dashboard";

  return {
    focus: () => term.focus(),
    /// Close the socket and free xterm: the panel or page holding it is gone.
    dispose() {
      const socket = ws;
      ws = null;
      socket?.close();
      clearInterval(idleTimer);
      clearTimeout(fitTimer);
      clearTimeout(noticeTimer);
      observer.disconnect();
      term.dispose();
      host.replaceChildren();
    },
  };
}

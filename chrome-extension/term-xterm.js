// A scratch session's live terminal (#491), and an item session's (runItem,
// #563, below): xterm.js (vendored, see the README) attached to the factory's `api/term/<session>` WebSocket through
// the service worker, on a port (`ssf-term`). What the terminal prints comes
// as bytes, what is typed goes as bytes, and the terminal is sized to its
// window: every change of size (debounced) is fitted and sent as a resize,
// which the tmux session follows. A view-only terminal neither types nor
// resizes the session.
import { Terminal } from "./vendor/xterm/xterm.mjs";
import { FitAddon } from "./vendor/xterm/addon-fit.mjs";
import { factoryUrl } from "./factory-url.js";
import { fromBase64, resizeMessage, toBase64 } from "./term-wire.js";

/// How long a run of size changes settles before the terminal is fitted.
const FIT_MS = 100;
/// A port that is used keeps the service worker holding the socket awake.
const PING_MS = 20000;

const encoder = new TextEncoder();

export function run({ url, session, takesInput, say, box, reconnect, resume }) {
  box.hidden = false;
  const term = new Terminal({
    cursorBlink: true,
    disableStdin: !takesInput,
    fontFamily:
      '"JetBrains Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
    fontSize: 13,
    scrollback: 5000,
    theme: { background: "#0d1117", foreground: "#e6edf3" },
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(box);

  let port = null;
  let connected = false;
  const post = (message) => {
    try {
      port?.postMessage(message);
    } catch {
      // The port is gone; its disconnect is heard below.
    }
  };
  const sendSize = () => {
    const text = resizeMessage(term.cols, term.rows);
    if (connected && takesInput && text) post({ type: "resize", data: text });
  };

  /// The socket closed: the session ended (its harness exited, or it was
  /// killed) or the factory went away. Reconnect attaches again to a session
  /// that is still there; Resume (where this factory takes writes) starts
  /// one that ended again on its workspace (`api/scratch/resume`).
  function closed(text) {
    connected = false;
    const held = port;
    port = null;
    held?.disconnect();
    say(`${text} · the session may have ended`, true);
    reconnect.hidden = false;
    resume.hidden = !takesInput;
    term.write(`\r\n\x1b[2m[${text}]\x1b[0m\r\n`);
  }

  async function restart() {
    resume.disabled = true;
    say("resuming…");
    let reply;
    try {
      reply = await chrome.runtime.sendMessage({ type: "ssf:scratch-resume", url, session });
    } catch (error) {
      reply = { ok: false, error: String(error) };
    }
    resume.disabled = false;
    if (reply?.ok) connect();
    else say(`not resumed (${reply?.error ?? "the factory did not answer"})`, true);
  }

  function connect() {
    port?.disconnect();
    reconnect.hidden = true;
    resume.hidden = true;
    connected = false;
    say("connecting…");
    const opened = chrome.runtime.connect({ name: "ssf-term" });
    port = opened;
    opened.onMessage.addListener((message) => {
      if (port !== opened) return;
      if (message?.type === "open") {
        connected = true;
        term.reset();
        say(takesInput ? "live" : "live · view only");
        sendSize();
        term.focus();
      } else if (message?.type === "data") {
        term.write(fromBase64(message.data));
      } else if (message?.type === "closed") {
        const why = message.error ?? (message.reason || null);
        closed(why ? `the terminal closed (${why})` : "the terminal closed");
      }
    });
    opened.onDisconnect.addListener(() => {
      if (port === opened) closed("the terminal closed: the extension's service worker stopped");
    });
    opened.postMessage({ type: "open", url, session });
  }

  if (takesInput) {
    term.onData((text) => connected && post({ type: "input", data: toBase64(encoder.encode(text)) }));
    // Mouse reports and the like, one byte per character.
    term.onBinary((text) => {
      if (!connected) return;
      const bytes = Uint8Array.from(text, (c) => c.charCodeAt(0) & 0xff);
      post({ type: "input", data: toBase64(bytes) });
    });
  }
  term.onResize(sendSize);
  let timer = null;
  new ResizeObserver(() => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      try {
        fit.fit();
      } catch {
        // A box with no size (hidden) has nothing to fit.
      }
    }, FIT_MS);
  }).observe(box);
  try {
    fit.fit();
  } catch {
    // As above.
  }
  setInterval(() => post({ type: "ping" }), PING_MS);
  reconnect.addEventListener("click", connect);
  resume.addEventListener("click", restart);
  connect();
}

/// Pixels of a smooth (trackpad) scroll that count as one wheel notch.
const NOTCH_PX = 50;
/// How long the terminal stays connected with nothing typed: collie's idle
/// pause.
const IDLE_MS = 30 * 60 * 1000;
/// The font size the terminal draws at when the pane fits, and the smallest
/// it shrinks to for a pane larger than the window.
const FONT_PX = 13;
const MIN_FONT_PX = 4;

/// An item session's live terminal (#563): the same `api/term/<session>`
/// socket, which for an item is one viewer of the pane's control stream that
/// the factory shares among everyone watching, like a shared tmux session:
/// one viewer at a time holds control, and only its typing, paste and
/// wheel reach the pane, which takes its size; everyone else sees it at
/// that size, the font shrunk to fit, and has `take` (Take control). `name` (`@login`, or `extension`) is how
/// this viewer is listed in `viewers`, the list of who is watching. It is
/// opened only where the factory takes typing into the pane and this
/// factory's Writes switch is on; Writes going off closes it, as does half
/// an hour with nothing typed. herdr keeps the scrollback, so xterm keeps
/// none, each wheel notch is one scroll, and a paste is always bracketed.
export function runItem({ url, session, name, say, box, notice: noticeNode, viewers, reconnect, take }) {
  box.hidden = false;
  const term = new Terminal({
    cursorBlink: true,
    fontFamily:
      '"JetBrains Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
    fontSize: FONT_PX,
    macOptionIsMeta: true,
    scrollback: 0,
    theme: { background: "#0d1117", foreground: "#e6edf3" },
  });
  term.open(box);

  let port = null;
  let connected = false;
  let writes = false;
  /// The pane's size, as the factory last said it.
  let pane = null;
  let lastActivity = Date.now();
  let wheel = 0;
  let noticeTimer = null;
  /// Whether this window holds control of the pane, as the factory said.
  let control = false;

  const post = (message) => {
    try {
      port?.postMessage(message);
    } catch {
      // The port is gone; its disconnect is heard below.
    }
  };
  const ask = (message) => post({ type: "ask", data: JSON.stringify(message) });

  function notice(text, sticky = false) {
    clearTimeout(noticeTimer);
    noticeNode.textContent = text;
    noticeNode.hidden = !text;
    if (text && !sticky) noticeTimer = setTimeout(() => (noticeNode.hidden = true), 6000);
  }

  /// The room the box has for the terminal, and a cell's size at FONT_PX.
  function measure() {
    const style = getComputedStyle(box);
    const px = (key) => parseFloat(style.getPropertyValue(key)) || 0;
    const w = box.clientWidth - px("padding-left") - px("padding-right");
    const h = box.clientHeight - px("padding-top") - px("padding-bottom");
    const cell = term._core?._renderService?.dimensions?.css?.cell;
    if (!cell?.width || !cell?.height || w <= 0 || h <= 0) return null;
    const k = FONT_PX / term.options.fontSize;
    return { w, h, cw: cell.width * k, ch: cell.height * k };
  }

  /// The size this window would give the pane, in cells at FONT_PX.
  function mySize() {
    const m = measure();
    if (!m) return null;
    const cols = Math.max(1, Math.min(1000, Math.floor(m.w / m.cw)));
    const rows = Math.max(1, Math.min(1000, Math.floor(m.h / m.ch)));
    return { cols, rows };
  }

  /// Draw the pane at its size, the font shrunk until it fits the window.
  function fitPane() {
    if (!pane) return;
    if (term.cols !== pane.cols || term.rows !== pane.rows) term.resize(pane.cols, pane.rows);
    const m = measure();
    if (!m) return;
    const scale = Math.min(m.w / (pane.cols * m.cw), m.h / (pane.rows * m.ch));
    const font = Math.max(MIN_FONT_PX, Math.min(FONT_PX, Math.floor(FONT_PX * scale)));
    if (term.options.fontSize !== font) term.options.fontSize = font;
  }

  /// Tell the factory this window's size: the pane takes it while this
  /// window holds control, and it is kept for when it takes control.
  function claimSize() {
    const size = mySize();
    const text = size && resizeMessage(size.cols, size.rows);
    if (text && connected && (size.cols !== pane?.cols || size.rows !== pane?.rows)) {
      post({ type: "resize", data: text });
    }
  }

  function type(bytes) {
    if (!connected) return;
    if (!control) {
      notice("View only: Take control to type here.");
      return;
    }
    lastActivity = Date.now();
    claimSize();
    post({ type: "input", data: toBase64(bytes) });
  }

  function closed(text) {
    const held = port;
    port = null;
    connected = false;
    held?.disconnect();
    viewers.textContent = "";
    control = false;
    take.hidden = true;
    say(text, true);
    reconnect.hidden = false;
    term.write(`\r\n\x1b[2m[${text}]\x1b[0m\r\n`);
  }

  function heard(message) {
    if (message?.type === "open") {
      connected = true;
      lastActivity = Date.now();
      term.reset();
      say("live");
      // The name is for the viewer list only; the size is the pane's only if
      // no one else is watching it yet.
      ask({ type: "hello", name, ...(mySize() ?? {}) });
      term.focus();
    } else if (message?.type === "data") {
      term.write(fromBase64(message.data));
    } else if (message?.type === "text") {
      let said;
      try {
        said = JSON.parse(message.data);
      } catch {
        return;
      }
      if (said.type === "size") {
        pane = { cols: said.cols, rows: said.rows };
        fitPane();
        // The controller's window is the pane's size.
        if (control) claimSize();
      } else if (said.type === "viewers") {
        viewers.textContent = (Array.isArray(said.names) ? said.names : []).map(String).join(", ");
        const had = control;
        control = said.control === true;
        const holder = typeof said.controller === "string" ? said.controller : "no one";
        say(control ? "live · in control" : `view only · ${holder} in control`);
        take.hidden = control;
        if (control && !had) claimSize();
        else if (had && !control) notice(`${holder} took control: this terminal is view only.`);
      } else if (said.type === "notice") {
        notice(String(said.text ?? ""));
      }
    } else if (message?.type === "closed") {
      const why = message.error ?? (message.reason || null);
      closed(why ? `the terminal closed (${why})` : "the terminal closed");
    }
  }

  function connect() {
    port?.disconnect();
    reconnect.hidden = true;
    connected = false;
    notice("");
    if (!writes) {
      closed("writes are off for this factory on the options page");
      return;
    }
    say("connecting…");
    const opened = chrome.runtime.connect({ name: "ssf-term" });
    port = opened;
    opened.onMessage.addListener((message) => port === opened && heard(message));
    opened.onDisconnect.addListener(() => {
      if (port === opened) closed("the terminal closed: the extension's service worker stopped");
    });
    opened.postMessage({ type: "open", url, session });
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
      ask({ type: "scroll", direction: notches < 0 ? "up" : "down" });
    }
    if (notches) lastActivity = Date.now();
    return false;
  });

  // A resize of this window is a resize of the pane; the first call is the
  // observer starting, not a resize.
  let timer = null;
  let observed = false;
  new ResizeObserver(() => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (!observed) {
        observed = true;
        return;
      }
      claimSize();
      fitPane();
    }, FIT_MS);
  }).observe(box);

  reconnect.addEventListener("click", connect);
  take.addEventListener("click", () => {
    lastActivity = Date.now();
    ask({ type: "take" });
    term.focus();
  });
  setInterval(() => {
    if (port && Date.now() - lastActivity >= IDLE_MS) {
      closed("disconnected: nothing was typed for a while");
    }
  }, 30000);
  setInterval(() => post({ type: "ping" }), PING_MS);

  // The factory's Writes switch, followed as the options page changes it.
  const readWrites = (factories) => {
    const item = (factories ?? []).find((one) => factoryUrl(one?.url) === url);
    writes = item !== undefined && item.writes !== false;
    if (!writes && port) closed("writes were turned off for this factory");
  };
  chrome.storage.local.get("factories").then(({ factories }) => {
    readWrites(factories);
    connect();
  });
  chrome.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes.factories) readWrites(changes.factories.newValue);
  });
  term.focus();
}

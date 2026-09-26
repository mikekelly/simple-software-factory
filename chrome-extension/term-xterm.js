// A scratch session's live terminal (#491), and an item session's while Type
// is on (runItem, #563, below): xterm.js (vendored, see the
// README) attached to the factory's `api/term/<session>` WebSocket through
// the service worker, on a port (`ssf-term`). What the terminal prints comes
// as bytes, what is typed goes as bytes, and the terminal is sized to its
// window: every change of size (debounced) is fitted and sent as a resize,
// which the tmux session follows. A view-only terminal neither types nor
// resizes the session.
import { Terminal } from "./vendor/xterm/xterm.mjs";
import { FitAddon } from "./vendor/xterm/addon-fit.mjs";
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
/// How long Type stays on with nothing typed: collie's idle pause.
const IDLE_MS = 30 * 60 * 1000;

/// An item session's terminal while Type is on (#563): the same
/// `api/term/<session>` socket, which for an item streams the pane through
/// herdr, with the dashboard's terminal logic (dashboard/terminal.js) on the
/// port. Read-only, the page shows the pane mirror instead (terminal.js); this
/// terminal is opened only to type. `enter` connects and asks the factory for
/// control, which it gives only to this extension and only where
/// `item_pane_input` is on; the port passes it (and what is typed) only while
/// the factory's Writes switch is on (term-wire.js). In control the pane takes
/// this terminal's size. `leave` releases the pane and closes the socket, and
/// `onLeave(why)` hands the page back to the mirror: Type off, the tab
/// hidden, nothing typed for a while, Writes off (terminal.js), control
/// refused or taken over elsewhere (never taken back: no `--takeover`), or the
/// socket closed. herdr keeps the scrollback, so xterm keeps none, each wheel
/// notch is one scroll, and a paste is always bracketed.
export function runItem({ url, session, say, box, notice, onLeave }) {
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
  box.hidden = false;
  term.open(box);
  box.hidden = true;

  let port = null;
  let control = false;
  let lastActivity = Date.now();
  let wheel = 0;

  const post = (message) => {
    try {
      port?.postMessage(message);
    } catch {
      // The port is gone; its disconnect is heard below.
    }
  };
  const ask = (message) => post({ type: "ask", data: JSON.stringify(message) });
  const fitted = () => {
    try {
      fit.fit();
    } catch {
      // A box with no size (hidden) has nothing to fit.
    }
  };

  function sendSize() {
    const size = fit.proposeDimensions();
    const text = size && resizeMessage(size.cols, size.rows);
    if (text && control) post({ type: "resize", data: text });
  }

  /// Bytes typed at the pane; nothing is sent until control is given.
  function type(bytes) {
    if (!control) return notice("Nothing was sent: waiting for control of the pane.");
    lastActivity = Date.now();
    post({ type: "input", data: toBase64(bytes) });
  }

  /// Give the pane back and hand the page to the mirror; `why`, when it was
  /// not the person who turned Type off, is said.
  function leave(why) {
    if (!port) return;
    ask({ type: "release" });
    const held = port;
    port = null;
    control = false;
    held.disconnect();
    box.hidden = true;
    onLeave(why);
  }

  function heard(message) {
    if (message?.type === "open") {
      term.reset();
      say("waiting for control of the pane…");
    } else if (message?.type === "data") {
      term.write(fromBase64(message.data));
    } else if (message?.type === "text") {
      let said;
      try {
        said = JSON.parse(message.data);
      } catch {
        return;
      }
      if (said.type === "mode") {
        control = said.control === true;
        if (said.may_control !== true) return leave("the factory does not take typing into this pane");
        // Control is asked for as the stream starts, and again when it
        // comes back read-only (the pane was found again).
        if (!control) ask({ type: "control" });
        else {
          fitted();
          sendSize();
          say("live · typing");
          term.focus();
        }
      } else if (said.type === "size" && !control) {
        term.resize(said.cols, said.rows);
      } else if (said.type === "refused") {
        leave(String(said.reason ?? "the factory refused control"));
      } else if (said.type === "notice") {
        notice(String(said.text ?? ""));
      }
    } else if (message?.type === "closed") {
      const why = message.error ?? (message.reason || null);
      leave(why ? `the terminal closed (${why})` : "the terminal closed");
    }
  }

  function enter() {
    if (port) return;
    control = false;
    lastActivity = Date.now();
    box.hidden = false;
    fitted();
    say("connecting…");
    const opened = chrome.runtime.connect({ name: "ssf-term" });
    port = opened;
    opened.onMessage.addListener((message) => port === opened && heard(message));
    opened.onDisconnect.addListener(() => {
      if (port === opened) leave("the extension's service worker stopped");
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
    if (!control) return false;
    // A mouse wheel moves in lines or whole notches; a trackpad in pixels.
    const notches =
      event.deltaMode === 0 ? Math.trunc((wheel += event.deltaY) / NOTCH_PX) : Math.sign(event.deltaY);
    if (event.deltaMode === 0) wheel -= notches * NOTCH_PX;
    for (let i = 0; i < Math.abs(notches); i += 1) {
      ask({ type: "scroll", direction: notches < 0 ? "up" : "down" });
    }
    lastActivity = Date.now();
    return false;
  });

  let timer = null;
  new ResizeObserver(() => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      if (!control) return;
      fitted();
      sendSize();
    }, FIT_MS);
  }).observe(box);

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") leave("the tab was hidden");
  });
  setInterval(() => {
    if (port && Date.now() - lastActivity >= IDLE_MS) leave("nothing was typed for a while");
  }, 30000);
  setInterval(() => post({ type: "ping" }), PING_MS);

  return { enter, leave, typing: () => port !== null };
}

// A scratch session's live terminal (#491): xterm.js (vendored, see the
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
  // Spike (#561): an item session's herdr pane. Its frames position the
  // cursor absolutely and repaint in place, so xterm keeps no scrollback;
  // herdr keeps it, and the wheel scrolls herdr's.
  const herdr = session.includes("#");
  const term = new Terminal({
    cursorBlink: true,
    disableStdin: !takesInput,
    fontFamily:
      '"JetBrains Mono", "Cascadia Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace',
    fontSize: 13,
    scrollback: herdr ? 0 : 5000,
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
  if (herdr && takesInput) {
    // Outside mouse mode the wheel scrolls herdr's scrollback; in it (a
    // full-screen TUI), xterm reports the wheel to the app itself.
    term.attachCustomWheelEventHandler((event) => {
      if (term.modes.mouseTrackingMode !== "none" || !connected || event.deltaY === 0) return true;
      const lines = Math.max(1, Math.round(Math.abs(event.deltaY) / 40));
      const direction = event.deltaY < 0 ? "up" : "down";
      post({ type: "scroll", data: JSON.stringify({ type: "scroll", lines, direction }) });
      return false;
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

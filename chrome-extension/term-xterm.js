// A scratch session's live terminal (#491), and an item session's (runItem,
// #563, below): xterm.js (vendored, see the
// README) attached to the factory's `api/term/<session>` WebSocket through
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
/// How long Type stays on with nothing typed: collie's idle pause.
const IDLE_MS = 30 * 60 * 1000;

/// An item session's live terminal (#563): the same `api/term/<session>`
/// socket, which for an item streams the pane through herdr, with the
/// dashboard's terminal logic (dashboard/terminal.js) on the port. It opens
/// read-only at the pane's own size, and anything typed or pasted then is not
/// sent: a notice says so. Type asks the factory for control, which it gives
/// only to this extension and only where `item_pane_input` is on; the port
/// passes it (and what is typed) only while the factory's Writes switch is on
/// (term-wire.js). Type turns itself off when the tab is hidden, nothing is
/// typed for a while, Writes goes off, or the pane is taken over elsewhere
/// (never taken back: no `--takeover`). herdr keeps the scrollback, so xterm
/// keeps none, each wheel notch is one scroll, and a paste is always
/// bracketed.
export function runItem({ url, session, takesInput, say, box, notice: noticeNode, typeButton, reconnect }) {
  box.hidden = false;
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

  let port = null;
  let control = false;
  let mayControl = false;
  let typing = false;
  let writes = false;
  let lastActivity = Date.now();
  let wheel = 0;
  let noticeTimer = null;

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
  const canType = () => takesInput && writes && mayControl;

  function notice(text, sticky = false) {
    clearTimeout(noticeTimer);
    noticeNode.textContent = text;
    noticeNode.hidden = !text;
    if (text && !sticky) noticeTimer = setTimeout(() => (noticeNode.hidden = true), 6000);
  }

  function shown() {
    if (port) say(control ? "live · typing" : "live · read-only");
    typeButton.hidden = !canType();
    typeButton.setAttribute("aria-pressed", String(typing));
  }

  function sendSize() {
    const size = fit.proposeDimensions();
    const text = size && resizeMessage(size.cols, size.rows);
    if (text && control) post({ type: "resize", data: text });
  }

  /// Bytes typed at the pane, or the read-only notice instead.
  function type(bytes) {
    if (!control) {
      notice(
        canType()
          ? "Read-only: nothing was sent. Turn on Type to type into this pane."
          : "Read-only: nothing was sent. " +
              (!takesInput
                ? "Comment on the item to speak to its agent."
                : !writes
                  ? "Writes are off for this factory on the options page."
                  : "The factory does not take typing into this pane."),
      );
      return;
    }
    lastActivity = Date.now();
    post({ type: "input", data: toBase64(bytes) });
  }

  function setTyping(on, why) {
    if (typing === on) return;
    typing = on;
    lastActivity = Date.now();
    ask({ type: on ? "control" : "release" });
    if (!on && why) notice(`Type is off: ${why}.`, true);
    else if (on) notice("");
    shown();
  }

  function closed(text) {
    const held = port;
    port = null;
    held?.disconnect();
    control = false;
    typing = false;
    shown();
    say(text, true);
    reconnect.hidden = false;
    term.write(`\r\n\x1b[2m[${text}]\x1b[0m\r\n`);
  }

  function heard(message) {
    if (message?.type === "open") {
      term.reset();
      shown();
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
        mayControl = said.may_control === true;
        // A stream that came back read-only (the pane was found again) is
        // asked for control again while Type is on.
        if (typing && !control && canType()) ask({ type: "control" });
        if (control) {
          fitted();
          sendSize();
        }
        shown();
      } else if (said.type === "size" && !control) {
        term.resize(said.cols, said.rows);
      } else if (said.type === "refused") {
        // Refused, or taken over elsewhere: Type goes off, and stays off.
        typing = false;
        shown();
        notice(`Type is off: ${said.reason}`, true);
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
    control = false;
    typing = false;
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
      if (control) fitted();
      sendSize();
    }, FIT_MS);
  }).observe(box);

  typeButton.addEventListener("click", () => {
    setTyping(!typing);
    term.focus();
  });
  reconnect.addEventListener("click", connect);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") setTyping(false, "the tab was hidden");
  });
  setInterval(() => {
    if (typing && Date.now() - lastActivity >= IDLE_MS) setTyping(false, "nothing was typed for a while");
  }, 30000);
  setInterval(() => post({ type: "ping" }), PING_MS);

  // The factory's Writes switch, followed as the options page changes it.
  const readWrites = (factories) => {
    const item = (factories ?? []).find((one) => factoryUrl(one?.url) === url);
    writes = item?.writes !== false;
    if (!writes) setTyping(false, "writes were turned off for this factory");
    shown();
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

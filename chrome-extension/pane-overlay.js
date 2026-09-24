// Open, and the terminal windows it floats over the page (#477, #491).
//
// Loaded as a content script after pane-geometry.js and before writes-form.js
// and content.js, in the same isolated world. Both draw Open with
// `ssfPane.button(...)` -- the terminal icon at the top of an agent's card and
// of a scratch session's row -- and adopt `STYLE` into their shadow sheet for it:
//
//   const open = ssfPane.button(factoryUrl, session, input);
//
// A click opens the session's terminal (terminal.html) in a window of its own
// floating over the GitHub page: not modal, so the page underneath still
// scrolls and takes clicks. The window is dragged by its title bar, resized
// from its bottom-right corner, kept inside the viewport and closed with its
// ×, or Esc while the title bar has focus (Esc in the terminal is the
// terminal's). Each session has at most one window: Open on a session that
// already has one brings it to the front. The last place a window was moved
// or sized to is remembered (chrome.storage.local) for the next.
//
// The frame is the extension's own page, listed in the manifest's
// web_accessible_resources for github.com only, so what it reads still carries
// the extension's origin and the permission the options page granted; it
// refuses a factory the options page does not hold.
(() => {
  const SVG_NS = "http://www.w3.org/2000/svg";
  const geometry = globalThis.ssfPaneGeometry;
  /// Where the last window moved or sized to is kept.
  const RECT_KEY = "paneWindowRect";


  /// The button, in the card's own shadow sheet: an icon that takes no more
  /// room than the line it sits at the end of. The selectors match the forms'
  /// own button rules, which a scratch row's Open sits inside of, and this sheet
  /// comes after theirs.
  const STYLE = `
button.ssf-pane-open { flex: none; display: inline-flex; align-items: center;
  justify-content: center; margin-left: auto; padding: 2px; border: 0;
  border-radius: 6px; background: none; color: var(--fgColor-muted, #59636e);
  cursor: pointer; }
button.ssf-pane-open:hover:not(:disabled) {
  background: var(--bgColor-neutral-muted, #afb8c133);
  color: var(--fgColor-default, #1f2328); }
/* Show agent: the same Open, labelled, leading a card's Actions row. */
button.ssf-pane-open.ssf-pane-show { flex: 1 1 auto; gap: 6px; margin-left: 0;
  padding: 3px 8px; font-weight: 500; color: var(--fgColor-default, #1f2328);
  background: var(--bgColor-muted, #f6f8fa);
  border: 1px solid var(--borderColor-default, #d1d9e0); }
button.ssf-pane-open.ssf-pane-show:hover:not(:disabled) {
  background: var(--bgColor-neutral-muted, #afb8c133); }
.ssf-pane-open:focus-visible { outline: 2px solid var(--fgColor-accent, #0969da);
  outline-offset: 1px; }
`;

  /// A window's own sheet: a dark panel like the terminal inside it, above
  /// GitHub's own overlays and the chip popover. The host is the panel.
  const OVERLAY_STYLE = `
:host { position: fixed; z-index: 2147483600; display: flex; flex-direction: column;
  box-sizing: border-box; overflow: hidden; border: 1px solid #30363d; border-radius: 8px;
  background: #0d1117; box-shadow: 0 8px 24px rgba(1, 4, 9, 0.5); }
.ssf-pane-head { flex: none; display: flex; align-items: center; gap: 8px;
  padding: 4px 6px 4px 12px; border-bottom: 1px solid #30363d; color: #e6edf3;
  cursor: move; user-select: none; touch-action: none; font: 600 12px -apple-system,
  BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif; }
.ssf-pane-head:focus-visible { outline: 2px solid #1f6feb; outline-offset: -2px; }
.ssf-pane-head span { flex: 1 1 auto; min-width: 0; overflow: hidden;
  text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, SFMono-Regular,
  "SF Mono", Menlo, Consolas, "Liberation Mono", monospace; }
.ssf-pane-close { flex: none; width: 24px; height: 24px; padding: 0; border: 0;
  border-radius: 6px; background: none; color: #8b949e; font: 18px/24px sans-serif;
  cursor: pointer; }
.ssf-pane-close:hover { background: #21262d; color: #e6edf3; }
.ssf-pane-frame { flex: 1 1 auto; width: 100%; min-height: 0; border: 0; background: #0d1117; }
.ssf-pane-grip { position: absolute; right: 0; bottom: 0; width: 14px; height: 14px;
  cursor: nwse-resize; touch-action: none;
  background: linear-gradient(135deg, transparent 50%, #484f58 50%, #484f58 60%,
    transparent 60%, transparent 75%, #484f58 75%, #484f58 85%, transparent 85%); }
/* While dragging, the frame does not take the pointer from the drag. */
:host([data-moving]) .ssf-pane-frame { pointer-events: none; }
`;

  let sheet = null;
  /// The open windows, by factory and session.
  const windows = new Map();
  /// The stacking order: each window brought to the front takes the next.
  let top = 2147483000;
  /// The remembered rect, read once.
  let saved = null;
  const loaded = storageGet();

  function storageGet() {
    try {
      return chrome.storage.local
        .get(RECT_KEY)
        .then((stored) => {
          saved = stored?.[RECT_KEY] ?? null;
        })
        .catch(() => {});
    } catch {
      // No storage (an extension reloaded under the page): nothing remembered.
      return Promise.resolve();
    }
  }

  function remember(rect) {
    saved = rect;
    try {
      chrome.storage.local.set({ [RECT_KEY]: rect }).catch(() => {});
    } catch {
      // As above: the rect is kept for this page only.
    }
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function terminalIcon() {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 16 16");
    svg.setAttribute("width", "16");
    svg.setAttribute("height", "16");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "1.3");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    for (const [tag, attributes] of [
      ["rect", { x: 1.5, y: 2.5, width: 13, height: 11, rx: 1.5 }],
      ["path", { d: "M4.5 6.5 6.5 8.5 4.5 10.5" }],
      ["path", { d: "M8.5 10.5h3" }],
    ]) {
      const node = document.createElementNS(SVG_NS, tag);
      for (const [name, value] of Object.entries(attributes)) {
        node.setAttribute(name, String(value));
      }
      svg.append(node);
    }
    return svg;
  }

  /// Open for one session. `input` is the snapshot's `pane_input`: whether the
  /// factory lets a person type there. The handler is an `onclick` property,
  /// which content.js copies onto a button it keeps between frames. `label`,
  /// where given, is drawn beside the icon: a card's Show agent.
  function button(factoryUrl, session, input, label) {
    const open = element("button", label ? "ssf-pane-open ssf-pane-show" : "ssf-pane-open");
    open.type = "button";
    open.title = label ? "Open this agent's terminal" : "Open terminal";
    open.setAttribute("aria-label", label ?? "Open terminal");
    open.append(terminalIcon());
    if (label) open.append(label);
    open.onclick = (event) =>
      show(factoryUrl, String(session ?? ""), input === true, event.currentTarget);
    return open;
  }


  /// The stacking range the windows use: the top of it is GitHub's own
  /// overlays' and the chip popover's ceiling and beyond.
  const Z_BASE = 2147483000;
  const Z_TOP = 2147483600;

  /// Bring `win` to the front. Near the end of the range the windows are
  /// numbered again from its start, in the order they stood.
  function raise(win) {
    if (win.z === top) return;
    if (top >= Z_TOP) {
      top = Z_BASE;
      const order = [...windows.values()].sort((a, b) => a.z - b.z);
      for (const other of order) {
        if (other === win) continue;
        other.z = ++top;
        other.host.style.zIndex = String(other.z);
      }
    }
    win.z = ++top;
    win.host.style.zIndex = String(win.z);
  }

  function place(win, rect) {
    win.rect = geometry.clampRect(rect, innerWidth, innerHeight);
    const { style } = win.host;
    style.left = `${win.rect.x}px`;
    style.top = `${win.rect.y}px`;
    style.width = `${win.rect.width}px`;
    style.height = `${win.rect.height}px`;
  }

  /// A pointer drag on `handle` that changes the window's rect by `change`,
  /// given how far the pointer moved; the rect is remembered once it ends.
  function dragging(win, handle, change) {
    handle.addEventListener("pointerdown", (event) => {
      if (event.button !== 0 || event.target.closest?.(".ssf-pane-close")) return;
      event.preventDefault();
      raise(win);
      const start = { ...win.rect };
      const { clientX, clientY } = event;
      handle.setPointerCapture(event.pointerId);
      win.host.dataset.moving = "";
      const move = (moved) =>
        place(win, change(start, moved.clientX - clientX, moved.clientY - clientY));
      const end = () => {
        handle.removeEventListener("pointermove", move);
        handle.removeEventListener("pointerup", end);
        handle.removeEventListener("pointercancel", end);
        delete win.host.dataset.moving;
        remember(win.rect);
      };
      handle.addEventListener("pointermove", move);
      handle.addEventListener("pointerup", end);
      handle.addEventListener("pointercancel", end);
    });
  }

  async function show(factoryUrl, session, input, opener) {
    const key = `${factoryUrl}\n${session}`;
    const open = windows.get(key);
    if (open) {
      raise(open);
      open.frame.focus();
      return;
    }
    await loaded;
    if (windows.has(key)) return show(factoryUrl, session, input, opener);
    const query = new URLSearchParams({ factory: factoryUrl, session, input: input ? "1" : "0" });
    const host = element("div");
    host.dataset.ssfPane = session;
    const shadow = host.attachShadow({ mode: "open" });
    if (!sheet) {
      sheet = new CSSStyleSheet();
      sheet.replaceSync(OVERLAY_STYLE);
    }
    shadow.adoptedStyleSheets = [sheet];
    host.setAttribute("role", "dialog");
    host.setAttribute("aria-label", `Terminal: ${session}`);

    const head = element("div", "ssf-pane-head");
    head.tabIndex = 0;
    const shut = element("button", "ssf-pane-close", "×");
    shut.type = "button";
    shut.title = "Close terminal";
    shut.setAttribute("aria-label", "Close terminal");
    head.append(element("span", undefined, session), shut);
    const frame = element("iframe", "ssf-pane-frame");
    frame.title = `Terminal: ${session}`;
    frame.src = `${chrome.runtime.getURL("terminal.html")}?${query}`;
    const grip = element("div", "ssf-pane-grip");
    grip.setAttribute("aria-hidden", "true");
    shadow.append(head, frame, grip);

    const win = { host, frame, opener, z: 0, rect: null };
    shut.onclick = () => close(key);
    // Esc closes the window from its title bar; in the terminal it is a key.
    head.addEventListener("keydown", (event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      close(key);
    });
    // A click anywhere in it -- the frame too, which the page hears only as
    // focus moving into it -- brings it to the front.
    host.addEventListener("pointerdown", () => raise(win), true);
    host.addEventListener("focusin", () => raise(win));
    dragging(win, head, (start, dx, dy) => ({ ...start, x: start.x + dx, y: start.y + dy }));
    dragging(win, grip, (start, dx, dy) => ({
      ...start,
      width: start.width + dx,
      height: start.height + dy,
    }));

    place(win, geometry.placeRect(saved, windows.size, innerWidth, innerHeight));
    windows.set(key, win);
    raise(win);
    document.body.append(host);
    frame.focus();
  }

  function close(key) {
    const win = windows.get(key);
    if (!win) return;
    windows.delete(key);
    win.host.remove();
    if (win.opener?.isConnected) win.opener.focus({ preventScroll: true });
  }

  function closeAll() {
    for (const key of [...windows.keys()]) close(key);
  }

  // A smaller viewport keeps every window on screen.
  addEventListener("resize", () => {
    for (const win of windows.values()) place(win, win.rect);
  });

  /// Open (or bring forward) a session's window without a button: a scratch
  /// session New scratch has just started.
  function open(factoryUrl, session, input) {
    show(factoryUrl, String(session ?? ""), input === true, null);
  }

  globalThis.ssfPane = { STYLE, button, open, close: closeAll };
})();

// Open, and the terminal it shows over the page (#477).
//
// Loaded as a content script before writes-form.js and content.js, in the same
// isolated world. Both draw Open with `ssfPane.button(...)` -- the terminal icon
// at the top of an agent's card and of a scratch session's row -- and adopt
// `STYLE` into their shadow sheet for it:
//
//   const open = ssfPane.button(factoryUrl, session, input);
//
// A click lays the pane mirror (terminal.html) over the GitHub page in a frame,
// rather than in a tab of its own, so the item the person was reading is where
// they left it once they close it: the close button, a click outside the
// panel, or Esc. Esc typed into the pane -- while its Type is on -- is the
// agent's, since an agent reads it as a key; then it closes only from outside
// the terminal.
//
// The frame is the extension's own page, listed in the manifest's
// web_accessible_resources for github.com only, so the stream it reads still
// carries the extension's origin and the permission the options page granted,
// exactly as it did in a tab; it refuses a factory the options page does not
// hold. While it is open the page underneath does not scroll: the wheel over
// the terminal is the terminal's.
//
// The panel is 90% of the window's width and 85% of its height: the terminal
// wraps its text to whatever width it has, so it needs no size of its own.
(() => {
  const SVG_NS = "http://www.w3.org/2000/svg";

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

  /// The overlay's own sheet: a dark panel like the terminal inside it, over a
  /// dimmed page, above GitHub's own overlays and the chip popover.
  const OVERLAY_STYLE = `
:host { position: fixed; inset: 0; z-index: 2147483600; }
.ssf-pane-backdrop { position: fixed; inset: 0; display: flex; align-items: center;
  justify-content: center; background: rgba(1, 4, 9, 0.6); }
.ssf-pane-panel { display: flex; flex-direction: column; width: 90vw; height: 85vh;
  overflow: hidden; border: 1px solid #30363d; border-radius: 8px; background: #0d1117;
  box-shadow: 0 8px 24px rgba(1, 4, 9, 0.5); }
.ssf-pane-head { display: flex; align-items: center; gap: 8px; padding: 6px 8px 6px 12px;
  border-bottom: 1px solid #30363d; color: #e6edf3; font: 600 12px -apple-system,
  BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif; }
.ssf-pane-head span { flex: 1 1 auto; min-width: 0; overflow: hidden;
  text-overflow: ellipsis; white-space: nowrap; }
.ssf-pane-close { flex: none; width: 24px; height: 24px; padding: 0; border: 0;
  border-radius: 6px; background: none; color: #8b949e; font: 18px/24px sans-serif;
  cursor: pointer; }
.ssf-pane-close:hover { background: #21262d; color: #e6edf3; }
.ssf-pane-frame { flex: 1 1 auto; width: 100%; border: 0; background: #0d1117; }
`;

  let sheet = null;
  /// The open overlay, or null: one at a time.
  let shown = null;

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  /// A terminal: a window with a prompt in it.
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
    open.title = "Open terminal";
    open.setAttribute("aria-label", label ?? "Open terminal");
    open.append(terminalIcon());
    if (label) open.append(label);
    open.onclick = (event) =>
      show(factoryUrl, String(session ?? ""), input === true, event.currentTarget);
    return open;
  }

  function show(factoryUrl, session, input, opener) {
    close();
    const query = new URLSearchParams({ factory: factoryUrl, session, input: input ? "1" : "0" });
    const host = element("div");
    host.dataset.ssfPane = session;
    const shadow = host.attachShadow({ mode: "open" });
    if (!sheet) {
      sheet = new CSSStyleSheet();
      sheet.replaceSync(OVERLAY_STYLE);
    }
    shadow.adoptedStyleSheets = [sheet];

    const backdrop = element("div", "ssf-pane-backdrop");
    const panel = element("div", "ssf-pane-panel");
    panel.setAttribute("role", "dialog");
    panel.setAttribute("aria-modal", "true");
    panel.setAttribute("aria-label", `Terminal: ${session}`);
    const head = element("div", "ssf-pane-head");
    const shut = element("button", "ssf-pane-close", "×");
    shut.type = "button";
    shut.title = "Close terminal";
    shut.setAttribute("aria-label", "Close terminal");
    shut.onclick = close;
    // The session is named on the terminal's own first line, just below.
    head.append(element("span", undefined, "Terminal"), shut);
    const frame = element("iframe", "ssf-pane-frame");
    frame.title = `Terminal: ${session}`;
    frame.src = `${chrome.runtime.getURL("terminal.html")}?${query}`;
    panel.append(head, frame);
    backdrop.append(panel);
    backdrop.addEventListener("click", (event) => {
      if (event.target === backdrop) close();
    });
    shadow.append(backdrop);

    // Esc from the page, and from the terminal, which cannot reach this
    // document's keys and says so from inside its frame.
    const onKey = (event) => {
      if (event.key !== "Escape") return;
      event.stopPropagation();
      close();
    };
    const onMessage = (event) => {
      if (event.source !== frame.contentWindow) return;
      if (event.data?.type === "ssf:pane-close") close();
    };
    addEventListener("keydown", onKey, true);
    addEventListener("message", onMessage);
    // The page underneath stays where it is: a wheel that runs off the end of
    // the terminal would otherwise scroll it.
    const root = document.documentElement;
    const overflow = root.style.overflow;
    root.style.overflow = "hidden";

    document.body.append(host);
    frame.focus();
    shown = {
      host,
      opener,
      undo() {
        removeEventListener("keydown", onKey, true);
        removeEventListener("message", onMessage);
        root.style.overflow = overflow;
      },
    };
  }

  function close() {
    if (!shown) return;
    const { host, opener, undo } = shown;
    shown = null;
    undo();
    host.remove();
    if (opener?.isConnected) opener.focus();
  }

  globalThis.ssfPane = { STYLE, button, close };
})();

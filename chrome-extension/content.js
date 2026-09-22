// Overlays live ssf state on github.com.
//
// Read-only: it renders the factory's own dashboard model (`cards` and
// `monitored_items` from the server's canonical status, see src/status.rs).
// Every screen answers one question -- is an agent on this, and does it need
// me? -- in one glance, and keeps the detail one click away:
//
//   * an issue or pull request page gets a card in the right sidebar, above
//     Assignees;
//   * issue and pull request lists, search results and project boards get one
//     chip per tracked item, and a click on a chip opens the same card in a
//     popover;
//   * every other page gets nothing.
//
// The five user-facing states are fixed in colour and icon so they read the
// same everywhere; the word ssf and the harness actually use is in every
// tooltip, and a stale or unreachable factory is a modifier on top rather than
// a state of its own, so a stale snapshot can never be mistaken for a live one.
//
// It never acts on the factory and never renders factory text as HTML: every
// node is built with textContent.
(() => {
  if (window.__ssfOverlayInstalled) return;
  window.__ssfOverlayInstalled = true;

  /// An issue or pull request page, including its subpages (`/pull/5/files`).
  const DETAIL_PATH = /^\/([^/]+)\/([^/]+)\/(issues|pull)\/(\d+)(?:\/|$)/;
  /// A link to exactly one issue or pull request, as lists and boards make them.
  const LINK_PATH = /^\/([^/]+)\/([^/]+)\/(?:issues|pull)\/(\d+)\/?$/;
  /// The only pages besides a detail page that carry a chip: the issue and pull
  /// request lists, search results, and project boards. Anywhere else gets
  /// nothing, however many issue links the page happens to contain.
  const CHIP_PATHS = [
    /^\/(?:issues|pulls)(?:\/|$)/,
    /^\/[^/]+\/[^/]+\/(?:issues|pulls)(?:\/|$)/,
    /^\/search\/?$/,
    /^\/(?:users|orgs)\/[^/]+\/projects\/\d+(?:\/|$)/,
    /^\/[^/]+\/[^/]+\/projects\/\d+(?:\/|$)/,
  ];
  const PING_MS = 20000;
  const RENDER_DEBOUNCE_MS = 200;
  const POPOVER_WIDTH = 320;

  const SVG_NS = "http://www.w3.org/2000/svg";

  const STYLE = `
:host { display: block; }
:host([data-ssf-slot="sidebar"]) { margin-bottom: 16px; }
:host([data-ssf-slot="sidebar-legacy"]) {
  margin-bottom: 16px; padding-top: 16px;
  border-top: 1px solid var(--borderColor-muted, #d1d9e0); }
:host([data-ssf-list]) { display: inline-flex; vertical-align: middle; }
:host([data-ssf-popover]) { position: fixed; z-index: 2147483000; }
.ssf-section, .ssf-popover { font-family: -apple-system, BlinkMacSystemFont,
  "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif; font-size: 12px;
  line-height: 1.5; text-align: left; color: var(--fgColor-default, #1f2328); }
.ssf-title { margin: 0 0 8px; font-size: 12px; font-weight: 600;
  color: var(--fgColor-muted, #59636e); }
.ssf-popover { box-sizing: border-box; width: min(${POPOVER_WIDTH}px, calc(100vw - 16px));
  padding: 10px 12px; border-radius: 8px;
  /* A card's message can be thousands of characters, and expanding it must not
     grow the popover past the viewport, or its own less toggle and Details
     would be out of reach. Border-box, so this is the whole box and
     placePopover's clamp always finds room for it. */
  max-height: calc(100vh - 16px); overflow: auto;
  border: 1px solid var(--borderColor-default, #d1d9e0);
  background: var(--bgColor-default, #ffffff);
  box-shadow: 0 8px 24px rgba(31, 35, 40, 0.2); }
.ssf-card { padding: 8px 10px; border-radius: 6px;
  border: 1px solid var(--borderColor-default, #d1d9e0);
  background: var(--bgColor-muted, #f6f8fa); }
.ssf-card + .ssf-card { margin-top: 8px; }
.ssf-via { margin-bottom: 2px; color: var(--fgColor-muted, #59636e); }
.ssf-state { display: flex; align-items: baseline; gap: 6px; min-width: 0; }
.ssf-word { font-weight: 600; }
.ssf-when { font-weight: 400; color: var(--fgColor-muted, #59636e); }
.ssf-when[data-stale="true"] { font-weight: 600; color: #9a6700; }
.ssf-stack { color: var(--fgColor-muted, #59636e); }
.ssf-said { margin-top: 6px; }
.ssf-message { overflow-wrap: anywhere; white-space: pre-wrap; }
.ssf-message[data-clamped="true"] { display: -webkit-box; -webkit-box-orient: vertical;
  -webkit-line-clamp: 2; overflow: hidden; white-space: normal; }
.ssf-more { margin: 2px 0 0; padding: 0; border: 0; background: none;
  font: inherit; font-weight: 600; color: var(--fgColor-accent, #0969da);
  cursor: pointer; }
.ssf-also { margin-top: 6px; color: var(--fgColor-muted, #59636e); }
.ssf-also a { color: var(--fgColor-accent, #0969da); text-decoration: none; }
.ssf-also a:hover { text-decoration: underline; }
.ssf-details { margin-top: 6px; }
.ssf-details summary { font-weight: 600; color: var(--fgColor-muted, #59636e);
  cursor: pointer; }
.ssf-details dl { display: grid; grid-template-columns: auto 1fr; gap: 2px 8px;
  margin: 4px 0 0; }
.ssf-details dt { color: var(--fgColor-muted, #59636e); }
.ssf-details dd { margin: 0; overflow-wrap: anywhere; }
.ssf-chip { display: inline-flex; align-items: center; gap: 4px; margin-left: 6px;
  padding: 0 6px; border: 1px solid var(--borderColor-default, #d1d9e0);
  border-radius: 999px; font-family: inherit; font-size: 11px; line-height: 18px;
  font-weight: 500; color: var(--fgColor-muted, #59636e); vertical-align: middle;
  white-space: nowrap; cursor: pointer; }
.ssf-chip:hover { background: var(--bgColor-muted, #f6f8fa); }
.ssf-chip:focus-visible { outline: 2px solid var(--fgColor-accent, #0969da);
  outline-offset: 1px; }
.ssf-icon { flex: none; }
/* One fixed colour per user-facing state, on every screen. Only the word and
   the icon carry it: the attribute never sits on a container, so a state
   colour cannot leak into the message or the stack beside it. */
[data-ssf-tone="working"] { color: #1a7f37; }
[data-ssf-tone="waiting"] { color: #9a6700; }
[data-ssf-tone="done"] { color: #57606a; }
[data-ssf-tone="problem"] { color: #cf222e; }
[data-ssf-tone="no-agent"] { color: #8c959f; }
.ssf-sep { opacity: 0.5; }
`;

  const sheet = new CSSStyleSheet();
  sheet.replaceSync(STYLE);

  /// The last merged snapshot from the service worker, or null before the
  /// worker has answered. Nothing is rendered from a null snapshot.
  let snapshot = null;
  let port = null;
  let rendering = false;
  let renderTimer = null;
  let pingTimer = null;
  /// name -> {host, shadow}, the nodes this script currently has in the page.
  const injected = new Map();
  /// name -> {more, details}, so a stream that repaints every couple of seconds
  /// cannot collapse what the reader has just opened.
  const opened = new Map();
  /// The open popover, or null: `{name, key, anchor, entry}`.
  let popover = null;

  /// The five user-facing states agreed on #408: every raw word ssf and the
  /// harness use maps onto one of them, each with its own colour and icon here
  /// and in every chip, so the same state reads the same everywhere. The raw
  /// word is never replaced -- it is what the tooltip shows, and it is the word
  /// the TUI prints.
  const PRESENTATION = {
    working: { label: "Working", kind: "working" },
    idle: { label: "Waiting on you", kind: "waiting" },
    blocked: { label: "Waiting on you", kind: "waiting" },
    done: { label: "Done", kind: "done" },
    "no-agent": { label: "Problem", kind: "problem" },
    "no-workspace": { label: "Problem", kind: "problem" },
    unknown: { label: "Problem", kind: "problem" },
    unbound: { label: "No agent", kind: "no-agent" },
  };
  /// A word this version does not know is a problem, not a healthy agent; the
  /// tooltip still carries the word itself.
  const PROBLEM = { label: "Problem", kind: "problem" };

  /// A pull request's body names the issue it delivers, and that issue's card is
  /// what a PR page shows. Closing keywords win over `Refs`, and the first match
  /// of each kind wins, per the layout decided on #408.
  const CLOSING_REF = /\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s+#(\d+)/gi;
  const REFS_REF = /\b(?:refs?|references?)\s*:?\s+#(\d+)/gi;

  /// `/issues/new` and `/owner/repo/issues/new`, with their `choose` subpage,
  /// live under a chip path but are forms rather than lists of items. Matched by
  /// the path's tail, so an owner actually named `issues` or a repository named
  /// `new` is not caught by it.
  function newItemForm(path) {
    const parts = path.split("/").filter(Boolean);
    const section = (index) =>
      parts.at(index) === "issues" || parts.at(index) === "pulls";
    return (
      (section(-2) && parts.at(-1) === "new") ||
      (section(-3) && parts.at(-2) === "new" && parts.at(-1) === "choose")
    );
  }

  /// The pages that carry chips: the issue and pull request lists, search
  /// results, and project boards. Anywhere else gets nothing, however many issue
  /// links the page happens to contain.
  function chipPage(path) {
    return !newItemForm(path) && CHIP_PATHS.some((pattern) => pattern.test(path));
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  /// A host element in the page with an isolated shadow tree, so GitHub's
  /// stylesheet and ours cannot reach each other. Custom properties still
  /// inherit, so GitHub's own theme drives everything that is not a state
  /// colour.
  function shadowHost(attribute, value) {
    const host = element("span");
    host.setAttribute(attribute, value);
    const shadow = host.attachShadow({ mode: "open" });
    shadow.adoptedStyleSheets = [sheet];
    return { host, shadow };
  }

  function ago(time) {
    if (time === null || time === undefined) return null;
    const at = typeof time === "number" ? time : Date.parse(time);
    if (Number.isNaN(at)) return null;
    const seconds = Math.round((Date.now() - at) / 1000);
    if (seconds < 45) return "just now";
    if (seconds < 3600) return `${Math.max(1, Math.round(seconds / 60))}m ago`;
    if (seconds < 86400) return `${Math.round(seconds / 3600)}h ago`;
    return `${Math.round(seconds / 86400)}d ago`;
  }

  function twoDigits(value) {
    return String(value).padStart(2, "0");
  }

  /// `HH:MM` on the reader's clock, for the stale modifier.
  function clock(time) {
    const at = new Date(time);
    if (Number.isNaN(at.getTime())) return "an unknown time";
    return `${twoDigits(at.getHours())}:${twoDigits(at.getMinutes())}`;
  }

  /// An absolute time for a tooltip.
  function absolute(time) {
    if (time === null || time === undefined) return null;
    const at = new Date(time);
    if (Number.isNaN(at.getTime())) return null;
    return (
      `${at.getFullYear()}-${twoDigits(at.getMonth() + 1)}-${twoDigits(at.getDate())}` +
      ` ${twoDigits(at.getHours())}:${twoDigits(at.getMinutes())}`
    );
  }

  /// A link to `owner/repo#number`, on the issue tracker rather than the pull
  /// request tracker, since that is where ssf and GitHub both put items.
  function itemLink(id, text) {
    const match = /^([^/#]+)\/([^/#]+)#(\d+)$/.exec(String(id ?? ""));
    if (!match) return element("span", undefined, text ?? String(id ?? ""));
    const link = element("a", undefined, text ?? `#${match[3]}`);
    link.href = `https://github.com/${match[1]}/${match[2]}/issues/${match[3]}`;
    link.target = "_blank";
    link.rel = "noreferrer noopener";
    return link;
  }

  function shape(tag, attributes, paint, stroked) {
    const node = document.createElementNS(SVG_NS, tag);
    for (const [name, value] of Object.entries(attributes)) {
      node.setAttribute(name, String(value));
    }
    if (stroked) {
      node.setAttribute("fill", "none");
      node.setAttribute("stroke", paint);
    } else {
      node.setAttribute("fill", paint);
    }
    return node;
  }

  /// The glyph inside a solid disc, in white; the same shape in the state
  /// colour when the disc is only outlined.
  function glyph(kind, solid) {
    const paint = solid ? "#ffffff" : "currentColor";
    switch (kind) {
      case "waiting":
        return [
          shape("rect", { x: 4.1, y: 4, width: 1.4, height: 4, rx: 0.5 }, paint, false),
          shape("rect", { x: 6.5, y: 4, width: 1.4, height: 4, rx: 0.5 }, paint, false),
        ];
      case "done":
        return [
          shape(
            "path",
            {
              d: "M3.7 6.2 5.3 7.8 8.5 4.2",
              "stroke-width": 1.6,
              "stroke-linecap": "round",
              "stroke-linejoin": "round",
            },
            paint,
            true,
          ),
        ];
      case "problem":
        return [
          shape(
            "path",
            { d: "M6 3.3V6.9", "stroke-width": 1.6, "stroke-linecap": "round" },
            paint,
            true,
          ),
          shape("circle", { cx: 6, cy: 8.7, r: 0.85 }, paint, false),
        ];
      default:
        return [];
    }
  }

  /// One icon per state: a solid disc carrying a white glyph for the three
  /// states that have a glyph (waiting, done, problem; Working and No agent are
  /// the bare disc), and -- when the snapshot behind it is stale -- the same
  /// shape outlined and dashed instead of filled, so a stale snapshot can never
  /// render as a working agent.
  function icon(kind, stale) {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 12 12");
    svg.setAttribute("width", "12");
    svg.setAttribute("height", "12");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("ssf-icon");
    svg.dataset.ssfTone = kind;
    const solid = !stale;
    const ring = shape("circle", { cx: 6, cy: 6, r: 4.6 }, solid ? "currentColor" : "none", !solid);
    if (!solid) {
      ring.setAttribute("stroke", "currentColor");
      ring.setAttribute("stroke-width", "1.3");
      ring.setAttribute("stroke-dasharray", "2 1.6");
    }
    svg.append(ring);
    for (const part of glyph(kind, solid)) svg.append(part);
    return svg;
  }

  /// What one item's state is, as this factory reports it. A factory that has
  /// gone stale keeps the states from its last snapshot but says when that was;
  /// one that never answered is a problem, not an agent.
  ///
  /// `stale` is the overlay's own modifier -- an inactive service, an
  /// unreachable VM, an unavailable driver or an overdue poll also make the
  /// snapshot untrustworthy, which is what `warning` carries.
  function stateFact(factory, match) {
    const raw =
      match.kind === "monitored"
        ? "unbound"
        : String(match.item.agent_state ?? "").trim() || "unknown";
    const shown = PRESENTATION[raw] ?? PROBLEM;
    // A snapshot is trustworthy only when the stream is live and the factory has
    // not flagged it: its own warning means the TUI paints the same snapshot
    // UNAVAILABLE / STALE, so a solid state here would contradict it.
    const live = factory.state === "live" && !factory.warning;
    const detail = [`ssf state: ${raw}`];
    if (match.kind === "monitored") {
      detail.push("ssf monitors this item but has no agent on it");
    }
    if (factory.warning) {
      detail.push(`the factory says its status may be incomplete: ${factory.warning}`);
    }
    if (factory.state === "stale") {
      detail.push(
        factory.error ?? "the factory stopped sending events; this is its last snapshot",
      );
    } else if (factory.state === "error") {
      detail.push(factory.error ?? "the factory did not answer");
    } else if (factory.state !== "live") {
      detail.push("the factory has not answered yet");
    }
    let time;
    if (factory.state === "error") time = "unreachable";
    else if (live) time = ago(match.item.last_activity_at) ?? "no activity recorded";
    else time = `as of ${clock(factory.lastFrameAt)}`;
    return { label: shown.label, kind: shown.kind, raw, stale: !live, time, detail };
  }

  /// `match` -> the facts a tooltip shows for it: the raw state word, the stack,
  /// the absolute time and the first line of the last message.
  function tooltip(factory, match) {
    const state = stateFact(factory, match);
    const lines = [`ssf state: ${state.raw}`];
    const stack = [match.item.harness, match.item.model].filter(Boolean).join(" · ");
    const when =
      absolute(match.item.last_activity_at) ??
      (factory.lastFrameAt ? `as of ${absolute(factory.lastFrameAt)}` : "no activity recorded");
    lines.push([factory.label, stack, when].filter(Boolean).join(" · "));
    const message = messageFor(match);
    if (message) lines.push(firstLine(message));
    lines.push(...state.detail.slice(1));
    return lines.join("\n");
  }

  function messageFor(match) {
    const message =
      match.kind === "monitored" ? match.item.title : match.item.last_assistant_message;
    return message ? String(message).trim() : "";
  }

  function firstLine(text) {
    const line = String(text).split("\n").find((part) => part.trim()) ?? "";
    return line.trim().length > 160 ? `${line.trim().slice(0, 157)}...` : line.trim();
  }

  /// Every factory that knows this `owner/repo#number`: its own card, or -- when
  /// the item is an additional one of another agent -- that agent, or an item
  /// ssf monitors without an agent. A factory that never answered cannot say
  /// whether it knows the item and is reported separately.
  function matchesFor(key) {
    const out = [];
    for (const factory of snapshot?.factories ?? []) {
      const own = factory.cards.find((card) => card.origin?.id === key);
      if (own) {
        out.push({ factory, item: own, kind: "agent" });
        continue;
      }
      const other = factory.cards.find((card) =>
        (card.additional ?? []).some((issue) => issue?.id === key),
      );
      if (other) {
        out.push({ factory, item: other, kind: "additional" });
        continue;
      }
      const monitored = factory.monitoredItems.find((item) => item?.id === key);
      if (monitored) out.push({ factory, item: monitored, kind: "monitored" });
    }
    return out;
  }

  /// A factory that has never answered cannot say whether it knows the item, so
  /// it is named rather than staying silent, which would read as "no agent".
  /// One that is still connecting knows nothing yet and is not a problem.
  function unreadableFactories() {
    return (snapshot?.factories ?? []).filter((factory) => factory.state === "error");
  }

  /// The state line every screen shares: the icon, the state word, and either
  /// the relative last activity or, for a stale snapshot, when it was taken.
  function stateLine(factory, match) {
    const state = stateFact(factory, match);
    const line = element("div", "ssf-state");
    // Hover always names the raw state word the TUI prints, so "Waiting on you"
    // never hides the fact that ssf says `idle`, and the factory's own reason
    // travels with it.
    line.title = state.detail.join("\n");
    line.append(icon(state.kind, state.stale));
    const word = element("span", "ssf-word", state.label);
    word.dataset.ssfTone = state.kind;
    line.append(word, element("span", "ssf-sep", "·"));
    const when = element("span", "ssf-when", state.time);
    if (state.stale) when.dataset.stale = "true";
    line.append(when);
    return line;
  }

  /// The message, trimmed to two lines behind a `more` toggle that is shown only
  /// when the text is actually clipped.
  function messageBlock(name, factory, match) {
    const message = messageFor(match);
    if (!message) return null;
    const key = `${name}|${factory.url}`;
    const flags = opened.get(key) ?? {};
    const wrap = element("div", "ssf-said");
    const body = element("div", "ssf-message", message);
    body.dataset.clamped = String(!flags.more);
    const more = element("button", "ssf-more", flags.more ? "less" : "more");
    more.type = "button";
    more.hidden = !flags.more;
    more.setAttribute("aria-expanded", String(Boolean(flags.more)));
    more.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      const now = !(opened.get(key)?.more ?? false);
      opened.set(key, { ...(opened.get(key) ?? {}), more: now });
      body.dataset.clamped = String(!now);
      more.textContent = now ? "less" : "more";
      more.setAttribute("aria-expanded", String(now));
      if (popover) placePopover();
    });
    wrap.append(body, more);
    return wrap;
  }

  /// The always-visible detail: what the agent is doing, where it is running and
  /// which session it is. The fields are the status model's own -- the current
  /// tool call, the workspace branch, the factory's label and the agent session
  /// id. A field the server does not send reads "not reported" rather than being
  /// invented; the factory row falls back to this extension's configured label,
  /// or to the factory's URL host when the options page has no label for it.
  function detailsBlock(name, factory, match) {
    const key = `${name}|${factory.url}`;
    const flags = opened.get(key) ?? {};
    const details = element("details", "ssf-details");
    details.open = Boolean(flags.details);
    details.addEventListener("toggle", () => {
      opened.set(key, { ...(opened.get(key) ?? {}), details: details.open });
    });
    details.append(element("summary", undefined, "Details"));
    const list = element("dl");
    const facts = [
      ["Tool", match.kind === "monitored" ? null : match.item.tool],
      ["Factory", match.item.factory ?? factory.label],
      ["Branch", match.kind === "monitored" ? null : match.item.branch],
      ["Session", match.kind === "monitored" ? null : match.item.agent_session_id],
    ];
    for (const [term, value] of facts) {
      list.append(
        element("dt", undefined, term),
        element("dd", undefined, value ? String(value) : "not reported"),
      );
    }
    details.append(list);
    return details;
  }

  /// One factory's card for an item. An issue that is an additional item of
  /// another agent leads with `worked on by the agent on #N`, so the card never
  /// claims an agent that belongs to a different issue.
  function card(name, factory, match) {
    const node = element("div", "ssf-card");
    if (match.kind === "additional") {
      const via = element("div", "ssf-via");
      via.append("worked on by the agent on ", itemLink(match.item.origin?.id));
      node.append(via);
    }
    node.append(stateLine(factory, match));
    const stack = [match.item.harness, match.item.model].filter(Boolean).join(" · ");
    if (stack) node.append(element("div", "ssf-stack", stack));
    const message = messageBlock(name, factory, match);
    if (message) node.append(message);
    const also = (match.item.additional ?? []).filter((issue) => issue?.id);
    if (match.kind === "agent" && also.length) {
      const line = element("div", "ssf-also");
      line.append("also on: ");
      also.forEach((issue, index) => {
        if (index) line.append(" ");
        line.append(itemLink(issue.id));
      });
      node.append(line);
    }
    // A factory that never answered has nothing to report about this item.
    if (match.kind !== "unreadable") node.append(detailsBlock(name, factory, match));
    return node;
  }

  /// Every card that belongs in one place: one per factory that knows the item,
  /// plus one per factory that could not be read at all. `forLabel` is the issue
  /// a pull request page resolved through, so its card says which issue it is
  /// about; `popover` swaps the sidebar's margin for the popover's own frame.
  function cards(name, matches, unreadable, { forLabel = null, popover = false } = {}) {
    const section = element("div", popover ? "ssf-popover" : "ssf-section");
    section.append(element("h3", "ssf-title", "SSF agent"));
    // The factory is named only when there is more than one to tell apart.
    const label = (snapshot?.factories?.length ?? 0) > 1;
    let first = true;
    for (const match of matches) {
      const node = card(name, match.factory, match);
      if (first && forLabel) node.prepend(element("div", "ssf-via", `for ${forLabel}`));
      if (label) node.prepend(element("div", "ssf-via", match.factory.label));
      first = false;
      section.append(node);
    }
    for (const factory of unreadable) {
      const node = card(name, factory, {
        factory,
        item: { agent_state: "" },
        kind: "unreadable",
      });
      // Always named: which factory could not be read is the whole point of the
      // card, and it is the only card when that factory is the only one.
      node.prepend(element("div", "ssf-via", factory.label));
      if (first && forLabel) node.prepend(element("div", "ssf-via", `for ${forLabel}`));
      first = false;
      section.append(node);
    }
    return section;
  }

  /// The chip beside an issue or pull request in a list, board or search
  /// result: icon, state word, relative last activity. The tooltip carries the
  /// harness, the model, the absolute time and the first line of the last
  /// message; the popover on a click carries the whole card.
  function chip(name, matches) {
    const first = matches[0];
    const state = stateFact(first.factory, first);
    const node = element("div", "ssf-chip");
    node.setAttribute("role", "button");
    node.setAttribute("tabindex", "0");
    node.setAttribute("aria-label", `ssf: ${state.label}, ${state.time}`);
    node.append(icon(state.kind, state.stale));
    const word = element("span", "ssf-word", state.label);
    word.dataset.ssfTone = state.kind;
    node.append(word, element("span", "ssf-sep", "·"));
    const when = element("span", "ssf-when", state.time);
    if (state.stale) when.dataset.stale = "true";
    node.append(when);
    node.title = matches.map((match) => tooltip(match.factory, match)).join("\n\n");
    return node;
  }

  /// Write one entry's content and put it where it belongs. Returns false when
  /// its place is not on the page yet; the next mutation re-renders.
  ///
  /// The placement is a no-op when nothing moved, so the script's own writes do
  /// not feed the observer that drives it.
  function update(entry, want) {
    const body =
      want.anchor === null
        ? cards(want.name, want.matches, want.unreadable ?? [], {
            forLabel: want.target?.label ?? null,
          })
        : chip(want.name, want.matches);
    entry.shadow.replaceChildren(body);
    let placed = false;
    if (want.anchor === null) {
      const slot = sidebar();
      if (!slot) return false;
      entry.host.dataset.ssfSlot = slot.slot;
      // Above Assignees; at the top when the page has no Assignees section.
      if (entry.host.parentElement !== slot.parent) slot.parent.prepend(entry.host);
      if (slot.before && slot.before.previousElementSibling !== entry.host) {
        slot.parent.insertBefore(entry.host, slot.before);
      }
      placed = true;
    } else {
      if (!want.anchor.isConnected) return false;
      if (
        !entry.host.isConnected ||
        entry.host.previousElementSibling !== want.anchor
      ) {
        want.anchor.insertAdjacentElement("afterend", entry.host);
      }
      placed = true;
    }
    if (placed) measure(entry);
    return placed;
  }

  /// Show a `more` toggle only where the trimmed message really is clipped.
  function measure(entry) {
    for (const wrap of entry.shadow.querySelectorAll(".ssf-said")) {
      const message = wrap.querySelector(".ssf-message");
      const more = wrap.querySelector(".ssf-more");
      if (!more || !message) continue;
      more.hidden =
        message.dataset.clamped === "true" &&
        message.scrollHeight <= message.clientHeight + 1;
    }
  }

  /// Where the sidebar card goes: GitHub's issue sidebar, above Assignees, and
  /// the older discussion sidebar a pull request page still uses. Two different
  /// containers, so both are tried.
  function sidebar() {
    const modern = document.querySelector('[class*="IssueSidebar-module__sidebarContent"]');
    if (modern) {
      return { parent: modern, before: sectionTitled(modern, "Assignees"), slot: "sidebar" };
    }
    const legacy = document.querySelector("#partial-discussion-sidebar");
    if (legacy) {
      return {
        parent: legacy,
        before: sectionTitled(legacy, "Assignees"),
        slot: "sidebar-legacy",
      };
    }
    return null;
  }

  function sectionTitled(parent, title) {
    for (const child of parent.children) {
      const heading = child.querySelector("h2, h3");
      if (heading && heading.textContent.trim().startsWith(title)) return child;
    }
    return null;
  }

  /// The title links on a list, board or search page, one per issue.
  function listAnchors() {
    const anchors = new Map();
    for (const anchor of document.querySelectorAll("a[href]")) {
      const match = LINK_PATH.exec(anchor.pathname);
      if (!match || !anchor.textContent.trim()) continue;
      if (anchor.closest("nav, [role='navigation']")) continue;
      const key = `${match[1]}/${match[2]}#${match[3]}`;
      if (!anchors.has(key)) anchors.set(key, anchor);
    }
    return anchors;
  }

  /// The issue a pull request page is about: the first one its body closes, or
  /// failing that the first it refs. A factory with no card for that issue but
  /// one for the pull request itself still resolves, so a pull request page is
  /// never emptier than its own number.
  function pullTarget(page) {
    const own = `${page.owner}/${page.repo}#${page.number}`;
    const body = document.querySelector(
      '[data-testid="issue-body"], #discussion_bucket .js-comment-body, .js-comment-body',
    );
    const linked = body ? linkedNumber(body.textContent) : null;
    if (linked) {
      const key = `${page.owner}/${page.repo}#${linked}`;
      if (matchesFor(key).length) return { key, label: `#${linked}` };
    }
    if (matchesFor(own).length) return { key: own, label: null };
    return null;
  }

  /// `Closes`/`Fixes`/`Resolves #N` first, then `Refs #N`, first match wins.
  function linkedNumber(body) {
    for (const pattern of [CLOSING_REF, REFS_REF]) {
      pattern.lastIndex = 0;
      const match = pattern.exec(body);
      if (match) return match[1];
    }
    return null;
  }

  function openPopover(name, key, anchor) {
    closePopover();
    const entry = shadowHost("data-ssf-popover", name);
    popover = { name, key, anchor, entry };
    document.body.append(entry.host);
    renderPopover();
  }

  function renderPopover() {
    if (!popover) return;
    const matches = matchesFor(popover.key);
    const unreadable = unreadableFactories();
    if (!matches.length && !unreadable.length) {
      closePopover();
      return;
    }
    const body = cards(popover.name, matches, unreadable, { popover: true });
    // A message too tall for the popover scrolls inside it, and the stream
    // repaints every couple of seconds: without this, the reader's place at the
    // end of a long message -- where the `less` toggle is -- would jump back to
    // the top under them.
    const previous = popover.entry.shadow.querySelector(".ssf-popover");
    const scrolled = previous ? previous.scrollTop : 0;
    popover.entry.shadow.replaceChildren(body);
    body.scrollTop = scrolled;
    measure(popover.entry);
    placePopover();
  }

  /// Keep the popover next to its chip, inside the viewport on both axes. The
  /// CSS bounds its height to the viewport, so the clamp always has room.
  function placePopover() {
    if (!popover) return;
    const { host } = popover.entry;
    const anchor = popover.anchor.getBoundingClientRect();
    const box = host.getBoundingClientRect();
    const left = Math.max(8, Math.min(anchor.left, innerWidth - box.width - 8));
    const below = anchor.bottom + 6;
    const preferred = below + box.height + 8 > innerHeight
      ? anchor.top - box.height - 6
      : below;
    const top = Math.max(8, Math.min(preferred, innerHeight - box.height - 8));
    host.style.left = `${left}px`;
    host.style.top = `${top}px`;
  }

  function closePopover() {
    if (!popover) return;
    popover.entry.host.remove();
    popover = null;
  }

  function popoverOpen(name) {
    return popover?.name === name;
  }

  /// Render what the current page and the latest snapshot call for, removing
  /// anything that no longer belongs. Idempotent: the same page and the same
  /// snapshot always leave the same nodes behind.
  function render() {
    if (rendering || !snapshot) return;
    rendering = true;
    try {
      const wanted = new Map();
      const page = DETAIL_PATH.exec(location.pathname);
      if (page) {
        const key = `${page[1]}/${page[2]}#${page[4]}`;
        const target =
          page[3] === "pull"
            ? pullTarget({ owner: page[1], repo: page[2], number: page[4] })
            : { key, label: null };
        if (target) {
          const matches = matchesFor(target.key);
          const unreadable = unreadableFactories();
          if (matches.length || unreadable.length) {
            wanted.set(`card:${key}`, { matches, unreadable, target, anchor: null });
          }
        }
      } else if (chipPage(location.pathname)) {
        for (const [key, anchor] of listAnchors()) {
          const matches = matchesFor(key);
          if (matches.length) wanted.set(`chip:${key}`, { matches, anchor });
        }
      }
      for (const [name, entry] of injected) {
        const want = wanted.get(name);
        if (!want) {
          entry.host.remove();
          injected.delete(name);
          if (popoverOpen(name)) closePopover();
          continue;
        }
        if (!update(entry, { ...want, name })) {
          entry.host.remove();
          injected.delete(name);
          if (popoverOpen(name)) closePopover();
        }
      }
      for (const [name, want] of wanted) {
        if (injected.has(name)) continue;
        const entry = shadowHost(
          want.anchor === null ? "data-ssf-card" : "data-ssf-list",
          name,
        );
        if (want.anchor !== null) {
          entry.host.dataset.ssfKey = name.slice("chip:".length);
          entry.host.addEventListener("click", (event) => chipActivate(event, entry));
          entry.host.addEventListener("keydown", (event) => {
            if (event.key !== "Enter" && event.key !== " ") return;
            chipActivate(event, entry);
          });
        }
        if (update(entry, { ...want, name })) injected.set(name, entry);
      }
      if (popover) renderPopover();
    } finally {
      rendering = false;
    }
  }

  /// A click or keypress on a chip opens its popover, and never reaches
  /// GitHub's own row click.
  function chipActivate(event, entry) {
    const target = event.composedPath()[0];
    if (!(target instanceof Element) || !target.closest(".ssf-chip")) return;
    event.preventDefault();
    event.stopPropagation();
    const name = entry.host.getAttribute("data-ssf-list");
    if (popoverOpen(name)) closePopover();
    else openPopover(name, entry.host.dataset.ssfKey, entry.host);
  }

  function scheduleRender() {
    if (renderTimer !== null) return;
    renderTimer = setTimeout(() => {
      renderTimer = null;
      render();
    }, RENDER_DEBOUNCE_MS);
  }

  function apply(payload) {
    snapshot = payload;
    scheduleRender();
  }

  function connect() {
    port = chrome.runtime.connect({ name: "ssf-overlay" });
    port.onMessage.addListener((message) => {
      if (message?.type === "snapshot") apply(message.payload);
    });
    port.onDisconnect.addListener(() => {
      port = null;
      // The worker was stopped to save memory; it restarts on the next request.
      setTimeout(connect, 1000);
    });
    clearInterval(pingTimer);
    pingTimer = setInterval(() => {
      // Keeps the worker awake, and refreshes the staleness it reports: a
      // sleeping worker runs no timer, so this is what notices a dead stream.
      try {
        port?.postMessage({ type: "ping" });
      } catch {
        // The port is gone; onDisconnect reconnects.
      }
    }, PING_MS);
  }

  // A popover belongs to the page it was opened on, and follows its chip while
  // the page scrolls; the first click outside it dismisses it.
  addEventListener("click", (event) => {
    if (!popover) return;
    const path = event.composedPath();
    if (path.includes(popover.entry.host) || path.includes(popover.anchor)) return;
    closePopover();
  });
  addEventListener("keydown", (event) => {
    if (event.key === "Escape") closePopover();
  });
  let repositioning = false;
  for (const event of ["scroll", "resize"]) {
    addEventListener(
      event,
      () => {
        if (!popover || repositioning) return;
        repositioning = true;
        requestAnimationFrame(() => {
          repositioning = false;
          placePopover();
        });
      },
      { passive: true, capture: true },
    );
  }

  // GitHub navigates client-side and re-renders its own React trees, so react
  // to its navigation events as well as to the DOM, and re-render rather than
  // render once.
  let lastPath = location.pathname;
  function navigated() {
    if (location.pathname !== lastPath) {
      lastPath = location.pathname;
      closePopover();
      for (const entry of injected.values()) entry.host.remove();
      injected.clear();
      // What the reader had opened belongs to the page they left.
      opened.clear();
    }
    scheduleRender();
  }
  for (const event of ["popstate", "turbo:load", "pjax:end", "soft-nav:end"]) {
    addEventListener(event, navigated);
  }
  const observer = new MutationObserver(navigated);
  observer.observe(document.documentElement, { childList: true, subtree: true });

  chrome.runtime.sendMessage({ type: "ssf:snapshot" }).then(apply, () => {});

  connect();
})();

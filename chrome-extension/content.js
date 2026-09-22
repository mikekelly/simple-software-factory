// Overlays live ssf state on github.com.
//
// Read-only: it renders the factory's own dashboard model (`cards` and
// `monitored_items` from the server's canonical status, see src/status.rs) as a
// badge beside the title of an issue or pull request, and as a small indicator
// on issue and pull request lists, search results and project boards. It never
// acts on the factory and never renders factory text as HTML: every node is
// built with textContent.
(() => {
  if (window.__ssfOverlayInstalled) return;
  window.__ssfOverlayInstalled = true;

  /// An issue or pull request page, including its subpages (`/pull/5/files`).
  const DETAIL_PATH = /^\/([^/]+)\/([^/]+)\/(?:issues|pull)\/(\d+)(?:\/|$)/;
  /// A link to exactly one issue or pull request, as lists and boards make them.
  const LIST_PATH = /^\/([^/]+)\/([^/]+)\/(?:issues|pull)\/(\d+)\/?$/;
  const PING_MS = 20000;
  const RENDER_DEBOUNCE_MS = 200;

  const STYLE = `
:host { display: inline-flex; vertical-align: middle; }
/* The issue page's own title carries order: 1 inside a flex TitleArea, so a
   plain append would land to its left; sort last explicitly. */
:host([data-ssf-badge]) { order: 99; }
.ssf-badge, .ssf-badge-row { display: flex; flex-wrap: wrap; align-items: baseline;
  gap: 2px 8px; min-width: 0; max-width: 100%; }
.ssf-badge { flex-direction: column; margin-left: 12px; font-family: inherit;
  font-size: 12px; line-height: 1.6; color: var(--fgColor-muted, #59636e); }
.ssf-mark { padding: 0 6px; border: 1px solid var(--borderColor-default, #d1d9e0);
  border-radius: 999px; font-weight: 600; color: var(--fgColor-default, #1f2328); }
.ssf-label { font-weight: 600; color: var(--fgColor-default, #1f2328); }
.ssf-sep { opacity: 0.5; }
.ssf-summary { max-width: 100%; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; }
.ssf-chip { display: inline-flex; align-items: center; gap: 4px; margin-left: 6px;
  padding: 0 6px; border: 1px solid var(--borderColor-default, #d1d9e0);
  border-radius: 999px; font-family: inherit; font-size: 11px; line-height: 18px;
  font-weight: 500; color: var(--fgColor-muted, #59636e); vertical-align: middle;
  white-space: nowrap; }
.ssf-dot { width: 6px; height: 6px; border-radius: 50%; background: currentColor; }
.ssf-fact, .ssf-chip { font-weight: 500; }
[data-kind="working"] { color: var(--fgColor-success, #1a7f37); font-weight: 600; }
[data-kind="blocked"], [data-kind="error"], [data-kind="unknown"] {
  color: var(--fgColor-danger, #cf222e); font-weight: 600; }
[data-kind="waiting"], [data-kind="idle"], [data-kind="done"],
[data-kind="stale"] { color: var(--fgColor-attention, #9a6700); font-weight: 600; }
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

  /// How the TUI paints each agent state (`dashboard.rs`), so both clients call
  /// the same state healthy.
  const KIND = {
    working: "working",
    blocked: "blocked",
    waiting: "waiting",
    idle: "idle",
    done: "done",
  };

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  /// A host element in the page with an isolated shadow tree, so GitHub's
  /// stylesheet and ours cannot reach each other. Custom properties still
  /// inherit, so GitHub's own colours drive the badge in either theme.
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

  /// `state` as this factory reports it, with a stale or unreachable factory
  /// said out loud rather than shown as its last known state. A factory can
  /// also flag its own snapshot as unreliable -- an inactive service, an
  /// unreachable VM, an unavailable driver, an overdue poll -- which the TUI
  /// paints as `UNAVAILABLE / STALE` and the server's web UI as "Status may be
  /// incomplete"; that reads `incomplete` here.
  function reportedState(factory, state) {
    const name = state || "unknown";
    if (factory.state === "live") {
      if (factory.warning) {
        return {
          text: `${name} \u00b7 incomplete`,
          kind: "stale",
          detail: `the factory says its status may be incomplete: ${factory.warning}`,
        };
      }
      return { text: name, kind: KIND[name] ?? "muted" };
    }
    if (factory.state === "stale") {
      const seen = ago(factory.lastFrameAt);
      return {
        text: `${name} \u00b7 stale`,
        kind: "stale",
        detail: [factory.error, seen ? `last update ${seen}` : null]
          .filter(Boolean)
          .join("; "),
      };
    }
    return {
      text: "unknown",
      kind: "unknown",
      detail: factory.error ?? "the factory did not answer",
    };
  }

  /// What one factory says about this item, as a fact: its agent state, or that
  /// ssf monitors the item without an agent on it.
  function factFor(factory, match) {
    const fact = reportedState(
      factory,
      match.kind === "monitored" ? "no agent" : match.item.agent_state,
    );
    if (match.kind === "monitored" && !fact.detail) {
      fact.detail = "ssf monitors this item but has no agent on it";
    }
    return fact;
  }

  function activityFact(item) {
    const activity = ago(item.last_activity_at);
    return activity
      ? { text: activity, kind: "muted" }
      : {
          text: "no activity recorded",
          kind: "muted",
          detail: "the factory has no activity time for this session",
        };
  }

  /// Every factory that knows this `owner/repo#number`, as a card with an agent
  /// or as an item ssf monitors without one.
  function matchesFor(key) {
    const out = [];
    for (const factory of snapshot?.factories ?? []) {
      const card = factory.cards.find(
        (card) =>
          card.origin?.id === key ||
          (card.additional ?? []).some((issue) => issue?.id === key),
      );
      if (card) {
        out.push({ factory, item: card, kind: "agent" });
        continue;
      }
      const monitored = factory.monitoredItems.find((item) => item?.id === key);
      if (monitored) out.push({ factory, item: monitored, kind: "monitored" });
    }
    return out;
  }

  function factNode(fact) {
    const node = element("span", "ssf-fact", fact.text);
    node.dataset.kind = fact.kind ?? "muted";
    if (fact.detail) node.title = fact.detail;
    return node;
  }

  /// The badge beside an issue or pull request title: one row per factory that
  /// knows the item, then one row per factory that could not be read at all.
  /// A factory that never answered cannot say whether it knows the item, so
  /// staying silent about it would read as "no agent" — the TUI is explicit
  /// about the same case (docs/dashboard.md).
  function badge(matches, unreadable) {
    const body = element("div", "ssf-badge");
    const many = (snapshot?.factories?.length ?? 0) > 1;
    const described = [];
    for (const match of matches) {
      const { factory, item } = match;
      const row = element("div", "ssf-badge-row");
      row.append(element("span", "ssf-mark", "ssf"));
      if (many) row.append(element("span", "ssf-label", factory.label));
      const facts = [];
      if (match.kind === "agent") {
        if (item.harness) facts.push({ text: item.harness });
        if (item.model) facts.push({ text: item.model });
      }
      facts.push(factFor(factory, match));
      facts.push(activityFact(item));
      facts.forEach((fact, index) => {
        if (index) row.append(element("span", "ssf-sep", "\u00b7"));
        row.append(factNode(fact));
      });
      const summary =
        match.kind === "monitored"
          ? item.title
          : item.last_assistant_message || "no summary yet";
      body.append(row, element("div", "ssf-summary", summary ?? ""));
      described.push(
        `${factory.label}: ${facts.map((fact) => fact.text).join(" \u00b7 ")}${
          summary ? ` \u2014 ${summary}` : ""
        }${factory.warning ? ` (status may be incomplete: ${factory.warning})` : ""}`,
      );
    }
    for (const factory of unreadable) {
      const fact = reportedState(factory, null);
      const row = element("div", "ssf-badge-row");
      row.append(element("span", "ssf-mark", "ssf"));
      row.append(element("span", "ssf-label", factory.label));
      row.append(element("span", "ssf-sep", "\u00b7"));
      row.append(factNode(fact));
      const reason = factory.error ?? "the factory did not answer";
      body.append(row, element("div", "ssf-summary", reason));
      described.push(`${factory.label}: ${fact.text} \u2014 ${reason}`);
    }
    return { body, description: described.join("\n") };
  }

  /// The small indicator beside an issue or pull request in a list, board or
  /// search result. The first factory that knows it sets the state; every
  /// factory that knows it is named in the tooltip.
  function chip(matches) {
    const body = element("div", "ssf-chip");
    const first = matches[0];
    const fact = factFor(first.factory, first);
    body.dataset.kind = fact.kind;
    body.append(element("span", "ssf-dot"), element("span", undefined, fact.text));
    return {
      body,
      description: [
        fact.detail,
        ...matches.map(
          ({ factory }) =>
            `ssf factory ${factory.label} (${factory.state}${
              factory.warning ? ", status may be incomplete" : ""
            })`,
        ),
      ]
        .filter(Boolean)
        .join("\n"),
    };
  }

  /// Where the badge goes: the page header that carries the title, on both the
  /// current issue and pull request layouts and the older one.
  function titleArea() {
    return (
      document.querySelector(
        '[data-testid="issue-header"] [data-component="TitleArea"]',
      ) ??
      document.querySelector(
        'header [data-component="PageHeader"] [data-component="TitleArea"]',
      ) ??
      document.querySelector(".gh-header-title")?.parentElement ??
      null
    );
  }

  /// Write one entry's content and put it where it belongs. Returns false when
  /// its place is not on the page yet; the next mutation re-renders.
  ///
  /// Both halves are no-ops when nothing changed, so the script's own writes do
  /// not feed the observer that drives it.
  function update(entry, want) {
    const { body, description } = want.anchor === null
      ? badge(want.matches, want.unreadable)
      : chip(want.matches);
    entry.shadow.replaceChildren(body);
    if (want.anchor === null) {
      const area = titleArea();
      if (!area) return false;
      entry.host.title = description;
      if (entry.host.parentElement !== area) area.append(entry.host);
      return true;
    }
    if (!want.anchor.isConnected) return false;
    entry.host.title = description;
    if (
      !entry.host.isConnected ||
      entry.host.previousElementSibling !== want.anchor
    ) {
      want.anchor.insertAdjacentElement("afterend", entry.host);
    }
    return true;
  }

  /// The title links on a list, board or search page, one per issue.
  function listAnchors() {
    const anchors = new Map();
    for (const anchor of document.querySelectorAll("a[href]")) {
      const match = LIST_PATH.exec(anchor.pathname);
      if (!match || !anchor.textContent.trim()) continue;
      if (anchor.closest("nav, [role='navigation']")) continue;
      const key = `${match[1]}/${match[2]}#${match[3]}`;
      if (!anchors.has(key)) anchors.set(key, anchor);
    }
    return anchors;
  }

  /// Render what the current page and the latest snapshot call for, removing
  /// anything that no longer belongs. Idempotent: the same page and the same
  /// snapshot always leave the same nodes behind.
  function render() {
    if (rendering || !snapshot) return;
    rendering = true;
    try {
      const wanted = new Map();
      const detail = DETAIL_PATH.exec(location.pathname);
      if (detail) {
        const key = `${detail[1]}/${detail[2]}#${detail[3]}`;
        const matches = matchesFor(key);
        const unreadable = (snapshot.factories ?? []).filter(
          (factory) => factory.state === "error",
        );
        if (matches.length || unreadable.length) {
          wanted.set(`badge:${key}`, { matches, unreadable, anchor: null });
        }
      } else {
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
          continue;
        }
        if (!update(entry, want)) {
          entry.host.remove();
          injected.delete(name);
        }
      }
      for (const [name, want] of wanted) {
        if (injected.has(name)) continue;
        const entry = shadowHost(
          want.anchor === null ? "data-ssf-badge" : "data-ssf-list",
          name,
        );
        if (update(entry, want)) injected.set(name, entry);
      }
    } finally {
      rendering = false;
    }
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

  // GitHub navigates client-side and re-renders its own React trees, so react
  // to its navigation events as well as to the DOM, and re-render rather than
  // render once.
  let lastPath = location.pathname;
  function navigated() {
    if (location.pathname !== lastPath) {
      lastPath = location.pathname;
      for (const entry of injected.values()) entry.host.remove();
      injected.clear();
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

import { mountTerminal, setThemePreference, themePreference } from "./term-view.js";

// The masthead's System / Light / Dark switch.
const themeButtons = document.querySelectorAll(".theme-switch button");
function showTheme(pref) {
  for (const button of themeButtons) button.setAttribute("aria-pressed", String(button.value === pref));
}
for (const button of themeButtons) {
  button.addEventListener("click", () => {
    setThemePreference(button.value);
    showTheme(button.value);
  });
}
showTheme(themePreference());
addEventListener("storage", () => showTheme(themePreference()));

const cardsNode = document.querySelector("#cards");
const emptyNode = document.querySelector("#empty");
const noticeNode = document.querySelector("#notice");
const statusNode = document.querySelector("#refresh-status");
const monitoredNode = document.querySelector("#monitored");
const refreshButton = document.querySelector("#refresh");
const template = document.querySelector("#card-template");
const buildNode = document.querySelector("#build");
let loading = false;
/// Whether this server takes typing from this page (`dashboard.terminal_input`):
/// an item's terminal is offered only where it can be typed into.
let terminalInput = false;

// The cards and monitored rows on screen, by the issue each is about. The poll
// redraws the list from every answer it gets, and what is already there is
// written into rather than drawn again: a card whose facts have not moved is
// left alone, and the list keeps the focus, the place and the scroll a person
// reading it has. What is not in the latest snapshot is forgotten, and its node
// dropped with it.
const mountedCards = new Map();
const mountedItems = new Map();

function fill(node, text) {
  if (node.textContent !== text) node.textContent = text;
}

// Show or hide a node only where that is a change. `hidden` is an attribute and
// writing it identically is a DOM write like any other: a poll that reports what
// the page already shows must not touch the page at all.
function show(node, visible) {
  if (node.hidden !== !visible) node.hidden = !visible;
}

// The item's own number, and its title after it where the list carries one.
//
// The href is compared against the attribute rather than read back from
// `anchor.href`: that getter returns the URL as the DOM resolved it, which can
// differ from what the payload carried, and the difference would rewrite the
// attribute on every poll.
function issueLink(anchor, issue, title) {
  fill(anchor, title === undefined ? issue.id : `${issue.id} — ${title}`);
  if (issue.url && anchor.getAttribute("href") !== issue.url) anchor.href = issue.url;
}

// When this session was last active, or why nobody can say. The reason is the
// server's own (`activity_note`), so this page, the TUI and the overlay all say
// the same thing instead of "unknown", which reads as a claim about the agent
// rather than about the transcript ssf could not read (#439).
function activityLabel(card) {
  const value = card.last_activity_at;
  if (!value) return card.activity_note || "not reported";
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "an unreadable time was reported";
  const elapsed = Math.max(0, Date.now() - date.valueOf());
  const minutes = Math.floor(elapsed / 60000);
  let relative = "just now";
  if (minutes >= 1440) relative = `${Math.floor(minutes / 1440)}d ago`;
  else if (minutes >= 60) relative = `${Math.floor(minutes / 60)}h ago`;
  else if (minutes >= 1) relative = `${minutes}m ago`;
  return `${relative} · ${date.toLocaleString()}`;
}

function stackOf(card) {
  return [card.harness, card.model, card.effort].filter(Boolean).join(" · ");
}

// The card's stack line: what the pane is running -- harness, model and effort,
// since effort is what the session's tokens cost -- and what the next launch
// would start (`codex → omp · opus · low next launch`, when a config edit left
// a live session on the older harness).
function stackLabel(card) {
  const running = stackOf(card);
  const next = card.next_launch && card.next_launch.harness;
  if (!next) return running;
  return `${running} → ${stackOf(card.next_launch)} next launch`;
}

// Put `nodes` in `parent` in this order, dropping what no longer belongs before
// moving anything: a node that moves loses the focus inside it, so nothing
// moves that does not have to, and a list redrawn as it stands writes nothing.
function place(parent, nodes) {
  const focused = document.activeElement;
  const wanted = new Set(nodes);
  for (const node of [...parent.childNodes]) {
    if (!wanted.has(node)) node.remove();
  }
  let index = 0;
  for (const node of nodes) {
    const at = parent.childNodes[index];
    if (at !== node) parent.insertBefore(node, at ?? null);
    index += 1;
  }
  // Re-inserting a node drops the focus inside it, so the element that had it
  // is put back: a poll that reorders the list is not a reason to lose the link
  // you were reading.
  if (focused?.isConnected && document.activeElement !== focused) {
    focused.focus({preventScroll: true});
  }
}

// Forget what the latest snapshot does not carry, so a long-lived tab does not
// hold every card it has ever drawn.
function forget(mounted, keep) {
  for (const key of [...mounted.keys()]) if (!keep.has(key)) mounted.delete(key);
}

// One item of a list of links, drawn from nothing: the monitored list's rows.
function rowFor(issue) {
  const item = document.createElement("li");
  const anchor = document.createElement("a");
  anchor.target = "_blank";
  anchor.rel = "noopener noreferrer";
  item.append(anchor);
  issueLink(anchor, issue, issue.title);
  return item;
}

// GitHub-style octicons for an issue and a pull request (#637).
const ICONS = {
  issue:
    '<path d="M8 9.5a1.5 1.5 0 1 0 0-3 1.5 1.5 0 0 0 0 3Z"/><path d="M8 0a8 8 0 1 1 0 16A8 8 0 0 1 8 0ZM1.5 8a6.5 6.5 0 1 0 13 0 6.5 6.5 0 0 0-13 0Z"/>',
  pull_request:
    '<path d="M1.5 3.25a2.25 2.25 0 1 1 3 2.122v5.256a2.251 2.251 0 1 1-1.5 0V5.372A2.25 2.25 0 0 1 1.5 3.25Zm5.677-.177L9.573.677A.25.25 0 0 1 10 .854V2.5h1A2.5 2.5 0 0 1 13.5 5v5.628a2.251 2.251 0 1 1-1.5 0V5a1 1 0 0 0-1-1h-1v1.646a.25.25 0 0 1-.427.177L7.177 3.427a.25.25 0 0 1 0-.354ZM3.75 2.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm0 9.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm8.25.75a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Z"/>',
};

function kindOf(issue) {
  return issue.kind === "pull_request" ? "pull_request" : "issue";
}

// An item's reference as the card shows it: `#N` in the card's own
// repository, `owner/repo#N` in any other.
function shortRef(id, home) {
  const [repo, number] = String(id).split("#");
  return number !== undefined && repo === home ? `#${number}` : String(id);
}

// One row of a card's Owns or Following list: kind icon, reference, title and,
// for a followed item, the level it is followed at.
function relatedRow(row, issue, home) {
  if (!row) {
    row = document.createElement("li");
    row.innerHTML =
      '<svg class="icon kind" viewBox="0 0 16 16" width="14" height="14" fill="currentColor" role="img"></svg>' +
      '<a class="ref" target="_blank" rel="noopener noreferrer"></a><span class="title"></span><span class="level"></span>';
  }
  const kind = kindOf(issue);
  const icon = row.querySelector(".kind");
  if (icon.dataset.kind !== kind) {
    icon.dataset.kind = kind;
    icon.innerHTML = ICONS[kind];
    icon.setAttribute("aria-label", kind === "pull_request" ? "Pull request" : "Issue");
  }
  const state = String(issue.github_state || "").toLowerCase();
  if (icon.dataset.state !== state) icon.dataset.state = state;
  const anchor = row.querySelector(".ref");
  fill(anchor, shortRef(issue.id, home));
  if (issue.url && anchor.getAttribute("href") !== issue.url) anchor.href = issue.url;
  fill(row.querySelector(".title"), issue.title && issue.title !== issue.id ? issue.title : "");
  const level = row.querySelector(".level");
  fill(level, issue.events || "");
  show(level, Boolean(issue.events));
  return row;
}

// A card's Owns or Following section, hidden when it has nothing to list. A row
// already at its place is written to, and only a new one is drawn.
function relatedList(section, issues, home) {
  const list = section.querySelector("ul");
  const rows = issues.map((issue, index) => relatedRow(list.children[index], issue, home));
  show(section, rows.length !== 0);
  place(list, rows);
}

// One card, in the node already drawn for this issue if there is one, so the
// link a person is on keeps its focus across a refresh.
function cardNode(card) {
  const id = card.origin.id;
  let article = mountedCards.get(id);
  if (!article) {
    article = template.content.firstElementChild.cloneNode(true);
    mountedCards.set(id, article);
  }
  const state = card.agent_state.toLowerCase().replace(/[^a-z-]/g, "");
  if (article.dataset.state !== state) article.dataset.state = state;
  fill(article.querySelector(".state-text"), card.agent_state);
  fill(article.querySelector(".harness"), stackLabel(card));
  // The item's reference is the heading, its title the line under it (#637).
  issueLink(article.querySelector(".issue-link"), card.origin);
  fill(article.querySelector(".issue-title"), card.origin.title === card.origin.id ? "" : card.origin.title);
  // The session's pane as a live, shared terminal (#563), offered only where
  // this server and the factory let the page type into it.
  const terminal = `terminal.html?session=${encodeURIComponent(card.owner || card.origin.id)}`;
  const terminalLink = article.querySelector(".terminal-link");
  if (terminalLink.getAttribute("href") !== terminal) terminalLink.href = terminal;
  show(terminalLink, terminalInput && card.pane_input === true);
  // The card, not just its time: with no time to show, the model's own reason
  // for that is what belongs in the row (#439).
  fill(article.querySelector(".activity"), activityLabel(card));
  const home = String(card.origin.id).split("#")[0];
  relatedList(article.querySelector(".owns"), card.additional || [], home);
  relatedList(article.querySelector(".following"), card.following || [], home);
  return article;
}

// One row of the monitored list, in the node already drawn for this issue.
function monitoredRow(issue) {
  const kept = mountedItems.get(issue.id);
  if (kept) {
    issueLink(kept.querySelector("a"), issue, issue.title);
    return kept;
  }
  const item = rowFor(issue);
  mountedItems.set(issue.id, item);
  return item;
}

function render(cards, monitoredItems) {
  place(cardsNode, cards.map(cardNode));
  forget(mountedCards, new Set(cards.map((card) => card.origin.id)));
  // The empty panel is what says so when there is nothing to show.
  show(emptyNode, cards.length === 0);

  const monitoredList = monitoredNode.querySelector("ul");
  place(monitoredList, monitoredItems.map(monitoredRow));
  forget(mountedItems, new Set(monitoredItems.map((issue) => issue.id)));
  show(monitoredNode, monitoredItems.length !== 0);
}

// The card list says it is being read only where that is a change, for the same
// reason a card's own facts are written the same way.
function busy(reading) {
  if (cardsNode.getAttribute("aria-busy") !== String(reading)) {
    cardsNode.setAttribute("aria-busy", String(reading));
  }
}

async function refresh() {
  if (loading) return;
  loading = true;
  refreshButton.disabled = true;
  busy(true);
  try {
    const response = await fetch("api/status", {cache: "no-store"});
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || `status request failed (${response.status})`);
    terminalInput = body.terminal_input === true;
    fill(buildNode, body.build || "");
    render(body.cards, body.monitored_items || []);
    if (body.warning) show(emptyNode, false);
    // A VM guest is upgraded on its own (`ssf vm upgrade`), so its release
    // can differ from the host's that serves this page; the note says so.
    fill(noticeNode, body.warning ? `Status may be incomplete: ${body.warning}` : body.version_note || "");
    show(noticeNode, Boolean(body.warning || body.version_note));
    fill(statusNode, `Updated ${new Date(body.refreshed_at * 1000).toLocaleTimeString()}`);
    return true;
  } catch (error) {
    show(emptyNode, false);
    fill(noticeNode, `Could not refresh: ${error.message}`);
    show(noticeNode, true);
    fill(statusNode, "Refresh failed");
  } finally {
    loading = false;
    refreshButton.disabled = false;
    busy(false);
  }
}

refreshButton.addEventListener("click", refresh);

// The page is a dockview layout (#574): the cards are the "Agents" panel, and
// each session's terminal opened from a card is a panel beside it that can be
// tabbed, split and dragged. The layout is this browser's, kept in
// localStorage; a terminal whose session has gone is dropped on restore.
const LAYOUT_KEY = "ssf.dashboard.layout";
const agentsNode = document.querySelector("#agents");
const { createDockview, DefaultTab } = window["dockview-core"];

/// dockview's tab, less its close control on the Agents panel (#606): the
/// cards keep their place. This build's tab closes only by that control.
class Tab extends DefaultTab {
  init(params) {
    super.init(params);
    if (params.api.id === "agents") this.action.remove();
  }
}

const dock = createDockview(document.querySelector("#dock"), {
  theme: { name: "ssf", className: "dockview-theme-ssf" },
  // Every tab is this one, restored layouts' included (they name no tab).
  defaultTabComponent: "ssf",
  createTabComponent: () => new Tab(),
  // A floating group of the cards could be closed or lost off screen.
  disableFloatingGroups: true,
  createComponent({ name }) {
    const element = document.createElement("div");
    element.className = "dock-panel";
    if (name === "agents") {
      element.append(agentsNode);
      return { element, init() {} };
    }
    let view = null;
    return {
      element,
      init({ params }) {
        view = mountTerminal(element, params.session);
      },
      focus: () => view?.focus(),
      dispose() {
        view?.dispose();
        view = null;
      },
    };
  },
});

const AGENTS = { id: "agents", component: "agents", title: "Agents" };

function defaultLayout() {
  dock.clear();
  dock.addPanel(AGENTS);
}

function saveLayout() {
  try {
    localStorage.setItem(LAYOUT_KEY, JSON.stringify(dock.toJSON()));
  } catch {}
}

/// Open `session`'s terminal, or focus it where it is already open.
function openTerminal(session) {
  const id = `term:${session}`;
  const open = dock.getPanel(id);
  if (open) {
    open.api.setActive();
    return;
  }
  const other = dock.panels.findLast((panel) => panel.id.startsWith("term:"));
  dock.addPanel({
    id,
    component: "terminal",
    title: session,
    params: { session },
    // Beside the cards the first time, then as a tab with the other terminals:
    // a split per terminal would give each too little room.
    position: other
      ? { direction: "within", referencePanel: other.id }
      : { direction: "right", referencePanel: "agents" },
  });
}

cardsNode.addEventListener("click", (event) => {
  const link = event.target.closest(".terminal-link");
  if (!link || event.button !== 0 || event.ctrlKey || event.metaKey || event.shiftKey) return;
  event.preventDefault();
  openTerminal(new URL(link.href).searchParams.get("session") ?? "");
});

/// Restore the saved layout, less the terminals of sessions `live` no longer
/// has (all kept when `live` is null: status is unknown); anything unexpected
/// falls back to the default.
function restoreLayout(live) {
  try {
    const saved = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "null");
    if (!saved?.panels?.agents) throw new Error("no saved layout");
    dock.fromJSON(saved);
    for (const panel of [...dock.panels]) {
      const session = panel.params?.session;
      if (panel.id !== "agents" && live && !live.has(session)) dock.removePanel(panel);
    }
    if (!dock.getPanel("agents")) throw new Error("no agents panel");
  } catch {
    defaultLayout();
  }
}

async function start() {
  const known = await refresh();
  const live = known && new Set(
    [...mountedCards.values()]
      .filter((card) => !card.querySelector(".terminal-link").hidden)
      .map((card) => new URL(card.querySelector(".terminal-link").href).searchParams.get("session")),
  );
  restoreLayout(live);
  dock.onDidLayoutChange(saveLayout);
  // A fallback: should the cards' panel go anyway, it comes back.
  dock.onDidRemovePanel((panel) => {
    if (panel.id === "agents") setTimeout(() => dock.getPanel("agents") || dock.addPanel(AGENTS));
  });
  setInterval(refresh, 5000);
}
start();

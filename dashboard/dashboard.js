const cardsNode = document.querySelector("#cards");
const emptyNode = document.querySelector("#empty");
const noticeNode = document.querySelector("#notice");
const statusNode = document.querySelector("#refresh-status");
const monitoredNode = document.querySelector("#monitored");
const refreshButton = document.querySelector("#refresh");
const template = document.querySelector("#card-template");
let loading = false;

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

// One item of a list of links, drawn from nothing: the monitored list's rows
// and a card's additional issues both use it.
function rowFor(issue) {
  const item = document.createElement("li");
  const anchor = document.createElement("a");
  anchor.target = "_blank";
  anchor.rel = "noopener noreferrer";
  item.append(anchor);
  issueLink(anchor, issue, issue.title);
  return item;
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
  fill(article.querySelector(".issue-title"), card.origin.title);
  issueLink(article.querySelector(".issue-link"), card.origin);
  // The session's pane as a live terminal (#563), read-only unless this
  // server and the factory let the page type.
  const terminal = `terminal.html?session=${encodeURIComponent(card.owner || card.origin.id)}`;
  const terminalLink = article.querySelector(".terminal-link");
  if (terminalLink.getAttribute("href") !== terminal) terminalLink.href = terminal;
  // The card, not just its time: with no time to show, the model's own reason
  // for that is what belongs in the row (#439).
  fill(article.querySelector(".activity"), activityLabel(card));
  fill(article.querySelector(".message"), card.last_assistant_message || "No message reported.");
  const section = article.querySelector(".additional");
  const list = section.querySelector("ul");
  // A card's additional issues are drawn in one order and rarely change; a row
  // already at this place is written to, and only a new one is drawn.
  const items = card.additional.map((issue, index) => {
    const kept = list.children[index];
    if (kept) {
      issueLink(kept.querySelector("a"), issue, issue.title);
      return kept;
    }
    return rowFor(issue);
  });
  show(section, items.length !== 0);
  place(list, items);
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
    render(body.cards, body.monitored_items || []);
    if (body.warning) show(emptyNode, false);
    fill(noticeNode, body.warning ? `Status may be incomplete: ${body.warning}` : "");
    show(noticeNode, Boolean(body.warning));
    fill(statusNode, `Updated ${new Date(body.refreshed_at * 1000).toLocaleTimeString()}`);
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
refresh();
setInterval(refresh, 5000);

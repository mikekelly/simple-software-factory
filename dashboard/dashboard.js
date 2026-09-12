const cardsNode = document.querySelector("#cards");
const emptyNode = document.querySelector("#empty");
const noticeNode = document.querySelector("#notice");
const statusNode = document.querySelector("#refresh-status");
const refreshButton = document.querySelector("#refresh");
const template = document.querySelector("#card-template");
let loading = false;

function issueLink(anchor, issue) {
  anchor.textContent = issue.id;
  anchor.dataset.issueId = issue.id;
  if (issue.url) anchor.href = issue.url;
}

function activityLabel(value) {
  if (!value) return "Unknown — the driver did not report a time";
  const date = new Date(value);
  if (Number.isNaN(date.valueOf())) return "Unknown — the driver reported an invalid time";
  const elapsed = Math.max(0, Date.now() - date.valueOf());
  const minutes = Math.floor(elapsed / 60000);
  let relative = "just now";
  if (minutes >= 1440) relative = `${Math.floor(minutes / 1440)}d ago`;
  else if (minutes >= 60) relative = `${Math.floor(minutes / 60)}h ago`;
  else if (minutes >= 1) relative = `${minutes}m ago`;
  return `${relative} · ${date.toLocaleString()}`;
}

function render(cards) {
  const focused = cardsNode.contains(document.activeElement) && document.activeElement.matches("a")
    ? document.activeElement.dataset.issueId
    : null;
  cardsNode.replaceChildren();
  for (const card of cards) {
    const fragment = template.content.cloneNode(true);
    const article = fragment.querySelector(".card");
    article.dataset.state = card.agent_state.toLowerCase().replace(/[^a-z-]/g, "");
    fragment.querySelector(".state-text").textContent = card.agent_state;
    fragment.querySelector(".harness").textContent = [card.harness, card.model].filter(Boolean).join(" · ");
    fragment.querySelector(".issue-title").textContent = card.origin.title;
    issueLink(fragment.querySelector(".issue-link"), card.origin);
    fragment.querySelector(".activity").textContent = activityLabel(card.last_activity_at);
    fragment.querySelector(".message").textContent = card.last_assistant_message || "No message reported.";
    if (card.additional.length) {
      const section = fragment.querySelector(".additional");
      const list = section.querySelector("ul");
      section.hidden = false;
      for (const issue of card.additional) {
        const item = document.createElement("li");
        const anchor = document.createElement("a");
        anchor.target = "_blank";
        anchor.rel = "noopener noreferrer";
        issueLink(anchor, issue);
        anchor.append(` — ${issue.title}`);
        item.append(anchor);
        list.append(item);
      }
    }
    cardsNode.append(fragment);
  }
  if (focused) {
    const replacement = [...cardsNode.querySelectorAll("a")]
      .find((anchor) => anchor.dataset.issueId === focused);
    if (replacement) replacement.focus({preventScroll: true});
  }
  emptyNode.hidden = cards.length !== 0;
}

async function refresh() {
  if (loading) return;
  loading = true;
  refreshButton.disabled = true;
  cardsNode.setAttribute("aria-busy", "true");
  try {
    const response = await fetch("api/status", {cache: "no-store"});
    const body = await response.json();
    if (!response.ok) throw new Error(body.error || `status request failed (${response.status})`);
    render(body.cards);
    if (body.warning) emptyNode.hidden = true;
    noticeNode.textContent = body.warning ? `Status may be incomplete: ${body.warning}` : "";
    noticeNode.hidden = !body.warning;
    statusNode.textContent = `Updated ${new Date(body.refreshed_at * 1000).toLocaleTimeString()}`;
  } catch (error) {
    emptyNode.hidden = true;
    noticeNode.textContent = `Could not refresh: ${error.message}`;
    noticeNode.hidden = false;
    statusNode.textContent = "Refresh failed";
  } finally {
    loading = false;
    refreshButton.disabled = false;
    cardsNode.setAttribute("aria-busy", "false");
  }
}

refreshButton.addEventListener("click", refresh);
refresh();
setInterval(refresh, 5000);

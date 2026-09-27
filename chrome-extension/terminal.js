// The terminal page: a live terminal (term-xterm.js): a scratch session's
// (#491, tmux) or an item session's (#563, herdr, shared by everyone watching
// it).
//
// This page is the extension's own, framed over the GitHub page by Open
// (pane-overlay.js, #477), and it talks to no factory itself: a frame under
// github.com is where Chrome's local-network rules can hold a request to a
// factory on a private or tailnet address. The service worker opens the
// socket with the extension's permission and passes it on a port
// (`ssf-term`).
import { factoryUrl } from "./factory-url.js";

const params = new URLSearchParams(location.search);
const url = factoryUrl(params.get("factory"));
const session = String(params.get("session") ?? "");
const takesInput = params.get("input") === "1";
const stateLine = document.getElementById("state");
document.getElementById("session").textContent = session;
// Framed in a floating window, whose title bar already names the session.
document.getElementById("session").hidden = window.top !== window;
document.title = `${session} · ssf`;

function say(text, problem = false) {
  stateLine.textContent = text;
  stateLine.dataset.problem = String(problem);
}

startTerm().catch((error) => say(String(error), true));

async function startTerm() {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  if (!url || !stored.some((item) => factoryUrl(item?.url) === url)) {
    say("this terminal names no configured factory or no session", true);
    return;
  }
  const { run, runItem } = await import("./term-xterm.js");
  if (!session.includes("~")) {
    runItem({
      url,
      session,
      name: String(params.get("viewer") ?? "extension"),
      say,
      box: document.getElementById("xterm"),
      notice: document.getElementById("notice"),
      viewers: document.getElementById("viewers"),
      reconnect: document.getElementById("reconnect"),
      take: document.getElementById("take"),
    });
    return;
  }
  run({
    url,
    session,
    takesInput,
    say,
    box: document.getElementById("xterm"),
    reconnect: document.getElementById("reconnect"),
    resume: document.getElementById("resume"),
  });
}

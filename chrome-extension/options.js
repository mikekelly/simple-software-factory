// The options page: the factories the overlay reads, and the Chrome permission
// each factory's address needs.
//
// Reading a cross-origin factory from the service worker requires a host
// permission for it, and Chrome only grants that after a prompt. The prompt
// needs a user gesture, so it is requested from the Save and Allow buttons
// rather than on load.
//
// Each factory also carries a Writes switch, on unless turned off. Off, the
// service worker refuses an assign and the content script draws no form, so
// this page is the one place a factory is made read-only.
import { factoryUrl, originPattern } from "./factory-url.js";

const rows = document.getElementById("rows");
const status = document.getElementById("status");
const add = document.getElementById("add");
const save = document.getElementById("save");

/// The editable list, in the order shown. Rows are the DOM's.
let entries = [];
/// origin pattern -> whether Chrome currently allows it.
let granted = new Map();

function show(message) {
  status.textContent = message;
}

/// The list as edited, with each URL in its canonical form. Returns the rows
/// that are not usable factory URLs, so Save can name them instead of storing
/// them.
///
/// Each entry carries the row element it came from and the text as typed, so a
/// control acts on that row rather than on a position: rows nobody filled in
/// are skipped, so a position in this list is not a position among the rows.
function collect() {
  const list = [];
  const invalid = [];
  for (const box of rows.querySelectorAll("[data-row]")) {
    const label = box.querySelector(".label").value.trim();
    const text = box.querySelector(".url").value.trim();
    // A row nobody filled in is not a factory.
    if (!text && !label) continue;
    const url = factoryUrl(text);
    if (!url) invalid.push(text || `the row labelled ${label}`);
    list.push({
      box,
      label,
      text,
      url,
      writes: box.querySelector(".writes").checked,
    });
  }
  return { list, invalid };
}

/// What Chrome currently allows, per factory address on the page.
async function refresh() {
  granted = new Map();
  for (const entry of entries) {
    const url = factoryUrl(entry.url);
    if (!url) continue;
    const pattern = originPattern(url);
    granted.set(pattern, await chrome.permissions.contains({ origins: [pattern] }));
  }
}

function render() {
  rows.replaceChildren();
  if (!entries.length) {
    const empty = document.createElement("p");
    empty.className = "empty";
    empty.textContent = "No factories yet.";
    rows.append(empty);
    return;
  }
  entries.forEach((entry) => rows.append(row(entry)));
  showHealth();
}

function row(entry) {
  const box = document.createElement("div");
  box.className = "row";
  box.dataset.row = "";

  const label = document.createElement("label");
  label.className = "field label-field";
  label.append(document.createTextNode("Label"));
  const labelInput = document.createElement("input");
  labelInput.className = "label";
  labelInput.type = "text";
  labelInput.placeholder = "factory-one";
  labelInput.value = entry.label ?? "";
  label.append(labelInput);

  const url = document.createElement("label");
  url.className = "field url-field";
  url.append(document.createTextNode("Capability URL"));
  const urlInput = document.createElement("input");
  urlInput.className = "url";
  urlInput.type = "text";
  urlInput.spellcheck = false;
  urlInput.placeholder = "http://host:8787/<secret>/";
  // What was typed, so a row that is not a factory URL yet keeps its text
  // through Add, Remove and the re-render they cause.
  urlInput.value = entry.url ?? entry.text ?? "";
  url.append(urlInput);

  const state = document.createElement("span");
  state.className = "state";
  if (!entry.url) {
    state.textContent = urlInput.value ? "not a factory URL" : "not saved yet";
    state.dataset.state = "missing";
  } else if (granted.get(originPattern(entry.url))) {
    state.textContent = "allowed";
    state.dataset.state = "allowed";
  } else {
    state.textContent = "not allowed yet";
    state.dataset.state = "blocked";
  }

  const allow = document.createElement("button");
  allow.type = "button";
  allow.className = "allow";
  allow.textContent = "Allow";
  allow.disabled = !entry.url || Boolean(granted.get(originPattern(entry.url)));
  allow.addEventListener("click", async () => {
    const current = factoryUrl(box.querySelector(".url").value);
    if (!current) {
      show("Enter a factory capability URL before allowing it.");
      return;
    }
    const pattern = originPattern(current);
    const allowed = await chrome.permissions.request({ origins: [pattern] });
    show(
      allowed
        ? `Chrome now allows ${pattern}.`
        : `Chrome did not allow ${pattern}; the overlay will not read this factory until it does.`,
    );
    await refresh();
    // Everything typed on the page survives the re-render, the Writes switch
    // included: it is drawn from `entries`, which only `collect` refreshes.
    entries = collect().list;
    render();
  });

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "remove";
  remove.textContent = "Remove";
  remove.addEventListener("click", () => {
    entries = collect().list.filter((entry) => entry.box !== box);
    render();
    show("Removed. Save to keep the change.");
  });

  // On unless this factory was saved with it off; an entry from before the
  // switch existed has no value, and reads as on.
  const writes = document.createElement("div");
  writes.className = "writes-field";
  const writesLabel = document.createElement("label");
  writesLabel.className = "writes-toggle";
  const writesBox = document.createElement("input");
  writesBox.type = "checkbox";
  writesBox.className = "writes";
  writesBox.checked = entry.writes !== false;
  writesLabel.append(writesBox, document.createTextNode("Writes"));
  const writesNote = document.createElement("span");
  writesNote.className = "writes-note";
  // What the switch does, in the factory's own terms. The server refuses a
  // write from any origin but an extension's, so this is the only place the
  // extension itself is told not to write; nothing outside the extension can
  // write through this factory either way.
  const describeWrites = () => {
    writesNote.textContent = writesBox.checked
      ? "On: the overlay can start a session through this factory. The factory refuses writes from any origin but an extension's, so nothing else on this machine can start one through it."
      : "Off: the overlay starts no session through this factory and draws no assign form for it.";
  };
  writesBox.addEventListener("change", describeWrites);
  describeWrites();
  writes.append(writesLabel, writesNote);

  // What the overlay is getting from this factory right now; see `showHealth`.
  const health = document.createElement("p");
  health.className = "health";
  health.dataset.url = entry.url ?? "";

  box.append(label, url, state, allow, remove, writes, health);
  return box;
}

/// The last snapshot the worker sent, so a re-render keeps its health lines.
let factories = [];

/// Fill in each row's health line from the worker's snapshot. The line says
/// whether the factory answered and, when it did, whether it reports the
/// repositories it watches -- which is what the assign form for an item the
/// factory has no record of is drawn from. Without it the two ways that form can
/// be missing from a page (an older `ssf-server` that publishes no repositories,
/// and a capability URL the server has since changed) are both just an absence
/// (#435).
function showHealth() {
  for (const node of rows.querySelectorAll(".health")) {
    const url = node.dataset.url;
    const factory = factories.find((one) => one.url === url);
    if (!url || !factory) {
      node.textContent = "";
      continue;
    }
    const watched = factory.repositories?.length ?? 0;
    if (factory.state === "error") {
      node.textContent = `unreachable: ${factory.error ?? "the factory did not answer"}`;
    } else if (factory.state === "connecting") {
      node.textContent = "not answered yet.";
    } else if (watched) {
      node.textContent = `${factory.state} · reports ${watched} watched ${watched === 1 ? "repository" : "repositories"}.`;
    } else {
      node.textContent = `${factory.state} · reports no watched repositories, so the overlay cannot offer the assign form for an item it has no record of. Either this factory watches none, or it is an older ssf-server that does not publish them.`;
    }
  }
}

// The worker pushes a snapshot when the page connects and on every change, so
// these lines stay current without polling: a URL just saved reads "not
// answered yet" until its stream connects, and then says what came back.
const worker = chrome.runtime.connect({ name: "ssf-overlay" });
worker.onMessage.addListener((message) => {
  if (message?.type !== "snapshot") return;
  factories = message.payload?.factories ?? [];
  showHealth();
});

/// Ask for every address the saved list needs and Chrome does not allow yet.
/// Must run inside the click's user gesture.
async function requestMissing() {
  const missing = entries
    .map((entry) => factoryUrl(entry.url))
    .filter(Boolean)
    .map(originPattern)
    .filter((pattern) => !granted.get(pattern));
  if (!missing.length) return null;
  const allowed = await chrome.permissions.request({ origins: missing });
  await refresh();
  return allowed ? null : missing;
}

add.addEventListener("click", () => {
  entries = collect().list;
  entries.push({ label: "", text: "", url: null, writes: true });
  render();
  const inputs = rows.querySelectorAll(".url");
  inputs[inputs.length - 1]?.focus();
});

save.addEventListener("click", async () => {
  const { list, invalid } = collect();
  if (invalid.length) {
    show(`Not a factory URL: ${invalid.join(", ")}`);
    return;
  }
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  entries = list.map((entry) => ({
    label: entry.label,
    url: entry.url,
    writes: entry.writes,
  }));
  await chrome.storage.local.set({
    factories: entries.map((entry) => ({
      label: entry.label,
      url: entry.url,
      writes: entry.writes,
    })),
  });
  // The list is saved before Chrome is asked, so a refused or unanswered
  // prompt leaves a factory to allow later rather than losing the edit.
  const refused = await requestMissing();
  // An address no factory uses any more is not worth holding.
  const kept = new Set(entries.map((entry) => originPattern(entry.url)));
  const dropped = stored
    .map((entry) => factoryUrl(entry?.url))
    .filter(Boolean)
    .map(originPattern)
    .filter((pattern) => !kept.has(pattern));
  if (dropped.length) {
    await chrome.permissions.remove({ origins: [...new Set(dropped)] });
  }
  await refresh();
  render();
  show(
    refused?.length
      ? `Saved. Chrome did not allow ${refused.join(", ")}; the overlay will not read ${
          refused.length > 1 ? "those factories" : "that factory"
        } until it does.`
      : `Saved ${entries.length} factor${entries.length === 1 ? "y" : "ies"}.`,
  );
});

chrome.permissions.onAdded.addListener(async () => {
  await refresh();
  entries = collect().list;
  render();
});
chrome.permissions.onRemoved.addListener(async () => {
  await refresh();
  entries = collect().list;
  render();
});

(async () => {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  entries = stored.map((entry) => ({
    label: String(entry?.label ?? ""),
    url: String(entry?.url ?? ""),
    writes: entry?.writes !== false,
  }));
  await refresh();
  render();
})();

// The version of the code the browser is actually running, so a copy loaded
// before an update can be told from the current one: `chrome://extensions`
// shows it too, but the page a reader is already on should answer "is this the
// build with the fix?" without them going to look (#435).
document.getElementById("version").textContent =
  `ssf overlay ${chrome.runtime.getManifest().version} — press Reload on chrome://extensions after updating this directory`;

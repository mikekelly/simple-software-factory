// The options page: the factories the overlay reads, and the Chrome permission
// each factory's address needs.
//
// Reading a cross-origin factory from the service worker requires a host
// permission for it, and Chrome only grants that after a prompt. The prompt
// needs a user gesture, so it is requested from the Save and Allow buttons
// rather than on load.
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
function collect() {
  const list = [];
  const invalid = [];
  for (const row of rows.querySelectorAll("[data-row]")) {
    const label = row.querySelector(".label").value.trim();
    const raw = row.querySelector(".url").value.trim();
    // A row nobody filled in is not a factory.
    if (!raw && !label) continue;
    const url = factoryUrl(raw);
    if (!url) invalid.push(raw || `the row labelled ${label}`);
    list.push({ label, url });
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
  entries.forEach((entry, index) => rows.append(row(entry, index)));
}

function row(entry, index) {
  const box = document.createElement("div");
  box.className = "row";
  box.dataset.row = String(index);

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
  urlInput.value = entry.url ?? "";
  url.append(urlInput);

  const state = document.createElement("span");
  state.className = "state";
  const canonical = factoryUrl(entry.url);
  if (!canonical) {
    state.textContent = entry.url ? "not a factory URL" : "not saved yet";
    state.dataset.state = "missing";
  } else if (granted.get(originPattern(canonical))) {
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
  allow.disabled = !canonical || Boolean(granted.get(originPattern(canonical)));
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
    render();
  });

  const remove = document.createElement("button");
  remove.type = "button";
  remove.className = "remove";
  remove.textContent = "Remove";
  remove.addEventListener("click", () => {
    entries = collect().list.filter((_, position) => position !== index);
    render();
    show("Removed. Save to keep the change.");
  });

  box.append(label, url, state, allow, remove);
  return box;
}

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
  entries.push({ label: "", url: "" });
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
  entries = list.map((entry) => ({ label: entry.label, url: entry.url }));
  await chrome.storage.local.set({
    factories: entries.map((entry) => ({ label: entry.label, url: entry.url })),
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
  render();
});
chrome.permissions.onRemoved.addListener(async () => {
  await refresh();
  render();
});

(async () => {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  entries = stored.map((entry) => ({
    label: String(entry?.label ?? ""),
    url: String(entry?.url ?? ""),
  }));
  await refresh();
  render();
})();

// The options page: the factories the overlay reads, and the Chrome permission
// each factory's address needs.
//
// Reading a cross-origin factory from the service worker requires a host
// permission for it, and Chrome only grants that after a prompt. The prompt
// needs a user gesture, so it is requested from the button that saves or allows
// a row rather than on load.
//
// The page states rather than asks (#439). A factory is added by filling in one
// form and saving it, which stores it and asks Chrome for its address in the
// same click; a saved factory is a fact on the page -- what it is called, where
// it is, what the overlay is getting from it -- with Edit and Remove as the two
// deliberate ways to change it, so nothing that is saved sits in a field that
// still looks as if it were being typed. Every row says out loud what happened
// to it: connecting, connected and what it reports, or why it is not.
import { factoryUrl, originPattern, factoryLabel } from "./factory-url.js";

const rows = document.getElementById("rows");
const status = document.getElementById("status");
const add = document.getElementById("add");

/// The list, in the order shown. Each entry is `{label, url, writes, editing}`:
/// a saved entry's `url` is the canonical factory URL, and an editing one holds
/// what has been typed in `label` and `url` until Save or Cancel. The page is
/// rendered from this and never read back out of the inputs, so a re-render (a
/// permission change, a fresh snapshot) cannot lose an edit that is under way.
let entries = [];
/// origin pattern -> whether Chrome currently allows it, from the last check.
let granted = new Map();
/// The canonical URL a save is asking Chrome about, so its row can say so while
/// the prompt is open.
let awaiting = null;
/// How often the page pings the worker to keep it from being evicted. The same
/// interval the content script uses.
const PING_MS = 20000;

function show(message) {
  status.textContent = message;
}

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

/// The saved entries, in order, as they are stored: `chrome.storage` holds an
/// array of `{label, url, writes}`, so every other shape is this page's.
///
/// A row being edited is stored as it was, from the snapshot Edit took, not
/// omitted: a second row's Remove or Save writes this list, and a factory that
/// disappeared from storage because someone was halfway through editing it is
/// the one loss this page cannot show. Cancel is what puts the snapshot back on
/// the page, so storage has to have kept it all along.
function saved() {
  return entries
    .map((entry) => (entry.editing ? entry.saved : entry))
    .filter((entry) => entry?.url)
    .map(({ label, url, writes }) => ({ label, url, writes }));
}

async function persist() {
  await chrome.storage.local.set({ factories: saved() });
}

/// What Chrome currently allows, per saved factory address on the page.
async function checkPermissions() {
  const next = new Map();
  for (const entry of entries) {
    if (entry.editing) continue;
    const pattern = originPattern(entry.url);
    next.set(pattern, await chrome.permissions.contains({ origins: [pattern] }));
  }
  granted = next;
}

function render() {
  rows.replaceChildren();
  if (!entries.length) {
    rows.append(
      element(
        "p",
        "empty",
        "No factories yet. Add the capability URL of one to see its agents on github.com.",
      ),
    );
  }
  for (const entry of entries) {
    rows.append(entry.editing ? editRow(entry) : savedRow(entry));
  }
  showHealth();
}

/// A saved factory: what it is, where it is, and what the overlay is getting
/// from it. Nothing here is a field, because nothing here is being edited.
function savedRow(entry) {
  const box = element("div", "row");
  box.dataset.row = "";
  box.dataset.url = entry.url;

  const head = element("div", "head");
  head.append(
    element("span", "name", entry.label || factoryLabel(entry.url)),
    element("code", "where", entry.url),
  );
  box.append(head);

  const pattern = originPattern(entry.url);
  const allowed = granted.get(pattern) === true;
  const controls = element("div", "controls");
  const state = element("span", "state");
  if (awaiting === entry.url) {
    state.textContent = "asking Chrome for permission…";
    state.dataset.state = "waiting";
  } else if (allowed) {
    state.textContent = "allowed";
    state.dataset.state = "allowed";
  } else {
    state.textContent = "not allowed yet";
    state.dataset.state = "blocked";
    // The one control a refused prompt leaves: the same request, made again on
    // purpose, beside the row that needs it.
    const allow = element("button", undefined, "Allow");
    allow.type = "button";
    allow.addEventListener("click", () => ask(entry, allow));
    controls.append(allow);
  }
  controls.prepend(state);

  const edit = element("button", undefined, "Edit");
  edit.type = "button";
  edit.addEventListener("click", () => {
    // What Cancel goes back to, captured here so the row that was saved is not
    // lost to an edit nobody finished.
    entry.saved = { label: entry.label, url: entry.url, writes: entry.writes };
    entry.editing = true;
    render();
    rows.querySelector(".row.editing .url-input")?.focus();
  });
  const remove = element("button", undefined, "Remove");
  remove.type = "button";
  remove.addEventListener("click", () => drop(entry));
  controls.append(edit, remove);

  box.append(controls);
  box.append(element("p", "health", healthLine(entry.url)));
  box.append(
    element(
      "p",
      "writes",
      entry.writes
        ? "Writes on: the overlay can start a session through this factory."
        : "Writes off: the overlay starts no session through this factory.",
    ),
  );
  return box;
}

/// A factory being added or changed: the only state in which this page has
/// fields, and the only state in which anything is unsaved.
function editRow(entry) {
  const box = element("div", "row editing");
  box.dataset.row = "";

  const label = element("label", "field");
  label.append(element("span", undefined, entry.saved ? "Label" : "Label (optional)"));
  const labelInput = element("input", "label-input");
  labelInput.type = "text";
  labelInput.placeholder = "factory-one";
  labelInput.value = entry.label;
  labelInput.addEventListener("input", () => {
    entry.label = labelInput.value;
  });
  label.append(labelInput);

  const url = element("label", "field");
  url.append(element("span", undefined, "Capability URL"));
  const urlInput = element("input", "url-input");
  urlInput.type = "text";
  urlInput.spellcheck = false;
  urlInput.autocomplete = "off";
  urlInput.placeholder = "http://host:8787/<secret>/";
  urlInput.value = entry.url;
  urlInput.addEventListener("input", () => {
    entry.url = urlInput.value;
    error.hidden = true;
  });
  url.append(urlInput);

  const error = element("p", "row-error");
  error.hidden = true;

  const writes = element("label", "writes-toggle");
  const writesBox = element("input", "writes");
  writesBox.type = "checkbox";
  writesBox.checked = entry.writes;
  writesBox.addEventListener("change", () => {
    entry.writes = writesBox.checked;
  });
  writes.append(writesBox, document.createTextNode("Writes"));
  const writesNote = element(
    "span",
    "writes-note",
    "On: the overlay can start a session through this factory. Off: it draws no assign form for it.",
  );
  const writesField = element("div", "writes-field");
  writesField.append(writes, writesNote);

  const controls = element("div", "controls");
  const save = element("button", "primary", entry.saved ? "Save changes" : "Save and allow");
  save.type = "button";
  save.addEventListener("click", () => saveEntry(entry, error));
  const cancel = element("button", undefined, "Cancel");
  cancel.type = "button";
  cancel.addEventListener("click", () => {
    // A row that was never saved has nothing to go back to, so Cancel is what
    // removes it.
    if (entry.saved) {
      Object.assign(entry, entry.saved, { editing: false });
      delete entry.saved;
      show("Nothing changed.");
    } else {
      entries = entries.filter((one) => one !== entry);
      show("Not added.");
    }
    render();
  });
  controls.append(save, cancel);

  box.append(label, url, error, writesField, controls);
  return box;
}

/// Save one row: store the list, and ask Chrome for the address in the same
/// click. Chrome's prompt needs the gesture, so the request is made before
/// anything is awaited; the list is stored whatever the answer, so a refused or
/// abandoned prompt leaves a factory to allow later rather than losing the edit.
function saveEntry(entry, error) {
  const url = factoryUrl(entry.url);
  if (!url) {
    error.textContent = entry.url.trim()
      ? "Not a factory capability URL: it looks like http://host:8787/<secret>/ (see the note above)."
      : "Enter the factory's capability URL.";
    error.hidden = false;
    return;
  }
  if (entries.some((one) => one !== entry && !one.editing && one.url === url)) {
    error.textContent = "That factory is already saved.";
    error.hidden = false;
    return;
  }
  const pattern = originPattern(url);
  // The address this row was stored under before the edit. A factory moved to
  // another host or port leaves its permission behind otherwise -- Chrome keeps
  // it, `granted` stops mentioning it, and no row is left to revoke it from --
  // which would make `optional_host_permissions` every address this page has
  // ever held rather than the ones on it.
  const before = entry.saved ? originPattern(entry.saved.url) : null;
  const already = granted.get(pattern) === true;
  const ask = already ? Promise.resolve(true) : chrome.permissions.request({ origins: [pattern] });
  entry.label = entry.label.trim();
  entry.url = url;
  entry.editing = false;
  if (entry.saved) delete entry.saved;
  awaiting = already ? null : url;
  render();
  show(`Saved ${entry.label || factoryLabel(url)}.`);
  // Stores the list, and gives the address this row was on back to Chrome when
  // nothing else is using it.
  forget([before]);
  ask.then(
    async (allowed) => {
      awaiting = null;
      await checkPermissions();
      render();
      show(
        allowed
          ? `${entry.label || factoryLabel(url)} is saved and allowed. The badge on github.com reads it now.`
          : `${entry.label || factoryLabel(url)} is saved, but Chrome did not allow ${pattern}. Press Allow on its row when you are ready.`,
      );
    },
    async (failure) => {
      awaiting = null;
      await checkPermissions();
      render();
      show(`${entry.label || factoryLabel(url)} is saved. Chrome could not be asked (${failure}).`);
    },
  );
}

/// Ask again for one saved factory's address: what a refused prompt leaves.
function ask(entry, button) {
  const pattern = originPattern(entry.url);
  button.disabled = true;
  chrome.permissions.request({ origins: [pattern] }).then(
    async (allowed) => {
      await checkPermissions();
      render();
      show(
        allowed
          ? `Chrome now allows ${pattern}.`
          : `Chrome did not allow ${pattern}; the overlay will not read this factory until it does.`,
      );
    },
    async (failure) => {
      await checkPermissions();
      render();
      show(`Chrome could not be asked (${failure}).`);
    },
  );
}

/// Ask Chrome to forget the addresses no saved factory uses any more. This is
/// what keeps `optional_host_permissions` to the factories on the page rather
/// than every address one has ever been saved under; `drop` and a save that
/// moved a factory both need it, and neither can be the only caller.
async function forget(patterns) {
  await persist();
  for (const pattern of patterns) {
    if (!pattern) continue;
    if (saved().some((one) => originPattern(one.url) === pattern)) continue;
    await chrome.permissions.remove({ origins: [pattern] });
  }
}

/// Remove one factory: from the list, and from Chrome's permissions when no
/// other saved factory uses that address.
function drop(entry) {
  const pattern = originPattern(entry.url);
  const label = entry.label || factoryLabel(entry.url);
  entries = entries.filter((one) => one !== entry);
  render();
  forget([pattern]).then(async () => {
    await checkPermissions();
    render();
    show(`Removed ${label}.`);
  });
}

/// The last snapshot the worker sent, so a re-render keeps its health lines.
let factories = [];

/// What the overlay is getting from one factory right now: whether it answered,
/// and -- when it did -- whether it reports the repositories it watches, which
/// is what the assign form for an item the factory has no record of is drawn
/// from. Without it the two ways that form can be missing from a page (an older
/// `ssf-server` that publishes no repositories, and a capability URL the server
/// has since changed) are both just an absence (#435).
function healthLine(url) {
  const factory = factories.find((one) => one.url === url);
  // A factory saved a moment ago reads "connecting" rather than blank: the
  // line that goes empty exactly when someone has just saved is the silence
  // this exists to remove (#435, #439).
  if (!factory) return "connecting…";
  if (factory.state === "error") {
    return `unreachable: ${factory.error ?? "the factory did not answer"}`;
  }
  if (factory.state === "connecting") return "connecting…";
  const watched = factory.repositories?.length ?? 0;
  if (watched) {
    return `${factory.state} · reports ${watched} watched ${
      watched === 1 ? "repository" : "repositories"
    }`;
  }
  return `${factory.state} · reports no watched repositories, so the overlay cannot offer the assign form for an item it has no record of. Either this factory watches none, or it is an older ssf-server that does not publish them.`;
}

function showHealth() {
  for (const node of rows.querySelectorAll(".health")) {
    const url = node.closest(".row")?.dataset.url;
    node.textContent = url ? healthLine(url) : "";
  }
}

// The worker pushes a snapshot when the page connects and on every change, so
// these lines stay current without polling. The ping is what keeps the worker
// alive -- an open port does not reset its idle timer, only messages do -- and a
// worker that is stopped closes the port, so the page opens another: without
// both, a page left open would freeze its lines and a factory saved afterwards
// would read blank, which is the silence these lines exist to remove (#435).
// The timer is replaced rather than added to on each reconnect, so a page that
// reconnects many times still holds one.
let pingTimer = null;
function connect() {
  const worker = chrome.runtime.connect({ name: "ssf-overlay" });
  worker.onMessage.addListener((message) => {
    if (message?.type !== "snapshot") return;
    factories = message.payload?.factories ?? [];
    showHealth();
  });
  worker.onDisconnect.addListener(() => setTimeout(connect, 1000));
  clearInterval(pingTimer);
  pingTimer = setInterval(() => {
    try {
      worker.postMessage({ type: "ping" });
    } catch {
      // The port is gone; `onDisconnect` opens another.
    }
  }, PING_MS);
}
connect();

add.addEventListener("click", () => {
  if (entries.some((entry) => entry.editing)) {
    show("Finish the factory you are adding first.");
    return;
  }
  entries.push({ label: "", url: "", writes: true, editing: true });
  render();
  show("Enter the factory's capability URL, then Save and allow.");
  rows.querySelector(".row.editing .label-input")?.focus();
});

chrome.permissions.onAdded.addListener(async () => {
  await checkPermissions();
  render();
});
chrome.permissions.onRemoved.addListener(async () => {
  await checkPermissions();
  render();
});

(async () => {
  const stored = (await chrome.storage.local.get("factories")).factories ?? [];
  entries = stored
    .map((entry) => ({
      label: String(entry?.label ?? ""),
      url: factoryUrl(entry?.url) ?? "",
      writes: entry?.writes !== false,
      editing: false,
    }))
    .filter((entry) => entry.url);
  await checkPermissions();
  render();
})();

// The version of the code the browser is actually running, so a copy loaded
// before an update can be told from the current one: `chrome://extensions`
// shows it too, but the page a reader is already on should answer "is this the
// build with the fix?" without them going to look (#435).
document.getElementById("version").textContent =
  `ssf overlay ${chrome.runtime.getManifest().version} — press Reload on chrome://extensions after updating this directory`;

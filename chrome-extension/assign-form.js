// The Assign agent form, drawn by the sidebar card and by the list/board
// popover of an item whose state is No agent.
//
// Loaded as a content script before content.js, in the same isolated world.
// content.js asks this module for the node to draw for an item, hands it every
// snapshot, adopts `STYLE` into its shadow sheet, and calls `onChange` with its
// own scheduler:
//
//   ssfAssignForm.onChange(scheduleRender);
//   const node = ssfAssignForm.render({ factories, repo, number });
//   ssfAssignForm.applySnapshot(payload);
//
// `factories` are the configured factories whose snapshot shows this item --
// the same ones the card is drawn from. A factory whose Writes switch is off
// is not one of them, so turning it off hides the form for that factory here as
// well as refusing the write in the service worker.
//
// Every piece of state lives here rather than in the nodes, because content.js
// rebuilds what it draws on every GitHub mutation and every snapshot frame. A
// picker half-way through being chosen would otherwise be thrown away twice a
// second.
//
// Nothing here talks to a factory. The service worker carries the write and the
// two listings, so the request's Origin is the extension's; this module only
// asks it to and shows the answer. Nothing is retried: a refusal is the
// person's to repeat, in the server's own words.
(() => {
  const DEFAULT = "";
  /// How long "Assigning…" waits for a snapshot that shows the item with an
  /// agent before it shows what the factory said instead.
  const TIMEOUT_MS = 30000;

  const STYLE = `
.ssf-assign { display: flex; flex-direction: column; gap: 6px; min-width: 0;
  padding-top: 6px; border-top: 1px solid var(--borderColor-muted, #d1d9e0); }
.ssf-assign-head { font-weight: 600; color: var(--fgColor-default, #1f2328); }
.ssf-assign-field { display: flex; align-items: center; gap: 6px; min-width: 0;
  color: var(--fgColor-muted, #59636e); }
.ssf-assign-field > span { flex: 0 0 4.5em; }
.ssf-assign select { flex: 1 1 auto; min-width: 0; max-width: 100%;
  font: inherit; color: inherit; background: var(--bgColor-default, #ffffff);
  border: 1px solid var(--borderColor-default, #d1d9e0); border-radius: 6px;
  padding: 2px 4px; }
.ssf-assign-actions { display: flex; gap: 8px; }
.ssf-assign button { font: inherit; color: var(--fgColor-default, #1f2328);
  background: var(--bgColor-default, #ffffff);
  border: 1px solid var(--borderColor-default, #d1d9e0); border-radius: 6px;
  padding: 3px 10px; cursor: pointer; }
.ssf-assign button.assign { color: #ffffff; background: var(--fgColor-success, #1a7f37);
  border-color: transparent; font-weight: 600; }
.ssf-assign button:disabled { opacity: 0.5; cursor: default; }
.ssf-assign-note { margin: 0; color: var(--fgColor-muted, #59636e); }
.ssf-assign-error { margin: 0; color: var(--fgColor-danger, #cf222e); }
.ssf-assign-stack { margin: 0; color: var(--fgColor-default, #1f2328); font-weight: 600; }
.ssf-assign a { color: var(--fgColor-accent, #0969da); }
`;

  /// Renders again once a listing has arrived; content.js sets this to its own
  /// scheduler.
  let redraw = () => {};

  /// `owner/name#N` -> state, one per item the form is being drawn for.
  const forms = new Map();

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  function fresh(repo, number) {
    return {
      repo,
      number,
      itemId: `${repo}#${number}`,
      /// The factories that may take this write, and the one it goes to.
      choices: [],
      label: "",
      url: null,
      agents: null,
      agentsPending: false,
      agentsError: null,
      harness: DEFAULT,
      models: null,
      modelsFor: null,
      modelsPending: false,
      modelsError: null,
      model: DEFAULT,
      effort: DEFAULT,
      /// The server's words about a refused assign; the form stays.
      error: null,
      /// A write is in the air: the asking, before the factory has answered.
      busy: false,
      /// `{at, result, timer, timedOut}` while the item waits for the snapshot
      /// that shows its agent.
      pending: null,
      result: null,
    };
  }

  /// The factories that may take this write for this item: those whose Writes
  /// switch is on. One of them means no factory picker, several mean one.
  function contenders(factories) {
    return (factories ?? [])
      .filter((factory) => factory?.url && factory.writes !== false)
      .map((factory) => ({ url: factory.url, label: factory.label || factory.url }));
  }

  /// Point the form at one factory, forgetting what the last one answered: its
  /// agents and models are its own.
  function chooseFactory(state, choices, url) {
    state.choices = choices;
    if (state.url === url) return;
    state.url = url;
    state.label = choices.find((choice) => choice.url === url)?.label ?? "";
    state.agents = null;
    state.agentsFor = null;
    state.agentsPending = false;
    state.agentsError = null;
    state.harness = DEFAULT;
    state.models = null;
    state.modelsFor = null;
    state.modelsPending = false;
    state.modelsError = null;
    state.model = DEFAULT;
    state.effort = DEFAULT;
    state.error = null;
  }

  /// The agents out of `ssf agents --json`, which is an array of records; a
  /// bare array of ids also reads. A harness says which of the optional
  /// settings it takes at all, and `ssf assign` refuses one it does not.
  function agentList(body) {
    const rows = Array.isArray(body) ? body : (body?.agents ?? []);
    return rows
      .map((row) =>
        typeof row === "string"
          ? { id: row, name: row, takesModel: true, efforts: [] }
          : {
              id: String(row?.id ?? ""),
              name: String(row?.name ?? row?.id ?? ""),
              takesModel: row?.takes_model !== false,
              efforts: Array.isArray(row?.effort_levels) ? row.effort_levels : [],
            },
      )
      .filter((row) => row.id);
  }

  /// The model ids out of `ssf models <harness>`' listing, which is the
  /// `{harness, models, source}` object `--json` prints; a bare array also
  /// reads.
  function modelList(body) {
    const ids = Array.isArray(body) ? body : (body?.models ?? []);
    return ids.map(String).filter(Boolean);
  }

  async function ask(message) {
    try {
      return await chrome.runtime.sendMessage(message);
    } catch (error) {
      return { ok: false, error: `the extension's service worker did not answer (${error})` };
    }
  }

  function loadAgents(state) {
    // A failure stands until Try again clears it: content.js re-renders on
    // every mutation and every frame, and none of that is a reason to ask a
    // factory that just answered the same question again.
    if (!state.url || state.agentsPending || state.agents || state.agentsError) return;
    const url = state.url;
    state.agentsPending = true;
    ask({ type: "ssf:agents", url }).then((reply) => {
      // A slow answer for a factory the form has since left is not an answer.
      if (state.url !== url) return;
      state.agentsPending = false;
      if (reply?.ok) {
        state.agents = agentList(reply.body);
        state.agentsError = null;
      } else {
        state.agentsError = reply?.error ?? "the factory did not answer";
      }
      redraw();
    });
  }

  function loadModels(state, harness) {
    if (!state.url || !harness || state.modelsFor === harness) return;
    const url = state.url;
    state.modelsPending = true;
    ask({ type: "ssf:models", url, harness }).then((reply) => {
      if (state.url !== url) return;
      state.modelsPending = false;
      state.modelsFor = harness;
      if (reply?.ok) {
        state.models = modelList(reply.body);
        state.modelsError = null;
      } else {
        state.models = null;
        state.modelsError = reply?.error ?? "the factory did not answer";
      }
      redraw();
    });
  }

  /// A picker: its label and one option per value, the first being the
  /// harness's own default.
  function select(label, values, chosen, onPick) {
    const field = element("label", "ssf-assign-field");
    field.append(element("span", undefined, label));
    const box = element("select");
    for (const { value, text } of values) {
      const option = element("option", undefined, text);
      option.value = value;
      box.append(option);
    }
    box.value = chosen;
    box.addEventListener("change", () => onPick(box.value));
    field.append(box);
    return field;
  }

  /// What the pickers hold, given what this factory has answered so far.
  ///
  /// Model and effort are offered only as far as the chosen harness takes
  /// them: `ssf assign` refuses a setting a harness does not accept, so
  /// offering one would be offering a refusal. A harness that lists no effort
  /// levels takes none, which is why nothing but the default appears.
  function stack(state) {
    const agents = state.agents ?? [];
    const chosen = agents.find((agent) => agent.id === state.harness);
    const models = chosen && !chosen.takesModel ? [] : (state.models ?? []);
    const levels = chosen?.efforts ?? [];
    return {
      harnesses: [
        { value: DEFAULT, text: state.agentsPending ? "loading…" : "Choose a harness" },
        ...agents.map((agent) => ({ value: agent.id, text: `${agent.id} · ${agent.name}` })),
      ],
      models: [
        { value: DEFAULT, text: "harness default" },
        ...models.map((id) => ({ value: id, text: id })),
      ],
      efforts: [
        { value: DEFAULT, text: "harness default" },
        ...levels.map((level) => ({ value: level, text: level })),
      ],
      ready: Boolean(chosen),
    };
  }

  function form(state) {
    const body = element("div", "ssf-assign");
    body.append(element("div", "ssf-assign-head", "Assign agent"));

    // Two factories watching this repository: which one takes the session is
    // not something to guess. One: no picker, nothing to choose.
    if (state.choices.length > 1) {
      const values = state.choices.map((choice) => ({ value: choice.url, text: choice.label }));
      body.append(
        select("Factory", values, state.url, (url) => {
          chooseFactory(state, state.choices, url);
          loadAgents(state);
          redraw();
        }),
      );
    }

    if (state.agentsError) {
      body.append(element("p", "ssf-assign-error", state.agentsError));
      const retry = element("button", undefined, "Try again");
      retry.type = "button";
      retry.addEventListener("click", () => {
        state.agentsError = null;
        loadAgents(state);
        redraw();
      });
      body.append(retry);
      return body;
    }

    const { harnesses, models, efforts, ready } = stack(state);
    body.append(
      select("Harness", harnesses, state.harness, (value) => {
        state.harness = value;
        state.model = DEFAULT;
        state.models = null;
        state.modelsFor = null;
        state.modelsError = null;
        loadModels(state, value);
        redraw();
      }),
      select("Model", models, state.model, (value) => {
        state.model = value;
      }),
      select("Effort", efforts, state.effort, (value) => {
        state.effort = value;
      }),
    );
    if (state.modelsError) body.append(element("p", "ssf-assign-error", state.modelsError));
    if (state.error) body.append(element("p", "ssf-assign-error", state.error));

    const actions = element("div", "ssf-assign-actions");
    const assign = element("button", "assign", "Assign");
    assign.type = "button";
    assign.disabled = !ready || state.modelsPending;
    assign.addEventListener("click", () => submit(state));
    actions.append(assign);
    body.append(actions);
    body.append(
      element(
        "p",
        "ssf-assign-note",
        // An item's own stack is the repository's, and guessing a harness
        // would start a session on one nobody asked for.
        ready
          ? "Starts a session on this item with the factory's own `ssf assign`."
          : "Pick a harness to start a session on this item.",
      ),
    );
    return body;
  }

  /// A write in the air, or a wait for the frame that shows its agent.
  function submitting(state) {
    const body = element("div", "ssf-assign");
    body.append(element("div", "ssf-assign-head", "Assigning\u2026"));
    const chosen = [state.harness, state.model, state.effort].filter(Boolean).join(" \u00b7 ");
    body.append(
      element("p", "ssf-assign-stack", chosen),
      element("p", "ssf-assign-note", "waiting for the factory to show an agent on this item"),
    );
    return body;
  }

  /// The two facts worth reading out of `ssf assign --json`: the stack it
  /// started and whether the assignment itself landed on GitHub.
  function resultLine(state) {
    const result = state.result;
    if (!result || typeof result !== "object") return String(result ?? "");
    const to = result.to ?? {};
    const stack = [to.harness, to.model, to.effort].filter(Boolean).join(" \u00b7 ");
    const facts = [];
    if (result.session) facts.push(String(result.session));
    if (stack) facts.push(stack);
    facts.push(result.assigned === false ? "not assigned on GitHub" : "assigned on GitHub");
    if (result.overrides_written === false) facts.push("this item's model settings were not written");
    return facts.join(" \u00b7 ");
  }

  /// What the factory said about a write it accepted but has not shown an
  /// agent for, and where to look.
  function settled(state) {
    const body = element("div", "ssf-assign");
    body.append(
      element("div", "ssf-assign-head", "Assign accepted"),
      element("p", "ssf-assign-stack", resultLine(state)),
      element("p", "ssf-assign-note", "the factory has not shown an agent on this item yet."),
    );
    const link = element("a", undefined, "Open the factory dashboard");
    link.href = state.url;
    link.target = "_blank";
    link.rel = "noreferrer";
    const line = element("p", "ssf-assign-note");
    line.append(link);
    body.append(line);
    return body;
  }

  function submit(state) {
    state.error = null;
    state.result = null;
    // Drawn at once rather than after the round trip; `pending` starts only
    // when the factory has accepted the write, so a snapshot arriving before
    // the response cannot end a wait that has not begun.
    state.busy = true;
    redraw();
    ask({
      type: "ssf:assign",
      url: state.url,
      repo: state.repo,
      number: state.number,
      harness: state.harness,
      model: state.model,
      effort: state.effort,
    }).then((reply) => {
      state.busy = false;
      if (!reply?.ok) {
        // The form stays: the server's own words, and nothing retried.
        state.error = reply?.error ?? "the factory did not answer";
        redraw();
        return;
      }
      state.result = reply.result;
      state.pending = { at: Date.now(), timedOut: false };
      state.pending.timer = setTimeout(() => {
        state.pending.timedOut = true;
        redraw();
      }, TIMEOUT_MS);
      redraw();
    });
  }

  /// The node to draw for one item, in whatever state the form is now, or
  /// `null` when nothing may be drawn for it: no factory that watches this
  /// item accepts writes.
  function render({ factories, repo, number }) {
    const key = `${repo}#${number}`;
    const state = forms.get(key) ?? fresh(repo, number);
    forms.set(key, state);
    const choices = contenders(factories);
    if (!choices.length) return null;
    chooseFactory(state, choices, choices.some((one) => one.url === state.url) ? state.url : choices[0].url);
    if (!state.pending && !state.busy) loadAgents(state);
    if (state.harness && !state.models && !state.modelsPending && !state.modelsError) {
      loadModels(state, state.harness);
    }
    if (state.busy) return submitting(state);
    if (state.pending) return state.pending.timedOut ? settled(state) : submitting(state);
    return form(state);
  }

  /// Every snapshot, so a waiting form can see the item gain its agent. That
  /// frame, not the HTTP response, is what ends "Assigning…": the response
  /// only says the factory accepted the write.
  ///
  /// The card that frame carries is what the item shows from then on, so the
  /// wait, its result and its timer all go; nothing of the form is left to
  /// draw over it.
  function applySnapshot(payload) {
    let changed = false;
    for (const state of forms.values()) {
      if (!state.pending) continue;
      const factory = (payload?.factories ?? []).find((one) => one.url === state.url);
      const card = (factory?.cards ?? []).find(
        (one) =>
          one.origin?.id === state.itemId ||
          (one.additional ?? []).some((issue) => issue?.id === state.itemId),
      );
      if (!card) continue;
      // A timed-out wait stays: it is the only thing on screen saying the
      // factory accepted a write it is not showing.
      if (!state.pending.timedOut) {
        clearTimeout(state.pending.timer);
        state.pending = null;
        state.result = null;
        changed = true;
      }
    }
    if (changed) redraw();
  }

  globalThis.ssfAssignForm = {
    STYLE,
    render,
    applySnapshot,
    onChange(callback) {
      redraw = callback;
    },
  };
})();

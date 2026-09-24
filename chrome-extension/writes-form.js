// An item's writes, drawn by the sidebar card and by the list/board popover.
//
// Loaded as a content script after pane-overlay.js and before content.js, in
// the same isolated world.
// content.js asks this module for the node to draw for an item, hands it every
// snapshot, adopts `STYLE` into its shadow sheet, and calls `onChange` with its
// own scheduler:
//
//   ssfWrites.onChange(scheduleRender);
//   const node = ssfWrites.render({ factories, repo, number });          // No agent
//   const node = ssfWrites.renderActions({ factories, repo, number, item }); // an agent
//   const node = ssfWrites.renderScratch({ factories, repo, login, sessions }); // a repository
//   ssfWrites.applySnapshot(payload);
//
// `render` is the Assign agent form, for an item with no agent; one form per
// item, its own Factory picker choosing among the factories that accept the
// write. Handlers are set as `onclick`/`onchange`/`oninput` properties, never
// with addEventListener: the page keeps a mounted node and copies a new
// frame's handlers onto it, so a reused button does what the new one does.
// `renderActions` is the Actions row, for an item whose state is Working,
// Waiting on you, Done or Problem: Hand over… (the assign pickers, prefilled
// with the stack the card is on, plus a note) and Release (a confirm step naming
// the branch). It is drawn once per factory that has an agent on the item --
// `factories` is that one factory -- so two factories working one item are two
// rows, each writing through its own factory, with its own state. `item` is the
// card's item object, which is what the pickers are prefilled from.
//
// `renderScratch` is a repository page's scratch sessions (#414): each with
// Open (pane-overlay.js), Kill or Resume, and New scratch, the assign pickers
// plus whose it is.
// A kill the factory's checks refuse is asked again, once, naming what will be
// lost; a clean workspace goes on the first click.
//
// Nothing here types at an agent: a person speaks to an item's agent on the
// item, where the exchange is in the item's record. This module starts, moves
// and frees sessions -- the things that are not a comment (#439). Open, the
// one way to an agent's own terminal, is pane-overlay.js's: an item's is at the
// top of its card, which content.js draws, and a scratch session's at the top
// of its row here.
//
// `factories` are the configured factories whose snapshot shows this item --
// the same ones the card is drawn from. A factory whose Writes switch is off
// is not one of them, so turning it off hides the form for that factory here as
// well as refusing the write in the service worker.
//
// Every piece of state lives here rather than in the nodes. content.js keeps
// the nodes it has drawn -- a form is not replaced under a picker's open popup
// or a caret -- but it draws a fresh tree on every GitHub mutation and every
// snapshot frame, and puts a new node in place wherever a frame's own shape
// differs, so a redraw must never be the thing that loses what the person was
// in the middle of: the picker they were choosing from, the note they were
// writing, the confirm step they had open.
//
// Nothing here talks to a factory. The service worker carries every write and
// the two listings, so the request's Origin is the extension's; this module
// only asks it to and shows the answer. Nothing is retried: a refusal is the
// person's to repeat, in the server's own words.
(() => {
  const DEFAULT = "";
  /// How long "Assigning…" waits for a snapshot that shows the item with an
  /// agent before it shows what the factory said instead.
  const TIMEOUT_MS = 30000;
  /// How many items' form state the module keeps. A person visits a handful of
  /// items in a session; the rest are dead weight.
  const REMEMBER_ITEMS = 16;

  const STYLE = `
/* The writes sit under the card's own text, behind the same 1px rule the card
   uses to separate itself from the page. */
.ssf-writes { display: flex; flex-direction: column; gap: 6px; min-width: 0;
  padding-top: 6px; border-top: 1px solid var(--borderColor-muted, #d1d9e0); }
.ssf-writes-head { font-weight: 600; color: var(--fgColor-default, #1f2328); }
.ssf-writes-field { display: flex; align-items: center; gap: 6px; min-width: 0;
  color: var(--fgColor-muted, #59636e); }
.ssf-writes-field > span { flex: 0 0 4.5em; }
.ssf-writes select { flex: 1 1 auto; min-width: 0; max-width: 100%;
  font: inherit; color: inherit; background: var(--bgColor-default, #ffffff);
  border: 1px solid var(--borderColor-default, #d1d9e0); border-radius: 6px;
  padding: 2px 4px; }
/* The hand-over note: the only text box left, and the only place this
   extension takes typed text at all (#439). */
.ssf-writes textarea { min-width: 0; max-width: 100%; resize: vertical;
  font: inherit; color: inherit; background: var(--bgColor-default, #ffffff);
  border: 1px solid var(--borderColor-default, #d1d9e0); border-radius: 6px;
  padding: 3px 6px; }
.ssf-writes-actions { display: flex; gap: 8px; }
.ssf-writes button { font: inherit; color: var(--fgColor-default, #1f2328);
  background: var(--bgColor-default, #ffffff);
  border: 1px solid var(--borderColor-default, #d1d9e0); border-radius: 6px;
  padding: 3px 10px; cursor: pointer; }
.ssf-writes button:hover:not(:disabled) { background: var(--bgColor-muted, #f6f8fa); }
.ssf-writes button.primary { color: #ffffff; background: var(--fgColor-success, #1a7f37);
  border-color: transparent; font-weight: 600; }
/* Keeps the white label readable: the plain hover would swap in a pale
   background behind it (#485). */
.ssf-writes button.primary:hover:not(:disabled) {
  background: var(--fgColor-success, #1a7f37); filter: brightness(0.9); }
.ssf-writes button.danger { color: var(--fgColor-danger, #cf222e);
  border-color: var(--borderColor-default, #d1d9e0); }
.ssf-writes button:disabled { opacity: 0.5; cursor: default; }
.ssf-writes-note { margin: 0; color: var(--fgColor-muted, #59636e); }
/* The daemon's own words, with its own line structure: a refused release
   lists what the workspace holds, one check per line. */
.ssf-writes-error { margin: 0; color: var(--fgColor-danger, #cf222e);
  white-space: pre-wrap; overflow-wrap: anywhere; }
.ssf-writes-stack { margin: 0; color: var(--fgColor-default, #1f2328); font-weight: 600; }
/* A scratch row's first line, with Open at its end. */
.ssf-writes-title { display: flex; align-items: center; gap: 6px; }
.ssf-writes a { color: var(--fgColor-accent, #0969da); }
`;

  /// Renders again once a listing has arrived; content.js sets this to its own
  /// scheduler.
  let redraw = () => {};

  /// `owner/name#N` -> state, one per item the form is being drawn for.
  const forms = new Map();

  /// Every node this module draws is a fresh one, and content.js writes a frame
  /// into the nodes already on screen; a node it must not put a different one in
  /// the place of carries `data-ssf-node`, its key, which is what that matching
  /// reads. Only siblings need distinct keys: a picker whose role can shift
  /// among its neighbours is the whole reason this exists -- a `<select>`'s
  /// change listener is drawn with the node, and a node reused for another role
  /// would keep the last frame's listener. The `.ssf-writes` body itself is
  /// keyed by content.js, which draws it into a card.
  function element(tag, className, text, key) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    if (key) node.dataset.ssfNode = key;
    return node;
  }

  function fresh(repo, number) {
    return {
      /// Which of the two nodes this item's state is for: `assign` or
      /// `actions`. An item does not change its mind between frames (an item
      /// with an agent is handed the Actions row), and a state kept for the
      /// other one is replaced rather than reused.
      mode: null,
      repo,
      number,
      itemId: `${repo}#${number}`,
      /// The card's own item object from the latest frame: the stack the
      /// hand-over pickers are prefilled from, and the branch the release
      /// confirm names.
      item: null,
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
      /// The server's words about a refused write; the form stays.
      error: null,
      /// A write is in the air: the asking, before the factory has answered.
      busy: false,
      /// `{at, result, timer, timedOut}` while the item waits for the snapshot
      /// that shows its agent.
      pending: null,
      result: null,
      /// The open step of the Actions row: `handover`, `release`, or null for
      /// the row itself.
      open: null,
      /// A handover or release is in the air.
      actionBusy: false,
      /// What the factory answered a handover or release with.
      actionResult: null,
      /// The note a handover carries.
      note: "",
      /// A scratch create: whose the new session is, `shared` or `mine`.
      whose: "shared",
      /// The signed-in GitHub user, from the page, for `mine`.
      login: null,
      /// The repository's scratch sessions from the latest frame.
      sessions: [],
      /// A kill the checks refused: `{session, message}` until it is confirmed
      /// or cancelled.
      kill: null,
      /// The scratch session a kill or resume is in the air for.
      rowBusy: null,
      /// What the factory said about the last scratch write.
      rowNote: null,
    };
  }

  /// The state for one item, one mode and -- for the Actions row, which is drawn
  /// once per factory that has an agent on the item -- one factory, most recent
  /// last. content.js keeps the nodes it has drawn, but a host is taken off the
  /// page when the item leaves the snapshot and drawn again when it returns, so
  /// this map, and not the nodes, is what outlives a page; a session keeps the
  /// items it has visited and forgets the rest.
  ///
  /// The factory is part of the key because the state is: two rows on one item
  /// write through two factories, and a state they shared would have one row
  /// clearing what the other was in the middle of (a half-written message, an
  /// open confirm step) on every pass. The assign form is keyed by item alone:
  /// one form is drawn per item, and its own Factory picker chooses among the
  /// factories that accept writes.
  function remember(itemKey, mode, factory) {
    const key = `${itemKey}|${mode}|${factory ?? ""}`;
    const held = forms.get(key);
    const [repo, number] = itemKey.split("#");
    const state = held && held.mode === mode ? held : fresh(repo, Number(number));
    state.mode = mode;
    forms.delete(key);
    forms.set(key, state);
    while (forms.size > REMEMBER_ITEMS) forget(forms.keys().next().value);
    return state;
  }

  /// The factories that may take this write for this item: those whose Writes
  /// switch is on. One of them means no factory picker, several mean one.
  function contenders(factories) {
    return (factories ?? [])
      .filter((factory) => factory?.url && factory.writes !== false)
      .map((factory) => ({ url: factory.url, label: factory.label || factory.url }));
  }

  /// Point the form at one factory, forgetting what the last one answered: its
  /// agents and models are its own, and so is whatever a half-finished write
  /// was for.
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
    state.open = null;
    state.actionResult = null;
    state.note = "";
  }

  /// Which factory takes this write, as a picker: drawn only when more than one
  /// accepts writes for the item, since choosing between them is not something
  /// to guess.
  function factoryPicker(state) {
    if (state.choices.length <= 1) return null;
    const values = state.choices.map((choice) => ({ value: choice.url, text: choice.label }));
    return select("Factory", values, state.url, (url) => {
      chooseFactory(state, state.choices, url);
      loadAgents(state);
      redraw();
    });
  }

  /// The factory could not be asked for its agents, so nothing can be offered
  /// for this item: the server's own words about it, and Try again, which is the
  /// only thing that clears it.
  function agentsRefused(state, head) {
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", head, "head"));
    const picker = factoryPicker(state);
    if (picker) body.append(picker);
    body.append(element("p", "ssf-writes-error", state.agentsError, "error"));
    const retry = element("button", undefined, "Try again", "retry");
    retry.type = "button";
    retry.onclick = () => {
      state.agentsError = null;
      loadAgents(state);
      redraw();
    };
    body.append(retry);
    return body;
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
    // A harness that takes no model setting has nothing to list, and the
    // factory answers 400 for one. The picker already offers only its default,
    // so settle here rather than ask on every render -- and settle the pending
    // flag too, or a listing still in the air for the harness the form just
    // left would leave Assign disabled for good.
    const chosen = (state.agents ?? []).find((agent) => agent.id === harness);
    if (chosen && !chosen.takesModel) {
      state.models = [];
      state.modelsFor = harness;
      state.modelsError = null;
      state.modelsPending = false;
      return;
    }
    const url = state.url;
    state.modelsPending = true;
    ask({ type: "ssf:models", url, harness }).then((reply) => {
      // An answer for the factory or the harness the form has since left is
      // not an answer: applying it would offer one harness's model ids under
      // another's name.
      if (state.url !== url || state.harness !== harness) return;
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
  /// harness's own default. A value the card is on but the factory no longer
  /// lists is still what the session runs, so it is offered as itself rather
  /// than dropped -- and the factory's own answer decides whether it takes it.
  ///
  /// The field is keyed by its label, so a picker that appears beside the
  /// others (the Factory picker, the moment a second factory starts accepting
  /// the write) takes its own place rather than one of theirs -- the change
  /// listener it carries is drawn with it.
  function select(label, values, chosen, onPick) {
    const field = element("label", "ssf-writes-field", undefined, `field:${label}`);
    field.append(element("span", undefined, label));
    const box = element("select");
    const options =
      !chosen || values.some((one) => one.value === chosen)
        ? values
        : [...values, { value: chosen, text: chosen }];
    for (const { value, text } of options) {
      const option = element("option", undefined, text);
      option.value = value;
      box.append(option);
    }
    box.value = chosen;
    // The node the person changed, which may be an earlier frame's.
    box.onchange = (event) => onPick(event.currentTarget.value);
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
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", "Assign agent", "head"));

    // Two factories watching this repository: which one takes the session is
    // not something to guess. One: no picker, nothing to choose.
    const picker = factoryPicker(state);
    if (picker) body.append(picker);

    if (state.agentsError) return agentsRefused(state, "Assign agent");

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
    if (state.modelsError) {
      body.append(element("p", "ssf-writes-error", state.modelsError, "error:models"));
    }
    if (state.error) body.append(element("p", "ssf-writes-error", state.error, "error"));

    const actions = element("div", "ssf-writes-actions", undefined, "actions");
    const assign = element("button", "primary", "Assign");
    assign.type = "button";
    assign.disabled = !ready || state.modelsPending;
    assign.onclick = () => submit(state);
    actions.append(assign);
    body.append(actions);
    body.append(
      element(
        "p",
        "ssf-writes-note",
        // An item's own stack is the repository's, and guessing a harness
        // would start a session on one nobody asked for.
        ready
          ? "Starts a session on this item with the factory's own `ssf assign`."
          : "Pick a harness to start a session on this item.",
        "note",
      ),
    );
    return body;
  }

  /// A write in the air, or a wait for the frame that shows its agent.
  function submitting(state) {
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", "Assigning\u2026", "head"));
    const chosen = [state.harness, state.model, state.effort].filter(Boolean).join(" \u00b7 ");
    body.append(
      element("p", "ssf-writes-stack", chosen, "stack"),
      element("p", "ssf-writes-note", "waiting for the factory to show an agent on this item", "note"),
    );
    return body;
  }

  /// The result line: what `ssf assign --json` answered, in the terms its own
  /// text output uses. The three facts worth reading are the session, the stack
  /// it starts on, and whether the item is closed (in which case nothing starts
  /// yet, which is a fact about the item and not a failure of the write).
  function resultLine(state) {
    const result = state.result;
    if (!result || typeof result !== "object") return String(result ?? "");
    const to = result.to ?? {};
    const facts = [];
    if (result.session) facts.push(String(result.session));
    const stack = [to.harness, to.model, to.effort].filter(Boolean).join(" · ");
    if (stack) facts.push(stack);
    if (result.open === false) facts.push("the item is closed, so no session starts yet");
    return facts.join(" \u00b7 ");
  }

  /// What the factory said about a write it accepted but has not shown an
  /// agent for, and where to look.
  function settled(state) {
    const body = element("div", "ssf-writes");
    body.append(
      element("div", "ssf-writes-head", "Assign accepted", "head"),
      element("p", "ssf-writes-stack", resultLine(state), "stack"),
      element("p", "ssf-writes-note", "the factory has not shown an agent on this item yet.", "note"),
    );
    const link = element("a", undefined, "Open the factory dashboard");
    link.href = state.url;
    link.target = "_blank";
    link.rel = "noreferrer";
    const line = element("p", "ssf-writes-note", undefined, "note:link");
    line.append(link);
    body.append(line);
    return body;
  }

  function submit(state) {
    // One write per intent. The button stays live until content.js redraws
    // (its scheduler is debounced), so a double click would otherwise post
    // twice and let the two answers race over the state.
    if (state.busy || state.pending) return;
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

  /// The Actions row for an item that has an agent: Hand over… and Release,
  /// with whichever step is open below them, and what the factory answered the
  /// last one with. `state.item` is the card's own item object, which is what
  /// the pickers are prefilled from and the confirm names.
  ///
  /// Nothing here types at an agent. An agent is spoken to on the item, where
  /// the exchange is part of the item's record and belongs to everyone working
  /// it; a text box on a card would make the overlay a second conversation
  /// nobody else can read (#439).
  function actions(state) {
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", "Actions", "head"));
    const picker = factoryPicker(state);
    if (picker) body.append(picker);
    if (state.actionResult) {
      body.append(
        element(
          "div",
          "ssf-writes-head",
          state.actionResult.kind === "release" ? "Release accepted" : "Handover recorded",
          "result",
        ),
        element("p", "ssf-writes-stack", actionLine(state), "result:stack"),
      );
    }

    if (state.open === "handover") {
      body.append(handoverForm(state));
    } else if (state.open === "release") {
      body.append(releaseConfirm(state));
    } else {
      if (state.error) body.append(element("p", "ssf-writes-error", state.error, "error"));
      const actions = element("div", "ssf-writes-actions", undefined, "actions");
      const hand = element("button", undefined, "Hand over\u2026");
      hand.type = "button";
      hand.onclick = () => openHandover(state);
      const release = element("button", "danger", "Release");
      release.type = "button";
      release.onclick = () => {
        state.open = "release";
        state.error = null;
        redraw();
      };
      actions.append(hand, release);
      body.append(actions);
      // Where a message to this agent goes, said once, now that the box that
      // used to be here is gone: the row is the only place a person meets the
      // question.
      body.append(
        element("p", "ssf-writes-note", "Comment on the item to talk to this agent."),
      );
    }
    return body;
  }


  /// What `ssf handover` or `ssf release` answered, in the terms their own text
  /// output uses: the session, where it moves to or what goes, and that the
  /// daemon acts on the next pass.
  function actionLine(state) {
    const result = state.actionResult?.result;
    if (!result || typeof result !== "object") return String(result ?? "");
    if (state.actionResult.kind === "release") {
      const facts = [result.session].filter(Boolean).map(String);
      if (result.already_gone) facts.push("the workspace was already gone");
      facts.push("the workspace is removed on the daemon's next pass");
      return facts.join(" \u00b7 ");
    }
    const to = result.to ?? {};
    const facts = [];
    if (result.session) facts.push(String(result.session));
    const stack = [to.harness, to.model, to.effort].filter(Boolean).join(" \u00b7 ");
    if (stack) facts.push(`to ${stack}`);
    if (result.summary_chars) facts.push(`with a ${result.summary_chars}-character note`);
    facts.push("the new session starts on the daemon's next pass");
    return facts.join(" \u00b7 ");
  }

  /// Open the hand-over step, prefilled with what the card is on: the person
  /// moves one of the pickers rather than choosing a stack from nothing.
  function openHandover(state) {
    const item = state.item ?? {};
    state.open = "handover";
    state.error = null;
    state.actionResult = null;
    state.harness = String(item.harness ?? "").trim() || DEFAULT;
    state.model = String(item.model ?? "").trim() || DEFAULT;
    state.effort = String(item.effort ?? "").trim() || DEFAULT;
    state.models = null;
    state.modelsFor = null;
    state.modelsError = null;
    if (state.harness) loadModels(state, state.harness);
    redraw();
  }

  /// The pickers, prefilled, plus the note the new session reads first. The
  /// pickers are the assign form's, so a harness that takes no model or no
  /// effort level is offered neither.
  function handoverForm(state) {
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", "Hand over to", "head"));
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
    if (state.modelsError) {
      body.append(element("p", "ssf-writes-error", state.modelsError, "error:models"));
    }
    const note = element("textarea", undefined, undefined, "note");
    note.rows = 2;
    note.placeholder = "What the new session should know (optional)";
    note.value = state.note;
    note.oninput = (event) => {
      state.note = event.currentTarget.value;
    };
    body.append(note);
    if (state.error) body.append(element("p", "ssf-writes-error", state.error, "error"));
    const actions = element("div", "ssf-writes-actions", undefined, "actions");
    const go = element("button", "primary", state.actionBusy ? "Handing over\u2026" : "Hand over");
    go.type = "button";
    go.disabled = !ready || state.modelsPending || state.actionBusy;
    go.onclick = () => sendHandover(state);
    const cancel = element("button", undefined, "Cancel");
    cancel.type = "button";
    cancel.onclick = () => closeStep(state);
    actions.append(go, cancel);
    body.append(actions);
    body.append(
      element(
        "p",
        "ssf-writes-note",
        // The session that is there ends and the new one starts in the same
        // workspace, which is what `ssf handover` says in its own words.
        "Ends this session on the daemon's next pass and starts the new one in the same workspace.",
        "hint",
      ),
    );
    return body;
  }

  /// The release confirm: the branch is named, because the workspace and the
  /// work in it are what goes, and the factory's own checks are what decide. A
  /// refusal leaves it standing, in the factory's words.
  function releaseConfirm(state) {
    const item = state.item ?? {};
    const branch = String(item.branch ?? "").trim();
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", "Release this workspace?", "head"));
    body.append(
      element(
        "p",
        "ssf-writes-note",
        branch
          ? `${state.itemId} works on ${branch}. Nothing is removed unless it is committed and pushed.`
          : `${state.itemId}'s workspace is removed only if it is committed and pushed.`,
        "note",
      ),
    );
    if (state.error) body.append(element("p", "ssf-writes-error", state.error, "error"));
    const actions = element("div", "ssf-writes-actions", undefined, "actions");
    const release = element("button", "danger", state.actionBusy ? "Releasing\u2026" : "Release");
    release.type = "button";
    release.disabled = state.actionBusy;
    release.onclick = () => sendRelease(state);
    const cancel = element("button", undefined, "Cancel");
    cancel.type = "button";
    cancel.onclick = () => closeStep(state);
    actions.append(release, cancel);
    body.append(actions);
    return body;
  }

  function closeStep(state) {
    state.open = null;
    state.error = null;
    redraw();
  }

  /// Hand the item to another stack: one write, no retry, and the refusal keeps
  /// the pickers and the note.
  function sendHandover(state) {
    if (state.actionBusy) return;
    state.error = null;
    state.actionBusy = true;
    redraw();
    ask({
      type: "ssf:handover",
      url: state.url,
      repo: state.repo,
      number: state.number,
      harness: state.harness,
      model: state.model,
      effort: state.effort,
      note: state.note,
    }).then((reply) => {
      state.actionBusy = false;
      if (!reply?.ok) {
        state.error = reply?.error ?? "the factory did not answer";
        redraw();
        return;
      }
      state.actionResult = { kind: "handover", result: reply.result };
      state.note = "";
      state.open = null;
      redraw();
    });
  }

  /// Release the workspace: one write, no retry, and the refusal keeps the
  /// confirm standing so the person can read what the workspace holds.
  function sendRelease(state) {
    if (state.actionBusy) return;
    state.error = null;
    state.actionBusy = true;
    redraw();
    ask({
      type: "ssf:release",
      url: state.url,
      repo: state.repo,
      number: state.number,
    }).then((reply) => {
      state.actionBusy = false;
      if (!reply?.ok) {
        state.error = reply?.error ?? "the factory did not answer";
        redraw();
        return;
      }
      state.actionResult = { kind: "release", result: reply.result };
      state.open = null;
      redraw();
    });
  }

  /// A repository's scratch sessions on one factory: a row each, the kill
  /// confirmation when one is open, and New scratch or its form.
  function scratchPanel(state) {
    const body = element("div", "ssf-writes");
    body.append(element("div", "ssf-writes-head", "Scratch sessions", "head"));
    if (!state.sessions.length) {
      body.append(element("p", "ssf-writes-note", "No scratch sessions on this repository.", "none"));
    }
    for (const one of state.sessions) body.append(scratchRow(state, one));
    if (state.rowNote) body.append(element("p", "ssf-writes-note", state.rowNote.text, "result"));
    if (state.open === "new") {
      body.append(scratchForm(state));
      return body;
    }
    if (state.error) body.append(element("p", "ssf-writes-error", state.error, "error"));
    const actions = element("div", "ssf-writes-actions", undefined, "actions");
    const create = element("button", "primary", "New scratch");
    create.type = "button";
    create.onclick = () => {
      state.open = "new";
      state.error = null;
      state.rowNote = null;
      loadAgents(state);
      redraw();
    };
    actions.append(create);
    body.append(actions);
    return body;
  }

  /// One scratch session: its id, whose it is, its stack and state, and what
  /// can be done to it -- Open (at the end of its first line) and Kill while it
  /// has a workspace, Resume once it is killed.
  function scratchRow(state, one) {
    const row = element("div", "ssf-writes", undefined, `row:${one.id}`);
    const id = String(one.id ?? "");
    const short = id.slice(id.lastIndexOf("~"));
    const whose = one.owner_login ? `@${one.owner_login}` : "shared";
    const stack = [one.harness, one.model, one.effort].filter(Boolean).join(" \u00b7 ");
    const title = element("p", "ssf-writes-stack ssf-writes-title", undefined, "id");
    title.append(`${short} \u00b7 ${whose}`);
    const open = one.active && globalThis.ssfPane?.button(state.url, id, one.pane_input === true);
    if (open) title.append(open);
    row.append(
      title,
      element("p", "ssf-writes-note", [stack, one.stateLabel].filter(Boolean).join(" \u00b7 "), "state"),
    );
    const busy = state.rowBusy === id;
    if (state.kill?.session === id) {
      row.append(
        element("p", "ssf-writes-error", state.kill.message, "check"),
        element(
          "p",
          "ssf-writes-error",
          "All work in this workspace will be lost. The conversation is kept, so Resume can bring the session back on a fresh workspace.",
          "lost",
        ),
      );
      const actions = element("div", "ssf-writes-actions", undefined, "actions");
      const force = element("button", "danger", busy ? "Killing\u2026" : "Kill anyway");
      force.type = "button";
      force.disabled = busy;
      force.onclick = () => sendKill(state, id, true);
      const cancel = element("button", undefined, "Cancel");
      cancel.type = "button";
      cancel.onclick = () => {
        state.kill = null;
        redraw();
      };
      actions.append(force, cancel);
      row.append(actions);
      return row;
    }
    const actions = element("div", "ssf-writes-actions", undefined, "actions");
    if (one.active) {
      const kill = element("button", "danger", busy ? "Killing\u2026" : "Kill");
      kill.type = "button";
      kill.disabled = busy;
      kill.onclick = () => sendKill(state, id, false);
      actions.append(kill);
    } else {
      const resume = element("button", undefined, busy ? "Resuming\u2026" : "Resume");
      resume.type = "button";
      resume.disabled = busy;
      resume.onclick = () => sendResume(state, id);
      actions.append(resume);
    }
    row.append(actions);
    return row;
  }

  /// New scratch: the assign pickers, and whose the session is -- shared by the
  /// repository, or the signed-in GitHub user's.
  function scratchForm(state) {
    const body = element("div", "ssf-writes", undefined, "new");
    body.append(element("div", "ssf-writes-head", "New scratch", "head"));
    if (state.agentsError) return agentsRefused(state, "New scratch");
    const { harnesses, models, efforts, ready } = stack(state);
    const owners = [{ value: "shared", text: "Shared" }];
    if (state.login) owners.push({ value: "mine", text: `Mine (@${state.login})` });
    if (state.whose === "mine" && !state.login) state.whose = "shared";
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
      select("Whose", owners, state.whose, (value) => {
        state.whose = value;
      }),
    );
    if (state.modelsError) {
      body.append(element("p", "ssf-writes-error", state.modelsError, "error:models"));
    }
    if (state.error) body.append(element("p", "ssf-writes-error", state.error, "error"));
    const actions = element("div", "ssf-writes-actions", undefined, "actions");
    const go = element("button", "primary", state.busy ? "Starting\u2026" : "Start");
    go.type = "button";
    go.disabled = !ready || state.modelsPending || state.busy;
    go.onclick = () => sendScratch(state);
    const cancel = element("button", undefined, "Cancel");
    cancel.type = "button";
    cancel.onclick = () => closeStep(state);
    actions.append(go, cancel);
    body.append(actions);
    body.append(
      element(
        "p",
        "ssf-writes-note",
        "Starts an agent on this repository in a workspace of its own, working on no item, until it is killed.",
        "hint",
      ),
    );
    return body;
  }

  function sendScratch(state) {
    if (state.busy) return;
    state.error = null;
    state.busy = true;
    redraw();
    ask({
      type: "ssf:scratch",
      url: state.url,
      repo: state.repo,
      harness: state.harness,
      model: state.model,
      effort: state.effort,
      for: state.whose === "mine" ? state.login : undefined,
    }).then((reply) => {
      state.busy = false;
      if (!reply?.ok) {
        state.error = reply?.error ?? "the factory did not answer";
        redraw();
        return;
      }
      state.open = null;
      const session = reply.result?.session ?? "the session";
      state.rowNote = rowNote(
        state,
        `Started ${session}; it shows here once the factory reports it.`,
        session,
        (row) => Boolean(row),
      );
      redraw();
    });
  }

  /// Kill a scratch session. The first request is never forced: a clean
  /// workspace goes, and one the checks refuse opens the confirmation, whose
  /// Kill anyway is the only request that sends `force`.
  function sendKill(state, session, force) {
    if (state.rowBusy) return;
    state.rowBusy = session;
    state.rowNote = null;
    state.error = null;
    redraw();
    ask({ type: "ssf:scratch-release", url: state.url, session, force }).then((reply) => {
      state.rowBusy = null;
      if (reply?.ok) {
        state.kill = null;
        state.rowNote = rowNote(
          state,
          `Killed ${session}; the workspace is removed on the daemon's next pass.`,
          session,
          (row) => !row?.active,
        );
      } else if (!force && reply?.body?.check) {
        state.kill = { session, message: reply.error };
      } else {
        state.kill = null;
        state.rowNote = rowNote(state, reply?.error ?? "the factory did not answer", session);
      }
      redraw();
    });
  }

  /// A note under the scratch rows about `session`, standing until a frame
  /// supersedes it: `settled(row)` says the factory now shows what the note
  /// was waiting for. A note without one (an error) goes once the session's
  /// row says something else than when the note was written.
  function rowNote(state, text, session, settled) {
    const label = (row) => (row ? `${row.stateLabel}|${row.active}|${row.released_at}` : "none");
    const then = label(state.sessions.find((one) => one.id === session));
    return { text, session, settled: settled ?? ((row) => label(row) !== then) };
  }

  function sendResume(state, session) {
    if (state.rowBusy) return;
    state.rowBusy = session;
    state.rowNote = null;
    redraw();
    ask({ type: "ssf:scratch-resume", url: state.url, session }).then((reply) => {
      state.rowBusy = null;
      state.rowNote = reply?.ok
        ? rowNote(state, `Resuming ${session} on the daemon's next pass.`, session, (row) =>
            Boolean(row?.active),
          )
        : rowNote(state, reply?.error ?? "the factory did not answer", session);
      redraw();
    });
  }

  /// Stop remembering an item: its waits, if any are in flight, are the
  /// module's only promise that anything still cares, and the timers go with
  /// it.
  function forget(key) {
    const state = forms.get(key);
    if (!state) return;
    if (state.pending) clearTimeout(state.pending.timer);
    forms.delete(key);
  }

  /// The Assign agent form for one item, or `null` when nothing may be drawn
  /// for it: no factory that watches this item accepts writes. One form per
  /// item; its own Factory picker chooses among the factories that accept the
  /// write.
  function render({ factories, repo, number }) {
    const choices = contenders(factories);
    if (!choices.length) return null;
    const state = remember(`${repo}#${number}`, "assign");
    chooseFactory(
      state,
      choices,
      choices.some((one) => one.url === state.url) ? state.url : choices[0].url,
    );
    if (!state.pending && !state.busy) loadAgents(state);
    if (state.harness && !state.models && !state.modelsPending && !state.modelsError) {
      loadModels(state, state.harness);
    }
    if (state.busy) return submitting(state);
    if (state.pending) return state.pending.timedOut ? settled(state) : submitting(state);
    return form(state);
  }

  /// The Actions row for one item whose state is Working, Waiting on you, Done
  /// or Problem, or `null` when the factory it is drawn for does not accept
  /// writes. `item` is the card's own item object: the stack the hand-over
  /// pickers are prefilled from, and the branch the release confirm names.
  ///
  /// `factories` is the one factory whose card this row is on -- the factory
  /// that has the agent -- so the row needs no Factory picker and its state is
  /// its own, keyed by that factory: a second factory's row on the same item is
  /// a second session to act on, not the same one under another name.
  function renderActions({ factories, repo, number, item }) {
    const choices = contenders(factories);
    if (!choices.length) return null;
    const state = remember(`${repo}#${number}`, "actions", choices[0].url);
    state.item = item ?? null;
    chooseFactory(state, choices, choices[0].url);
    if (state.agentsError) return agentsRefused(state, "Actions");
    loadAgents(state);
    // The pickers are only drawn once a step is open, so the listing they need
    // is asked for when it opens rather than on every frame.
    return actions(state);
  }

  /// A repository's scratch sessions on one factory, or `null` when that
  /// factory does not accept writes. `sessions` are the factory's scratch
  /// rows for the repository, each with the `stateLabel` the page drew for it;
  /// `login` is the signed-in GitHub user, which is what `mine` means.
  function renderScratch({ factories, repo, login, sessions }) {
    const choices = contenders(factories);
    if (!choices.length) return null;
    const state = remember(`${repo}#0`, "scratch", choices[0].url);
    chooseFactory(state, choices, choices[0].url);
    state.login = login || null;
    state.sessions = sessions ?? [];
    const noted = state.rowNote;
    if (noted && noted.settled(state.sessions.find((one) => one.id === noted.session))) {
      state.rowNote = null;
    }
    if (state.harness && !state.models && !state.modelsPending && !state.modelsError) {
      loadModels(state, state.harness);
    }
    return scratchPanel(state);
  }

  /// Every snapshot, so a waiting form can see the item gain its agent, and so
  /// a sent message can see the item's last message change. That frame, not the
  /// HTTP response, is what ends "Assigning…": the response only says the
  /// factory accepted the write.
  ///
  /// The card that frame carries is what the item shows from then on, so the
  /// wait, its result and its timer all go; nothing of the form is left to
  /// draw over it.
  function applySnapshot(payload) {
    let changed = false;
    for (const state of forms.values()) {
      const factory = (payload?.factories ?? []).find((one) => one.url === state.url);
      const card = (factory?.cards ?? []).find(
        (one) =>
          one.origin?.id === state.itemId ||
          (one.additional ?? []).some((issue) => issue?.id === state.itemId),
      );
      // The card this frame carries is what the item is now: what a hand-over
      // is prefilled from and what the release confirm names.
      if (card && state.mode === "actions") state.item = card;
      if (!state.pending) continue;
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

  globalThis.ssfWrites = {
    STYLE,
    render,
    renderActions,
    renderScratch,
    applySnapshot,
    onChange(callback) {
      redraw = callback;
    },
  };
})();

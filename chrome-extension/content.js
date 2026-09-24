// Overlays live ssf state on github.com.
//
// It renders the factory's own dashboard model (`cards` and `monitored_items`
// from the server's canonical status, see src/status.rs). Every screen answers
// one question -- is an agent on this, and does it need me? -- in one glance,
// and keeps the detail one click away:
//
//   * an issue or pull request page, and the item a project board's side panel
//     is showing, get a card at the top of the right sidebar, above Assignees;
//   * issue and pull request lists, search results and project boards get one
//     chip per tracked item, and a click on a chip opens the same card in a
//     popover;
//   * a repository's home page gets its scratch sessions (#414) at the top of
//     the right sidebar, with New scratch, for each factory that watches it;
//   * every other page gets nothing.
//
// The five user-facing states are fixed in colour and icon so they read the
// same everywhere; the word ssf and the harness actually use is in every
// tooltip, and a stale or unreachable factory is a modifier on top rather than
// a state of its own, so a stale snapshot can never be mistaken for a live one.
//
// The one thing it does to a factory is what an item's card offers: the Assign
// agent form for an item with no agent, and the Actions row -- hand over and
// release -- for one that has, with Open at the top of its card, which shows the
// agent's own terminal over the page (pane-overlay.js). Nothing on a card types
// at an agent: a person speaks to an item's agent by commenting on the item,
// where the exchange stays. Every write is the service worker's, never this
// script's, and none of them is retried. Nothing factory-written is ever parsed
// as HTML: every node is built with textContent.
(() => {
  if (window.__ssfOverlayInstalled) return;
  window.__ssfOverlayInstalled = true;

  /// An issue or pull request page, including its subpages (`/pull/5/files`).
  const DETAIL_PATH = /^\/([^/]+)\/([^/]+)\/(issues|pull)\/(\d+)(?:\/|$)/;
  /// A repository's home page: where its scratch sessions are listed.
  const REPO_PATH = /^\/([^/]+)\/([^/]+)\/?$/;
  /// A link to exactly one issue or pull request, as lists and boards make them.
  const LINK_PATH = /^\/([^/]+)\/([^/]+)\/(?:issues|pull)\/(\d+)\/?$/;
  /// The two shapes a project board has. A board carries a card for an item
  /// the factory watches but has no record of -- that is where an item is
  /// picked up -- where the lists and search results carry a chip only for an
  /// item that already moves, since a watched repository's whole backlog as a
  /// column of grey chips is not what those pages are read for.
  const BOARD_PATHS = [
    /^\/(?:users|orgs)\/[^/]+\/projects\/\d+(?:\/|$)/,
    /^\/[^/]+\/[^/]+\/projects\/\d+(?:\/|$)/,
  ];
  /// The only pages besides a detail page that carry a chip: the issue and pull
  /// request lists, search results, and project boards. Anywhere else gets
  /// nothing, however many issue links the page happens to contain.
  const CHIP_PATHS = [
    /^\/(?:issues|pulls)(?:\/|$)/,
    /^\/[^/]+\/[^/]+\/(?:issues|pulls)(?:\/|$)/,
    /^\/search\/?$/,
    ...BOARD_PATHS,
  ];
  const PING_MS = 20000;
  const RENDER_DEBOUNCE_MS = 200;
  const POPOVER_WIDTH = 320;

  const SVG_NS = "http://www.w3.org/2000/svg";

  const STYLE = `
/* One surface language for the sidebar card and the popover: the same frame,
   the same radii, the same 1px rules GitHub draws its own boxes with. A card's
   state colour is its band (#497's option 1a) and nothing else: the body under
   it, the chips and the message stay neutral, so a state cannot leak into the
   text beside it. A chip carries it in its icon and word alone. */
:host { display: block; }
:host([data-ssf-slot="sidebar"]) { margin-bottom: 16px; }
:host([data-ssf-slot="sidebar-legacy"]) {
  margin-bottom: 16px; padding-top: 16px;
  border-top: 1px solid var(--borderColor-muted, #d1d9e0); }
:host([data-ssf-list]) { display: inline-flex; vertical-align: middle; }
:host([data-ssf-popover]) { position: fixed; z-index: 2147483000; }
.ssf-section, .ssf-popover { font-family: -apple-system, BlinkMacSystemFont,
  "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif; font-size: 12px;
  line-height: 1.5; text-align: left; color: var(--fgColor-default, #1f2328); }
/* The header names the thing the way GitHub names a sidebar section, so the
   card reads as part of the page rather than as something laid over it. */
.ssf-title { margin: 0 0 8px; font-size: 12px; font-weight: 600;
  color: var(--fgColor-muted, #59636e); }
.ssf-popover { box-sizing: border-box; width: min(${POPOVER_WIDTH}px, calc(100vw - 16px));
  padding: 10px 12px; border-radius: 8px;
  /* A card's message can be thousands of characters, and expanding it must not
     grow the popover past the viewport, or its own less toggle and Details
     would be out of reach. Border-box, so this is the whole box and
     placePopover's clamp always finds room for it. */
  max-height: calc(100vh - 16px); overflow: auto;
  border: 1px solid var(--borderColor-default, #d1d9e0);
  background: var(--bgColor-default, #ffffff);
  box-shadow: 0 8px 24px rgba(31, 35, 40, 0.2); }
/* No overflow clip: the ••• menu opens past the card's edge. The band and the
   Details bar round their own corners instead. */
.ssf-card { border-radius: 6px;
  border: 1px solid var(--borderColor-default, #d1d9e0);
  background: var(--bgColor-default, #ffffff); }
/* The band: the state, tinted by it -- green working, yellow waiting on you,
   grey done or no agent, red a problem -- with the time and SSF beside it. */
.ssf-band { padding: 8px 12px; font-size: 13px; border-radius: 5px 5px 0 0;
  border-bottom: 1px solid var(--ssf-band-border); color: var(--ssf-band-fg);
  background: var(--ssf-band-bg); }
.ssf-band .ssf-word, .ssf-band .ssf-when, .ssf-band .ssf-icon { color: inherit; }
.ssf-band .ssf-when { font-size: 12px; opacity: 0.8; }
.ssf-band .ssf-when[data-stale="true"] { opacity: 1; }
.ssf-brand { margin-left: auto; font-size: 11px; font-weight: 600;
  letter-spacing: 0.04em; }
[data-ssf-band="working"] {
  --ssf-band-bg: var(--bgColor-success-muted, #dafbe1);
  --ssf-band-border: var(--borderColor-success-muted, #aceebb);
  --ssf-band-fg: var(--fgColor-success, #116329); }
[data-ssf-band="waiting"] {
  --ssf-band-bg: var(--bgColor-attention-muted, #fff8c5);
  --ssf-band-border: var(--borderColor-attention-muted, #eac54f66);
  --ssf-band-fg: var(--fgColor-attention, #7d4e00); }
[data-ssf-band="done"], [data-ssf-band="no-agent"] {
  --ssf-band-bg: var(--bgColor-muted, #f6f8fa);
  --ssf-band-border: var(--borderColor-muted, #d1d9e0);
  --ssf-band-fg: var(--fgColor-muted, #59636e); }
[data-ssf-band="problem"] {
  --ssf-band-bg: var(--bgColor-danger-muted, #ffebe9);
  --ssf-band-border: var(--borderColor-danger-muted, #ffcecb);
  --ssf-band-fg: var(--fgColor-danger, #d1242f); }
/* A live working agent's icon breathes; a stale snapshot's never does. */
[data-ssf-band="working"]:not([data-stale]) .ssf-icon {
  animation: ssf-pulse 2s infinite; }
@keyframes ssf-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.35; } }
@media (prefers-reduced-motion: reduce) {
  [data-ssf-band] .ssf-icon { animation: none; } }
.ssf-body { display: flex; flex-direction: column; gap: 8px; min-width: 0;
  padding: 12px; }
.ssf-body > .ssf-via { margin: 0; }
/* One long value, one line: the whole of it is its tooltip. */
.ssf-clip { min-width: 0; overflow: hidden; text-overflow: ellipsis;
  white-space: nowrap; }
.ssf-branch { display: flex; align-items: center; gap: 6px; min-width: 0;
  color: var(--fgColor-muted, #59636e);
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas,
  "Liberation Mono", monospace; }
.ssf-branch-mark { flex: none; }
.ssf-branch .ssf-clip { color: var(--fgColor-default, #1f2328); }
.ssf-where { color: var(--fgColor-muted, #59636e); }
.ssf-where strong { font-weight: 600; color: var(--fgColor-default, #1f2328); }
.ssf-card + .ssf-card { margin-top: 8px; }
/* The popover scrolls its own box, which would clip the ••• menu opening
   below the row; there it opens upward, over the card's facts. */
.ssf-popover .ssf-writes-menu-list { top: auto; bottom: calc(100% + 4px); }
/* The lead line of a card: the state, at a size it can be read at, with the
   time beside it in the muted colour GitHub uses for a fact about a thing. */
.ssf-state { display: flex; align-items: center; gap: 6px; min-width: 0; }
.ssf-word { font-weight: 600; }
.ssf-when { font-weight: 400; color: var(--fgColor-muted, #59636e); }
.ssf-when[data-stale="true"] { font-weight: 600; color: #9a6700; }
.ssf-sep { opacity: 0.5; }
/* Which item a card is about, and whose agent it is: the small print above
   the state, never competing with it. */
.ssf-via { margin-bottom: 3px; color: var(--fgColor-muted, #59636e); }
/* The stack -- harness, model, effort -- as chips, in the monospace GitHub
   uses for something a person might copy, since that is what it is. */
.ssf-stack { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; }
.ssf-tag { padding: 1px 6px; border-radius: 4px; overflow-wrap: anywhere;
  border: 1px solid var(--borderColor-default, #d1d9e0);
  background: var(--bgColor-muted, #f6f8fa);
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas,
  "Liberation Mono", monospace; }
.ssf-tag[data-effort] { background: var(--bgColor-attention-muted, #fff8c5);
  border-color: var(--borderColor-attention-emphasis, #eac54f); }
.ssf-next { color: var(--fgColor-muted, #59636e); font-size: 11px; }
.ssf-message { overflow-wrap: anywhere; white-space: pre-wrap; }
.ssf-message[data-clamped="true"] { display: -webkit-box; -webkit-box-orient: vertical;
  -webkit-line-clamp: 2; overflow: hidden; white-space: normal; }
.ssf-more { margin: 2px 0 0; padding: 0; border: 0; background: none;
  font: inherit; font-weight: 600; color: var(--fgColor-accent, #0969da);
  cursor: pointer; }
.ssf-also { color: var(--fgColor-muted, #59636e); }
.ssf-also a { color: var(--fgColor-accent, #0969da); text-decoration: none; }
.ssf-also a:hover { text-decoration: underline; }
/* Where the Assign agent form would be drawn, for an item the write would be
   refused for: the same rule above the text as the form itself carries. */
.ssf-hold { margin: 0; padding-top: 6px;
  border-top: 1px solid var(--borderColor-muted, #d1d9e0); }
/* Details: a term/definition list with its own label column, so a long
   workspace path wraps under its own value instead of pushing every term out
   of line. The disclosure marker is GitHub's own triangle. */
.ssf-details { border-top: 1px solid var(--borderColor-muted, #d1d9e0); }
.ssf-details summary { padding: 6px 12px; color: var(--fgColor-muted, #59636e);
  background: var(--bgColor-muted, #f6f8fa); cursor: pointer; }
.ssf-details:not([open]) summary { border-radius: 0 0 5px 5px; }
.ssf-details summary:hover { color: var(--fgColor-default, #1f2328); }
.ssf-details dl { display: grid; grid-template-columns: minmax(4.5em, auto) minmax(0, 1fr);
  gap: 4px 8px; margin: 0; padding: 8px 12px; }
.ssf-details dt { color: var(--fgColor-muted, #59636e); }
/* A value is one line, whole in its tooltip; a path keeps its tail, which is
   the part that tells two workspaces apart. */
.ssf-details dd { margin: 0; min-width: 0; overflow: hidden;
  text-overflow: ellipsis; white-space: nowrap; }
.ssf-details dd[data-path] { direction: rtl; text-align: left;
  font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas,
  "Liberation Mono", monospace; }
/* The chip: a compact pill that sits on GitHub's own row baseline. Its height
   is the line box it lands in, so a list of titles does not grow a pixel per
   tracked item. */
.ssf-chip { display: inline-flex; align-items: center; gap: 4px; margin-left: 6px;
  padding: 0 7px; border: 1px solid var(--borderColor-default, #d1d9e0);
  border-radius: 999px; font-family: inherit; font-size: 11px; line-height: 18px;
  font-weight: 500; color: var(--fgColor-muted, #59636e); vertical-align: middle;
  white-space: nowrap; cursor: pointer; }
.ssf-chip:hover { background: var(--bgColor-muted, #f6f8fa);
  border-color: var(--borderColor-muted, #d1d9e0); }
.ssf-chip:focus-visible { outline: 2px solid var(--fgColor-accent, #0969da);
  outline-offset: 1px; }
.ssf-icon { flex: none; }
/* One fixed colour per user-facing state, on every screen. Only the word and
   the icon carry it: the attribute never sits on a container, so a state
   colour cannot leak into the message or the stack beside it. */
[data-ssf-tone="working"] { color: #1a7f37; }
[data-ssf-tone="waiting"] { color: #9a6700; }
[data-ssf-tone="done"] { color: #57606a; }
[data-ssf-tone="problem"] { color: #cf222e; }
[data-ssf-tone="no-agent"] { color: #8c959f; }
`;

  const sheet = new CSSStyleSheet();
  // The form's own rules live with it, and Open's with it, so the parts of the
  // overlay cannot drift apart.
  sheet.replaceSync(
    STYLE + (globalThis.ssfWrites?.STYLE ?? "") + (globalThis.ssfPane?.STYLE ?? ""),
  );

  /// The last merged snapshot from the service worker, or null before the
  /// worker has answered. Nothing is rendered from a null snapshot.
  let snapshot = null;
  let port = null;
  let rendering = false;
  let renderTimer = null;
  let pingTimer = null;
  /// name -> {host, shadow}, the nodes this script currently has in the page.
  const injected = new Map();
  /// name -> {more, details}, so a stream that repaints every couple of seconds
  /// cannot collapse what the reader has just opened.
  const opened = new Map();
  /// name -> whether the last layout pass found the message clipped, which is
  /// when the `more` toggle belongs on screen. A frame draws the toggle from
  /// this, so a frame that restates the message does not hide the toggle and
  /// have the next pass show it again -- the toggle a frame draws and the
  /// toggle the page has stay one and the same node, saying the same thing.
  const overflowed = new Map();
  /// The open popover, or null: `{name, key, anchor, entry}`.
  let popover = null;

  /// The five user-facing states agreed on #408: every raw word ssf and the
  /// harness use maps onto one of them, each with its own colour and icon here
  /// and in every chip, so the same state reads the same everywhere. The raw
  /// word is never replaced -- it is what the tooltip shows, and it is the word
  /// the TUI prints.
  const PRESENTATION = {
    working: { label: "Working", kind: "working" },
    idle: { label: "Waiting on you", kind: "waiting" },
    blocked: { label: "Waiting on you", kind: "waiting" },
    done: { label: "Done", kind: "done" },
    "no-agent": { label: "Problem", kind: "problem" },
    "no-workspace": { label: "Problem", kind: "problem" },
    unknown: { label: "Problem", kind: "problem" },
    unbound: { label: "No agent", kind: "no-agent" },
  };
  /// A word this version does not know is a problem, not a healthy agent; the
  /// tooltip still carries the word itself.
  const PROBLEM = { label: "Problem", kind: "problem" };

  /// An item in a repository a factory watches that the factory has no record
  /// of: no card, and not in its monitored list. ssf has said nothing about it
  /// -- there is no session and no record to report one from -- so the reading
  /// is the overlay's own and the tooltip says where it comes from rather than
  /// putting a word ssf never said behind it. It reads as No agent because that
  /// is the state `ssf assign` starts a session on, which is the whole of what
  /// such an item offers.
  const NO_RECORD = "no record";
  const NO_AGENT = { label: "No agent", kind: "no-agent" };

  /// The state icons GitHub draws for an item, and which of them mean there is
  /// nothing left to run.
  const ITEM_ICONS = [
    "svg.octicon-issue-opened",
    "svg.octicon-issue-closed",
    "svg.octicon-git-pull-request",
    "svg.octicon-git-pull-request-closed",
    "svg.octicon-git-pull-request-draft",
    "svg.octicon-git-merge",
    "svg.octicon-check-circle",
  ];
  const FINISHED_ICONS = [
    "svg.octicon-issue-closed",
    "svg.octicon-git-pull-request-closed",
    "svg.octicon-git-merge",
    "svg.octicon-check-circle",
  ];
  /// Every mark GitHub has been seen to draw for an item's own state: the state
  /// container the issue list carries (which reads "Status: Closed
  /// (completed)."), the tooltip wrapper the pull request list uses ("Merged
  /// Pull Request"), and the state icon itself, which is all the search results
  /// carry.
  const STATE_MARKS = [
    '[data-testid="list-row-state-icon"]',
    ".tooltipped[aria-label]",
    ".tooltipped[title]",
    ...ITEM_ICONS,
  ].join(", ");
  /// The state words a mark names, and the word that marks a *linked* item
  /// instead ("1 linked issue", "1 linked PR").
  const STATE_WORD = /^(?:status:\s*)?(open|closed|merged|draft)\b/i;
  const LINKED_WORD = /\blinked\b/i;

  /// A pull request's body names the issue it delivers, and that issue's card is
  /// what a PR page shows. Closing keywords win over `Refs`, and the first match
  /// of each kind wins, per the layout decided on #408.
  const CLOSING_REF = /\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s+#(\d+)/gi;
  const REFS_REF = /\b(?:refs?|references?)\s*:?\s+#(\d+)/gi;

  /// `/issues/new` and `/owner/repo/issues/new`, with their `choose` subpage,
  /// live under a chip path but are forms rather than lists of items. Matched by
  /// the path's tail, so an owner actually named `issues` or a repository named
  /// `new` is not caught by it.
  function newItemForm(path) {
    const parts = path.split("/").filter(Boolean);
    const section = (index) =>
      parts.at(index) === "issues" || parts.at(index) === "pulls";
    return (
      (section(-2) && parts.at(-1) === "new") ||
      (section(-3) && parts.at(-2) === "new" && parts.at(-1) === "choose")
    );
  }

  /// The pages that carry chips: the issue and pull request lists, search
  /// results, and project boards. Anywhere else gets nothing, however many issue
  /// links the page happens to contain.
  function chipPage(path) {
    return !newItemForm(path) && CHIP_PATHS.some((pattern) => pattern.test(path));
  }

  /// Whether this page is a project board, the one place a chip stands for an
  /// item the factory has no record of.
  function boardPage(path) {
    return BOARD_PATHS.some((pattern) => pattern.test(path));
  }

  /// The states the overlay reads as No agent: ssf holds the item and nothing
  /// is running on it. `unbound` is an item ssf knows without a session, and a
  /// monitored item is one whose agent has gone. Both are `ssf assign`'s to
  /// start; every other state is either an agent at work or a session
  /// `ssf handover` moves.
  const NO_AGENT_STATES = new Set(["unbound", "no-agent"]);

  /// Whether the state the overlay draws for `match` is its own No agent, which
  /// is the only state a session can be started on. `assignable` is the
  /// overlay's own reading of an item the factory has no record of, which is
  /// the same nothing-running case read from the other side.
  function noAgent(match) {
    return (
      match.kind === "monitored" ||
      match.kind === "assignable" ||
      NO_AGENT_STATES.has(String(match.item.agent_state ?? "").trim())
    );
  }

  /// An item with no agent that ssf already has a workspace for. `ssf assign`
  /// refuses one -- `ssf release` is what frees it -- and a monitored item is
  /// where the overlay used to meet that refusal: monitored entries carried no
  /// workspace fact, so the form was drawn and the write came back 409 (#421,
  /// #428). An item bound to another session's workspace carries its id, so
  /// the record's own `worktree_id` answers for that case too.
  function held(match) {
    return noAgent(match) && match.item.has_workspace === true;
  }

  /// Whether the Assign agent form belongs on `match`: its own No agent, with
  /// nothing in the way.
  function assignable(match) {
    return noAgent(match) && !held(match);
  }

  /// The state a mark names, where GitHub names one: its own `aria-label`,
  /// `title` or text (the issue list's state container reads "Status: Closed
  /// (completed)."), or the nearest ancestor's label (the older rows wrap the
  /// icon in the words, and `1 linked issue` names a linked item's own mark).
  /// `"linked"` when the mark is about a linked item rather than this row's own,
  /// and `null` where nothing names one at all -- which is every search result.
  function namedState(mark) {
    const own = [
      mark.getAttribute?.("aria-label"),
      mark.getAttribute?.("title"),
      mark.textContent,
    ]
      .filter(Boolean)
      .join(" ")
      .trim();
    if (LINKED_WORD.test(own)) return "linked";
    const word = STATE_WORD.exec(own);
    if (word) return word[1].toLowerCase();
    let node = mark.parentElement;
    for (let depth = 0; node && depth < 4; depth += 1, node = node.parentElement) {
      const name = (node.getAttribute("aria-label") ?? node.getAttribute("title") ?? "").trim();
      if (!name) continue;
      if (LINKED_WORD.test(name)) return "linked";
      const named = STATE_WORD.exec(name);
      if (named) return named[1].toLowerCase();
    }
    return null;
  }

  /// Whether a state mark says the item is finished: by the state it names,
  /// where the row names one, and by the icon it is where it does not.
  function finishedMark(mark) {
    const state = namedState(mark);
    if (state) return state === "closed" || state === "merged";
    return FINISHED_ICONS.some((icon) => mark.matches(icon));
  }

  /// The state marks inside `root`, outermost first: the marks that stand for
  /// an item's own state. A mark inside one already kept is the same item's
  /// state drawn twice (the issue list's state container and the icon in it),
  /// and is counted once; a linked item's mark is a reference to another item
  /// and a comment count is not a state, so neither is one.
  function stateMarks(root) {
    const marks = [];
    for (const mark of root.querySelectorAll(STATE_MARKS)) {
      if (marks.some((kept) => kept.contains(mark))) continue;
      const state = namedState(mark);
      if (state === "linked") continue;
      if (!state && !ITEM_ICONS.some((icon) => mark.matches(icon))) continue;
      marks.push(mark);
    }
    return marks;
  }

  /// Whether `root` holds a link to an item other than `key`: the boundary of
  /// the item's own area, past which any state mark belongs to something else.
  /// A card's own cross-reference badge is a link to another item too, and a
  /// card rendering one above its state mark keeps the factory's report rather
  /// than losing it to the badge -- the reading then falls back to a red
  /// Problem, never to a claim that a live item is done.
  function holdsOtherItem(root, key) {
    for (const link of root.querySelectorAll("a[href]")) {
      const match = LINK_PATH.exec(link.pathname);
      if (match && `${match[1]}/${match[2]}#${match[3]}` !== key) return true;
    }
    return false;
  }

  /// Whether an item's own page -- or the side panel showing it -- says the
  /// item is closed or merged, read from the state label GitHub draws in the
  /// item's header. A page whose header this version cannot read keeps the
  /// state as the factory reported it (#462).
  function pageClosed() {
    const mark = document.querySelector(
      '[data-testid="header-state"], [class*="PageHeader"] [data-component="StateLabel"], .gh-header-show .State, .gh-header-sticky .State',
    );
    return mark ? finishedMark(mark) : false;
  }

  /// Whether the page shows the item `key` as closed or merged, read from the
  /// state mark GitHub draws in the same row, card or search result as the
  /// link. ssf's own `github_state` cannot answer it: it is the item's state as
  /// of the last poll, and it is missing altogether for items bound before ssf
  /// recorded it (#409).
  ///
  /// The row is the nearest ancestor holding exactly one item's state mark,
  /// inside the item's own area: a link to another item is where that area
  /// ends, so a row whose state mark this version cannot read -- a shape it
  /// does not know, or more than one mark -- leaves the state as the factory
  /// reported it, rather than reading a neighbouring item's mark, which would
  /// be a claim that a live item is done.
  function closedInDom(anchor, key) {
    let node = anchor;
    while (node?.parentElement) {
      node = node.parentElement;
      if (key && holdsOtherItem(node, key)) return false;
      const marks = stateMarks(node);
      if (marks.length === 1) return finishedMark(marks[0]);
      if (marks.length > 1) return false;
    }
    return false;
  }

  function element(tag, className, text) {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  }

  /// A host element in the page with an isolated shadow tree, so GitHub's
  /// stylesheet and ours cannot reach each other. Custom properties still
  /// inherit, so GitHub's own theme drives everything that is not a state
  /// colour.
  function shadowHost(attribute, value) {
    const host = element("span");
    host.setAttribute(attribute, value);
    const shadow = host.attachShadow({ mode: "open" });
    shadow.adoptedStyleSheets = [sheet];
    return { host, shadow };
  }

  /// Mark a node as one identity, so the next frame can put what it draws where
  /// this node is instead of drawing it again: a card is its factory's, and the
  /// node a person is using -- a picker, the note they are writing in -- is its
  /// item's, whatever else moved around it.
  function named(node, key) {
    node.dataset.ssfNode = key;
    return node;
  }

  function keyOf(node) {
    return node.nodeType === 1 ? (node.dataset.ssfNode ?? null) : null;
  }

  /// Whether a mounted node can hold what a frame asks for: a text node holds
  /// text, anything else holds a node of its own tag. What it says is `patch`'s
  /// to write; this only decides whether the node can be kept at all.
  function holds(mounted, want) {
    if (mounted.nodeType !== want.nodeType) return false;
    return want.nodeType === Node.TEXT_NODE || mounted.tagName === want.tagName;
  }

  /// The event handler properties a frame's nodes may carry; see `patch`.
  const HANDLERS = ["onclick", "onchange", "oninput", "onkeydown", "onsubmit"];

  /// Draw `wanted` into `parent`, keeping the nodes already there.
  ///
  /// Every frame is drawn from scratch and nothing here is a framework: a node
  /// that must survive a redraw carries `data-ssf-node`, its key, and is
  /// matched to the mounted node with the same key; the rest pair up in order,
  /// stepping over the keyed ones. A matched node is written to only where it
  /// differs from what the frame asks for, so a frame that restates the page
  /// leaves the tree -- an open picker's own popup, a caret, a scrolled box --
  /// exactly as it is.
  function reconcile(parent, wanted) {
    const mounted = [...parent.childNodes];
    const keyed = new Map();
    for (const node of mounted) {
      const key = keyOf(node);
      if (key && !keyed.has(key)) keyed.set(key, node);
    }
    const kept = new Set();
    const order = [];
    let cursor = 0;
    for (const want of wanted) {
      const key = keyOf(want);
      let old = null;
      if (key) {
        const found = keyed.get(key);
        if (found && !kept.has(found) && holds(found, want)) old = found;
      } else {
        while (cursor < mounted.length && (keyOf(mounted[cursor]) || kept.has(mounted[cursor]))) {
          cursor += 1;
        }
        if (cursor < mounted.length && holds(mounted[cursor], want)) old = mounted[cursor];
        cursor += 1;
      }
      if (old) {
        kept.add(old);
        patch(old, want);
        order.push(old);
      } else {
        order.push(want);
      }
    }
    // Drop what no frame asked for before putting anything in place: a stale
    // node left among the wanted ones would push every node after it along, and
    // a node that moves is a node that loses the focus inside it and closes the
    // picker it holds -- so nothing is moved that does not have to be.
    for (const node of mounted) {
      if (!kept.has(node)) node.remove();
    }
    let index = 0;
    for (const node of order) {
      const at = parent.childNodes[index];
      if (at !== node) parent.insertBefore(node, at ?? null);
      index += 1;
    }
  }

  /// Write what differs from `want` into `mounted`, leaving the node itself --
  /// and anything the person is doing to it -- where it is.
  function patch(mounted, want) {
    if (mounted === want) return;
    if (want.nodeType === Node.TEXT_NODE) {
      if (mounted.data !== want.data) mounted.data = want.data;
      return;
    }
    const attributes = new Map();
    for (const { name, value } of want.attributes) attributes.set(name, value);
    for (const { name } of [...mounted.attributes]) {
      if (!attributes.has(name)) mounted.removeAttribute(name);
    }
    for (const [name, value] of attributes) {
      if (mounted.getAttribute(name) !== value) mounted.setAttribute(name, value);
    }
    // A kept node takes the frame's handlers too: a handler is what a button
    // does, and it closes over the frame that drew it, so a node reused for
    // another button -- Kill anyway where Kill was -- must not keep doing what
    // the old one did. The forms set every handler as one of these
    // properties. (The few listeners this file adds with addEventListener act
    // on the node they were added to, which is the one that stays.)
    for (const name of HANDLERS) {
      if (mounted[name] !== want[name]) mounted[name] = want[name];
    }
    reconcile(mounted, [...want.childNodes]);
    // A box or a picker holds its value as a property rather than an attribute,
    // and the state map already holds what the person typed or picked: equal
    // means nothing is written, and the caret stays where they left it.
    if ("value" in mounted && mounted.value !== want.value) mounted.value = want.value;
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

  function twoDigits(value) {
    return String(value).padStart(2, "0");
  }

  /// `HH:MM` on the reader's clock, for the stale modifier.
  function clock(time) {
    const at = new Date(time);
    if (Number.isNaN(at.getTime())) return "an unknown time";
    return `${twoDigits(at.getHours())}:${twoDigits(at.getMinutes())}`;
  }

  /// An absolute time for a tooltip.
  function absolute(time) {
    if (time === null || time === undefined) return null;
    const at = new Date(time);
    if (Number.isNaN(at.getTime())) return null;
    return (
      `${at.getFullYear()}-${twoDigits(at.getMonth() + 1)}-${twoDigits(at.getDate())}` +
      ` ${twoDigits(at.getHours())}:${twoDigits(at.getMinutes())}`
    );
  }

  /// A link to `owner/repo#number`, on the issue tracker rather than the pull
  /// request tracker, since that is where ssf and GitHub both put items.
  function itemLink(id, text) {
    const match = /^([^/#]+)\/([^/#]+)#(\d+)$/.exec(String(id ?? ""));
    if (!match) return element("span", undefined, text ?? String(id ?? ""));
    const link = element("a", undefined, text ?? `#${match[3]}`);
    link.href = `https://github.com/${match[1]}/${match[2]}/issues/${match[3]}`;
    link.target = "_blank";
    link.rel = "noreferrer noopener";
    return link;
  }

  function shape(tag, attributes, paint, stroked) {
    const node = document.createElementNS(SVG_NS, tag);
    for (const [name, value] of Object.entries(attributes)) {
      node.setAttribute(name, String(value));
    }
    if (stroked) {
      node.setAttribute("fill", "none");
      node.setAttribute("stroke", paint);
    } else {
      node.setAttribute("fill", paint);
    }
    return node;
  }

  /// The glyph inside a solid disc, in white; the same shape in the state
  /// colour when the disc is only outlined.
  function glyph(kind, solid) {
    const paint = solid ? "#ffffff" : "currentColor";
    switch (kind) {
      case "waiting":
        return [
          shape("rect", { x: 4.1, y: 4, width: 1.4, height: 4, rx: 0.5 }, paint, false),
          shape("rect", { x: 6.5, y: 4, width: 1.4, height: 4, rx: 0.5 }, paint, false),
        ];
      case "done":
        return [
          shape(
            "path",
            {
              d: "M3.7 6.2 5.3 7.8 8.5 4.2",
              "stroke-width": 1.6,
              "stroke-linecap": "round",
              "stroke-linejoin": "round",
            },
            paint,
            true,
          ),
        ];
      case "problem":
        return [
          shape(
            "path",
            { d: "M6 3.3V6.9", "stroke-width": 1.6, "stroke-linecap": "round" },
            paint,
            true,
          ),
          shape("circle", { cx: 6, cy: 8.7, r: 0.85 }, paint, false),
        ];
      default:
        return [];
    }
  }

  /// One icon per state: a solid disc carrying a white glyph for the three
  /// states that have a glyph (waiting, done, problem; Working and No agent are
  /// the bare disc), and -- when the snapshot behind it is stale -- the same
  /// shape outlined and dashed instead of filled, so a stale snapshot can never
  /// render as a working agent.
  function icon(kind, stale) {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 12 12");
    svg.setAttribute("width", "12");
    svg.setAttribute("height", "12");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("ssf-icon");
    svg.dataset.ssfTone = kind;
    const solid = !stale;
    const ring = shape("circle", { cx: 6, cy: 6, r: 4.6 }, solid ? "currentColor" : "none", !solid);
    if (!solid) {
      ring.setAttribute("stroke", "currentColor");
      ring.setAttribute("stroke-width", "1.3");
      ring.setAttribute("stroke-dasharray", "2 1.6");
    }
    svg.append(ring);
    for (const part of glyph(kind, solid)) svg.append(part);
    return svg;
  }

  /// The stack a session is on, as one line: harness, model and effort. The
  /// effort belongs with the other two -- it is what the session's tokens cost,
  /// and the hand-over pickers are prefilled from the same three fields -- and
  /// it was the one setting no screen showed (#439).
  ///
  /// A session whose next launch would start another harness says both, which
  /// is the reading the server's own dashboard uses: the pane's stack, then
  /// what the next launch would use, since a config edit does not touch a
  /// session that is already running.
  function stackLine(item) {
    const running = [item?.harness, item?.model, item?.effort].filter(Boolean).join(" · ");
    const next = item?.next_launch;
    if (!next?.harness) return running;
    const to = [next.harness, next.model, next.effort].filter(Boolean).join(" · ");
    return `${running} → ${to} next launch`;
  }

  /// Why there is no activity time for `match`, where there is none. The
  /// factory's own reason travels on the card (`activity_note`), because ssf
  /// dates a session from the local transcript its harness keeps and every way
  /// that can be missing is a different fact: an OMP session never has one,
  /// since OMP keeps no transcript ssf can read (#439). The overlay's own two
  /// readings -- an item ssf monitors, one it has no record of -- have no
  /// session to date at all, and say that instead of "no activity recorded",
  /// which read as a claim about the agent.
  function activityNote(match) {
    if (match.kind === "monitored") return "no agent, so nothing is running to date";
    if (match.kind === "assignable") return "no record, so no session has run on this item";
    return match.item?.activity_note ?? null;
  }

  /// What one item's state is, as this factory reports it. A factory that has
  /// gone stale keeps the states from its last snapshot but says when that was;
  /// one that never answered is a problem, not an agent.
  ///
  /// `stale` is the overlay's own modifier -- a daemon that is not answering,
  /// an unreachable VM, an unavailable driver or an overdue poll also make the
  /// snapshot untrustworthy, which is what `warning` carries.
  ///
  /// `closed` is the page's own fact about the item: see `closedInDom`.
  ///
  /// `time` is null where there is nothing truthful to put beside the state: a
  /// live factory that reports no activity time for the session, which is every
  /// OMP session, says so in the tooltip and in Details rather than filling the
  /// line with "no activity recorded".
  function stateFact(factory, match, closed = false) {
    const assignable = match.kind === "assignable";
    const raw = assignable
      ? NO_RECORD
      : match.kind === "monitored"
        ? "unbound"
        : String(match.item.agent_state ?? "").trim() || "unknown";
    const shown = assignable ? NO_AGENT : (PRESENTATION[raw] ?? PROBLEM);
    // An item the page shows closed or merged is not a problem: ssf has nothing
    // running on it because there is nothing left to run, and a released
    // workspace reads `no-workspace`, which would otherwise be the Problem
    // colour -- a red chip on a merged item (#426's board, #411). The raw word
    // is still what the tooltip leads with.
    const finished =
      closed && (shown.kind === "problem" || shown.kind === "no-agent");
    // A snapshot is trustworthy only when the stream is live and the factory has
    // not flagged it: its own warning means the TUI paints the same snapshot
    // UNAVAILABLE / STALE, so a solid state here would contradict it.
    const live = factory.state === "live" && !factory.warning;
    const detail = [`ssf state: ${raw}`];
    if (match.kind === "monitored") {
      detail.push("ssf monitors this item but has no agent on it");
    }
    if (assignable) {
      detail.push(
        "the factory watches this repository and has no record of this item, so no session is running on it; `ssf assign` starts one here",
      );
    }
    if (finished) {
      detail.push("the page shows this item closed or merged, so nothing is left to run");
    }
    if (factory.warning) {
      detail.push(`the factory says its status may be incomplete: ${factory.warning}`);
    }
    if (factory.state === "stale") {
      detail.push(
        factory.error ?? "the factory stopped sending events; this is its last snapshot",
      );
    } else if (factory.state === "error") {
      detail.push(factory.error ?? "the factory did not answer");
    } else if (factory.state !== "live") {
      detail.push("the factory has not answered yet");
    }
    let time;
    if (factory.state === "error") time = "unreachable";
    else if (live) time = ago(match.item.last_activity_at);
    else time = `as of ${clock(factory.lastFrameAt)}`;
    // Nothing to put beside the state? Then say why, in the places a reason
    // belongs -- the tooltip and Details -- rather than printing "no activity
    // recorded", which is not a time and reads as the agent having gone quiet.
    const activity = live && !time ? activityNote(match) : null;
    if (activity) detail.push(activity);
    return {
      label: finished ? "Done" : shown.label,
      kind: finished ? "done" : shown.kind,
      raw,
      stale: !live,
      time,
      activity,
      detail,
    };
  }

  /// `match` -> the facts a tooltip shows for it: the raw state word, the stack,
  /// the absolute time and the first line of the last message.
  function tooltip(factory, match, closed = false) {
    const state = stateFact(factory, match, closed);
    const lines = [`ssf state: ${state.raw}`];
    const stack = stackLine(match.item);
    const when =
      absolute(match.item.last_activity_at) ??
      state.activity ??
      (factory.lastFrameAt ? `as of ${absolute(factory.lastFrameAt)}` : null);
    lines.push([factory.label, stack, when].filter(Boolean).join(" · "));
    const message = messageFor(match);
    if (message) lines.push(firstLine(message));
    lines.push(...state.detail.slice(1));
    return lines.join("\n");
  }

  function messageFor(match) {
    // An item the factory has no record of has no message either: ssf has
    // never run anything on it, and the overlay does not invent one from the
    // page's own title.
    if (match.kind === "assignable") return "";
    const message =
      match.kind === "monitored" ? match.item.title : match.item.last_assistant_message;
    return message ? String(message).trim() : "";
  }

  function firstLine(text) {
    const line = String(text).split("\n").find((part) => part.trim()) ?? "";
    return line.trim().length > 160 ? `${line.trim().slice(0, 157)}...` : line.trim();
  }

  /// Every factory that knows this `owner/repo#number`: its own card, or -- when
  /// the item is an additional one of another agent -- that agent, or an item
  /// ssf monitors without an agent. A factory that never answered cannot say
  /// whether it knows the item and is reported separately.
  ///
  /// `unrecorded` also matches a factory that *watches the item's repository*
  /// and has no record of the item at all: no card and not monitored. ssf has
  /// said nothing about such an item, which is exactly the item `ssf assign`
  /// starts a session on, and the factory publishes the repositories it watches
  /// for this (#435). The caller decides where that reading belongs: an item's
  /// own page and a board card, where there is one row per item and the form is
  /// the point, but not a list or a search result, where a watched repository's
  /// whole backlog as a column of grey chips is not what the page is read for.
  function matchesFor(key, { unrecorded = false } = {}) {
    const out = [];
    for (const factory of snapshot?.factories ?? []) {
      const own = factory.cards.find((card) => card.origin?.id === key);
      if (own) {
        out.push({ factory, item: own, kind: "agent" });
        continue;
      }
      const other = factory.cards.find((card) =>
        (card.additional ?? []).some((issue) => issue?.id === key),
      );
      if (other) {
        out.push({ factory, item: other, kind: "additional" });
        continue;
      }
      const monitored = factory.monitoredItems.find((item) => item?.id === key);
      if (monitored) {
        out.push({ factory, item: monitored, kind: "monitored" });
        continue;
      }
      if (unrecorded && watches(factory, key)) {
        // The key is `owner/name#number`. A record carries a title, a branch
        // and a workspace; this reading has none of them to carry, and
        // `has_workspace` is false because `ssf assign` refuses an item that
        // has one -- so the form is only ever drawn where there is nothing in
        // the way.
        out.push({ factory, item: { id: key, has_workspace: false }, kind: "assignable" });
      }
    }
    return out;
  }

  /// Whether one factory watches the repository `key` names. A factory that has
  /// not answered publishes no repositories, so it claims none.
  function watches(factory, key) {
    const repo = key.slice(0, key.indexOf("#"));
    return (factory.repositories ?? []).includes(repo);
  }

  /// A factory that has never answered cannot say whether it knows the item, so
  /// it is named rather than staying silent, which would read as "no agent".
  /// One that is still connecting knows nothing yet and is not a problem.
  function unreadableFactories() {
    return (snapshot?.factories ?? []).filter((factory) => factory.state === "error");
  }

  /// The state line every screen shares: the icon, the state word, and either
  /// the relative last activity or, for a stale snapshot, when it was taken.
  /// With neither -- a live factory that reports no activity time, which is
  /// every OMP session -- the line is the state alone, and the reason is in the
  /// tooltip and in Details (#439).
  function stateLine(factory, match, closed = false) {
    const state = stateFact(factory, match, closed);
    const line = element("div", "ssf-state");
    // Hover always names the raw state word the TUI prints, so "Waiting on you"
    // never hides the fact that ssf says `idle`, and the factory's own reason
    // travels with it.
    line.title = state.detail.join("\n");
    line.append(icon(state.kind, state.stale));
    const word = element("span", "ssf-word", state.label);
    word.dataset.ssfTone = state.kind;
    line.append(word);
    if (!state.time) return line;
    line.append(element("span", "ssf-sep", "·"));
    const when = element("span", "ssf-when", state.time);
    if (state.stale) when.dataset.stale = "true";
    line.append(when);
    return line;
  }

  /// The message, trimmed to two lines behind a `more` toggle that is shown only
  /// when the text is actually clipped.
  function messageBlock(name, factory, match) {
    const message = messageFor(match);
    if (!message) return null;
    const key = `${name}|${factory.url}`;
    const flags = opened.get(key) ?? {};
    const wrap = element("div", "ssf-said");
    // Where `measure` finds this block's own answer again: whether the message
    // overflows the clamp is a fact about the layout, not about the frame.
    wrap.ssfKey = key;
    const body = element("div", "ssf-message", message);
    body.dataset.clamped = String(!flags.more);
    const more = element("button", "ssf-more", flags.more ? "less" : "more");
    more.type = "button";
    // Up when the reader has the message open, or when the last layout pass
    // found the trimmed message really is clipped. Drawn here rather than
    // written by the pass afterwards, so a frame that says what the page
    // already says leaves this node -- and everything else in the block --
    // alone.
    more.hidden = !(flags.more || overflowed.get(key) === true);
    more.setAttribute("aria-expanded", String(Boolean(flags.more)));
    more.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      const now = !(opened.get(key)?.more ?? false);
      opened.set(key, { ...(opened.get(key) ?? {}), more: now });
      body.dataset.clamped = String(!now);
      more.textContent = now ? "less" : "more";
      more.setAttribute("aria-expanded", String(now));
      if (popover) placePopover();
    });
    wrap.append(body, more);
    return wrap;
  }

  /// The rows Details carries for one card, each a fact the card is holding:
  /// what ssf says the state is, which item this is, the stack the session runs
  /// (harness, model and effort), the stack the next launch would use when it
  /// differs, what the agent is doing, the workspace it is doing it in, the
  /// session's own id, the factory and the item's activity.
  ///
  /// A row with nothing to say is left out rather than filled with "not
  /// reported": four rows of "not reported" around the one fact the card has is
  /// what made this read as missing information (#439). What the state row says
  /// is the reading the rest of the card is drawn from, so the raw word the
  /// tooltip leads with is here too, in the same terms.
  function detailFacts(factory, match, closed = false) {
    const state = stateFact(factory, match, closed);
    const item = match.item ?? {};
    const facts = [["State", detailState(state, match)]];
    // Which item this card is about. An additional item's card is about the
    // agent's own item -- it leads with "worked on by the agent on #N" -- so it
    // is the one card that does not name an item of its own.
    if (match.kind !== "additional") {
      facts.push(["Item", item.origin?.id ?? item.id ?? null]);
    }
    if (match.kind === "agent" || match.kind === "additional") {
      const stack = stackLine(item);
      if (stack) facts.push(["Stack", stack]);
      if (item.next_launch?.harness) {
        facts.push([
          "Next launch",
          [item.next_launch.harness, item.next_launch.model, item.next_launch.effort]
            .filter(Boolean)
            .join(" · "),
        ]);
      }
      if (item.tool) facts.push(["Doing", item.tool]);
    }
    if (item.branch) facts.push(["Branch", item.branch]);
    if (item.worktree_path) facts.push(["Workspace", item.worktree_path]);
    if (match.kind === "monitored" && item.has_workspace) {
      facts.push([
        "Workspace",
        item.branch ? `${item.branch}, held by no session` : "held by no session",
      ]);
    }
    if (match.kind === "agent" || match.kind === "additional") {
      if (item.agent_session_id) facts.push(["Session", item.agent_session_id]);
    }
    facts.push(["Factory", item.factory ?? factory.label]);
    if (state.activity || item.last_activity_at) {
      const when = absolute(item.last_activity_at) ?? state.activity;
      const relative = ago(item.last_activity_at);
      facts.push(["Active", relative ? `${when} · ${relative}` : when]);
    }
    if (item.handover?.harness) {
      facts.push([
        "Hand over",
        [item.handover.harness, item.handover.model, item.handover.effort]
          .filter(Boolean)
          .join(" · ") +
          (item.handover.by ? `, asked by ${item.handover.by}` : "") +
          ", on the daemon's next pass",
      ]);
    }
    return facts;
  }

  /// The State row: the word on screen, and what ssf actually said behind it.
  /// The one reading here that is the overlay's own -- an item ssf monitors --
  /// says so rather than putting a word ssf never said behind a raw state. A
  /// card for an item the factory has no record of is the other, and it carries
  /// no Details at all: every row would be empty, and the form under the card is
  /// the only thing there is to say about it.
  function detailState(state, match) {
    if (match.kind === "monitored") {
      return `${state.label} — ssf monitors this item with no agent on it`;
    }
    if (state.label === state.raw) return state.raw;
    return `${state.label} — ssf reports ${state.raw}`;
  }

  /// Details: every fact the card holds, folded away until it is asked for.
  function detailsBlock(name, factory, match, closed = false) {
    const key = `${name}|${factory.url}`;
    const flags = opened.get(key) ?? {};
    const details = element("details", "ssf-details");
    details.open = Boolean(flags.details);
    details.addEventListener("toggle", () => {
      opened.set(key, { ...(opened.get(key) ?? {}), details: details.open });
    });
    details.append(element("summary", undefined, "Details"));
    const list = element("dl");
    for (const [term, value] of detailFacts(factory, match, closed)) {
      if (!value) continue;
      const dd = element("dd");
      dd.title = String(value);
      if (term === "Workspace" && String(value).startsWith("/")) {
        dd.dataset.path = "true";
        dd.append(element("bdi", undefined, String(value)));
      } else {
        dd.textContent = String(value);
      }
      list.append(element("dt", undefined, term), dd);
    }
    details.append(list);
    return details;
  }

  /// One factory's card for an item, laid out as #497's option 1a: a band at
  /// the top tinted by the state -- the one place a state colour fills a
  /// container -- then the body (the stack, the branch, the factory, the
  /// message and the writes), and Details folded away at the foot. An issue
  /// that is an additional item of another agent leads its body with `worked
  /// on by the agent on #N`, so the card never claims an agent that belongs to
  /// a different issue.
  function card(name, factory, match, closed = false) {
    const node = element("div", "ssf-card");
    const band = stateLine(factory, match, closed);
    const state = stateFact(factory, match, closed);
    band.classList.add("ssf-band");
    band.dataset.ssfBand = state.kind;
    if (state.stale) band.dataset.stale = "true";
    band.append(element("span", "ssf-brand", "SSF"));
    node.append(named(band, "state"));
    const body = element("div", "ssf-body");
    node.append(named(body, "body"));
    if (match.kind === "additional") {
      const via = element("div", "ssf-via");
      via.append("worked on by the agent on ", itemLink(match.item.origin?.id));
      body.append(named(via, "worked"));
    }
    const stack = stackChips(match.item);
    if (stack) body.append(named(stack, "stack"));
    if (match.item.branch) {
      const branch = element("div", "ssf-branch");
      branch.append(element("span", "ssf-branch-mark", "\u2387"));
      const value = element("span", "ssf-clip", match.item.branch);
      value.title = match.item.branch;
      branch.append(value);
      body.append(named(branch, "branch"));
    }
    const where = whereLine(factory, match, closed);
    if (where) body.append(named(where, "where"));
    const message = messageBlock(name, factory, match);
    if (message) body.append(named(message, "said"));
    const also = (match.item.additional ?? []).filter((issue) => issue?.id);
    if (match.kind === "agent" && also.length) {
      const line = element("div", "ssf-also");
      line.append("also on: ");
      also.forEach((issue, index) => {
        if (index) line.append(" ");
        line.append(itemLink(issue.id));
      });
      body.append(named(line, "also"));
    }
    // A factory that never answered has nothing to report about this item, and
    // neither has one with no record of it: there is no tool call, branch,
    // workspace or session to list, and Details would be the one row naming the
    // factory again.
    if (match.kind !== "unreadable" && match.kind !== "assignable") {
      node.append(named(detailsBlock(name, factory, match, closed), "details"));
    }
    return node;
  }

  /// The stack as chips -- harness, model, effort -- in the monospace GitHub
  /// uses for something a person might copy. The effort is the chip the eye is
  /// drawn to, since it is what the session costs. A next launch on another
  /// stack follows as muted text; Details carries it in full.
  function stackChips(item) {
    const parts = [item?.harness, item?.model, item?.effort];
    if (!parts.some(Boolean)) return null;
    const row = element("div", "ssf-stack");
    parts.forEach((part, index) => {
      if (!part) return;
      const chip = element("span", "ssf-tag", String(part));
      if (index === 2) chip.dataset.effort = "true";
      row.append(chip);
    });
    const next = item?.next_launch;
    if (next?.harness) {
      const to = [next.harness, next.model, next.effort].filter(Boolean).join(" · ");
      row.append(element("span", "ssf-next", `→ ${to} next launch`));
    }
    return row;
  }

  /// `on <factory> · <why there is no activity time>`. The factory is named
  /// when there is more than one to tell apart, and always on a card for an
  /// item it has no record of (the write goes to it) or one it could not be
  /// read for (which factory is the whole point). The note is there only when
  /// the band has no time to show.
  function whereLine(factory, match, closed) {
    const state = stateFact(factory, match, closed);
    const many = (snapshot?.factories?.length ?? 0) > 1;
    const parts = [];
    const line = element("div", "ssf-where");
    if (many || match.kind === "assignable" || match.kind === "unreadable") {
      line.append("on ", element("strong", undefined, factory.label));
      parts.push(true);
    }
    if (state.activity) {
      if (parts.length) line.append(" · ");
      line.append(state.activity);
      parts.push(true);
    }
    return parts.length ? line : null;
  }

  /// An item's writes for `itemKey`, on the card of the factory they belong to:
  /// the Assign agent form where it may be started, the note that says what
  /// frees an item ssf already holds a workspace for, and an Actions row on the
  /// card of each factory that has an agent on the item. The nodes are drawn
  /// fresh per call -- the card and an open popover each get their own -- and
  /// marked with one key, `writes`, so the frame after this one writes into the
  /// form already on screen rather than drawing a second one.
  ///
  /// `itemKey` is the item the card is *about*: for a pull request page that is
  /// the issue its body closes, not the pull request, so the write acts on the
  /// same item the card names.
  ///
  /// `closed` is the page's own fact that the item is finished: a finished
  /// item is not one to start a session on, so it gets neither the form nor
  /// the note about what frees it. An agent still on it keeps its Actions row,
  /// whose Release is what gives a kept workspace back.
  function withWrites(section, matches, itemKey, closed = false) {
    // Each card's writes go at the foot of its body, above Details.
    const cards = [...section.querySelectorAll(".ssf-card")].map(
      (card) => card.querySelector(".ssf-body") ?? card,
    );
    // An item ssf already has a workspace for is one the write would be refused
    // for, so its card says what frees it rather than offering the form; a
    // factory whose answer is the one that takes the write draws the form
    // whether or not another factory's card carries the note.
    for (const match of closed ? [] : matches.filter(held)) {
      cards[matches.indexOf(match)]?.append(
        named(element("p", "ssf-hold", "Has a workspace; release it first."), "hold"),
      );
    }
    const [repo, number] = itemKey.split("#");
    const where = Number(number);
    // Every factory with an agent on the item gets its own row: a row writes
    // through one factory, and an agent is that factory's, so two factories
    // with a session each are two sessions to act on and each needs its own.
    for (const match of matches.filter((one) => one.kind === "agent")) {
      // Show agent leads the row: the agent's own terminal, over the page.
      const session = match.item.owner ?? match.item.origin?.id;
      const open = globalThis.ssfPane?.button(
        match.factory.url,
        session,
        match.item.pane_input === true,
        "Show agent",
      );
      const row = globalThis.ssfWrites?.renderActions({
        factories: [match.factory],
        repo,
        number: where,
        item: match.item,
        open,
      });
      if (row) cards[matches.indexOf(match)]?.append(named(row, "writes"));
    }
    // The Assign agent form goes where a session can still be started: a card
    // with no agent, with nothing in the way. A card that carries a row is one
    // the factory already has a session for -- `ssf assign` refuses it, and the
    // row's own Hand over… and Release are what work there -- so it is left to
    // its row. One card offers one write: the card a row is on is never also offered
    // a form, which is what makes the two branches a decision rather than a
    // coincidence of the states ssf reports today. The card the form belongs on
    // may be another factory's, one with nothing on the item at all.
    const assignableMatches = matches.filter(
      (match) => match.kind !== "agent" && assignable(match),
    );
    if (closed || !assignableMatches.length) return section;
    const form = globalThis.ssfWrites?.render({
      factories: assignableMatches.map((match) => match.factory),
      repo,
      number: where,
    });
    if (!form) return section;
    const card = cards[matches.indexOf(assignableMatches[0])] ?? section;
    card.append(named(form, "writes"));
    return section;
  }

  /// Every card that belongs in one place: one per factory that knows the item,
  /// plus one per factory that could not be read at all. `forLabel` is the issue
  /// a pull request page resolved through, so its card says which issue it is
  /// about; `popover` swaps the sidebar's margin for the popover's own frame;
  /// `closed` is the page's own fact about the item, which only a chip and its
  /// own popover have (a sidebar card is about the page's item, or -- on a pull
  /// request page -- about the issue it resolves through, whose state the page
  /// does not show).
  function cards(name, matches, unreadable, { forLabel = null, popover = false, closed = false } = {}) {
    const section = element("div", popover ? "ssf-popover" : "ssf-section");
    // Which factory a card is from is its body's `on <factory>` line; see
    // `whereLine`.
    let first = true;
    for (const match of matches) {
      // A card is its factory's: an item that gains an agent keeps the node it
      // had, and the form in it is patched into the row that replaces it.
      const node = named(card(name, match.factory, match, closed), `card:${match.factory.url}`);
      if (first && forLabel) {
        node.querySelector(".ssf-body").prepend(
          named(element("div", "ssf-via", `for ${forLabel}`), "for"),
        );
      }
      first = false;
      section.append(node);
    }
    for (const factory of unreadable) {
      const node = named(
        card(name, factory, {
          factory,
          item: { agent_state: "" },
          kind: "unreadable",
        }),
        `card:${factory.url}`,
      );
      if (first && forLabel) {
        node.querySelector(".ssf-body").prepend(
          named(element("div", "ssf-via", `for ${forLabel}`), "for"),
        );
      }
      first = false;
      section.append(node);
    }
    return section;
  }

  /// The chip beside an issue or pull request in a list, board or search
  /// result: icon, state word, relative last activity. The tooltip carries the
  /// harness, the model, the absolute time and the first line of the last
  /// message; the popover on a click carries the whole card.
  function chip(name, matches, closed = false) {
    // The word a chip carries comes from a factory with something to report.
    // The overlay's own reading of an item no factory has a record of is the
    // fallback for when no other factory knows the item, and on a board where
    // one factory works an item and another only watches its repository both
    // match -- so the reading must not displace a report ssf did make (#435).
    // The tooltip still carries every matching factory's reading.
    const first = matches.find((match) => match.kind !== "assignable") ?? matches[0];
    const state = stateFact(first.factory, first, closed);
    const node = element("div", "ssf-chip");
    node.setAttribute("role", "button");
    node.setAttribute("tabindex", "0");
    node.setAttribute(
      "aria-label",
      ["ssf:", state.label, state.time ?? state.activity].filter(Boolean).join(" "),
    );
    node.append(icon(state.kind, state.stale));
    const word = element("span", "ssf-word", state.label);
    word.dataset.ssfTone = state.kind;
    node.append(word);
    // With no time to put beside the state the chip is the state alone, and the
    // reason -- which is a sentence, not a chip's worth of text -- is in the
    // tooltip and the popover's Details (#439).
    if (state.time) {
      node.append(element("span", "ssf-sep", "·"));
      const when = element("span", "ssf-when", state.time);
      if (state.stale) when.dataset.stale = "true";
      node.append(when);
    }
    node.title = matches
      .map((match) => tooltip(match.factory, match, closed))
      .join("\n\n");
    return node;
  }

  /// Draw one entry's content and put the host where it belongs. Returns false
  /// when its place is not on the page yet; the next mutation re-renders.
  ///
  /// The frame is drawn from scratch and reconciled into what is already there,
  /// so a frame that says what the page already says writes nothing at all --
  /// which is where an open picker and a caret survive it -- and the placement
  /// below is a no-op when nothing moved: neither feeds the observer that
  /// drives this.
  function update(entry, want) {
    if (want.scratch) return updateScratch(entry, want);
    const closed = want.closed === true;
    // What a click on this chip reads when it opens its popover: the same
    // reading the chip was drawn from, so a board card's popover knows the item
    // is one the factory has no record of (#435).
    entry.unrecorded = want.unrecorded === true;
    const body =
      want.anchor === null
        ? cards(want.name, want.matches, want.unreadable ?? [], {
            forLabel: want.target?.label ?? null,
            closed,
          })
        : chip(want.name, want.matches, closed);
    // The item's writes are drawn into the card they belong to, in the same
    // tree and before it is put in the page: the Assign agent form, or the
    // Actions row. Drawing them into this frame's own tree is what lets the
    // reconcile below keep the nodes of a form already on screen.
    if (want.anchor === null) withWrites(body, want.matches, want.key, closed);
    reconcile(entry.shadow, [body]);
    let placed = false;
    if (want.anchor === null) {
      const slot = sidebar();
      if (!slot) return false;
      if (entry.host.dataset.ssfSlot !== slot.slot) entry.host.dataset.ssfSlot = slot.slot;
      // Above Assignees; at the top when the page has no Assignees section.
      if (entry.host.parentElement !== slot.parent) slot.parent.prepend(entry.host);
      if (slot.before && slot.before.previousElementSibling !== entry.host) {
        slot.parent.insertBefore(entry.host, slot.before);
      }
      placed = true;
    } else {
      if (!want.anchor.isConnected) return false;
      if (
        !entry.host.isConnected ||
        entry.host.previousElementSibling !== want.anchor
      ) {
        want.anchor.insertAdjacentElement("afterend", entry.host);
      }
      placed = true;
    }
    if (placed) measure(entry);
    return placed;
  }

  /// A repository page's scratch sessions: one card per factory that watches
  /// the repository, holding its sessions and New scratch. `mine` is the
  /// GitHub user signed in on this page, from GitHub's own `user-login` meta
  /// tag: it says whose session to make, and is not an access control.
  function updateScratch(entry, want) {
    const section = element("div", "ssf-section");
    section.append(element("h3", "ssf-title", "SSF scratch"));
    const login = document.querySelector('meta[name="user-login"]')?.content?.trim() || null;
    const label = (snapshot?.factories?.length ?? 0) > 1;
    for (const factory of want.factories) {
      const sessions = (factory.scratch ?? [])
        .filter((one) => sameRepo(one?.repo, want.repo))
        .map((one) => ({ ...one, stateLabel: scratchState(factory, one) }));
      const node = named(element("div", "ssf-card"), `card:${factory.url}`);
      if (label) node.append(named(element("div", "ssf-via", factory.label), "via"));
      const panel = globalThis.ssfWrites?.renderScratch({
        factories: [factory],
        repo: want.repo,
        login,
        sessions,
      });
      if (panel) {
        node.append(named(panel, "writes"));
      } else {
        const count = `${sessions.length} scratch session${sessions.length === 1 ? "" : "s"}`;
        node.append(
          named(element("p", "ssf-hold", `${count}; writes are off for this factory.`), "off"),
        );
      }
      section.append(node);
    }
    reconcile(entry.shadow, [section]);
    const slot = repoSidebar();
    if (!slot) return false;
    if (entry.host.dataset.ssfSlot !== "sidebar") entry.host.dataset.ssfSlot = "sidebar";
    if (entry.host.parentElement !== slot || slot.firstElementChild !== entry.host) {
      slot.prepend(entry.host);
    }
    return true;
  }

  /// Where a repository page's scratch section goes: the top of the right
  /// sidebar, wherever that page's own markup puts it. GitHub's code view
  /// renders that column as a React pane (`CodeViewSidebar-...`); the classic
  /// `Layout-sidebar` is what the older markup had.
  function repoSidebar() {
    return (
      document.querySelector(".Layout-sidebar") ??
      document.querySelector('[class*="CodeViewSidebar-module__borderGrid"]')
    );
  }

  function sameRepo(a, b) {
    return String(a ?? "").toLowerCase() === String(b ?? "").toLowerCase();
  }

  /// The word a scratch session's row carries: its card's state while an agent
  /// runs, Off while its workspace is there and its terminal is not, then
  /// Releasing and Killed.
  function scratchState(factory, one) {
    const phase = globalThis.ssfWrites?.scratchPhase(one) ?? (one.active ? "live" : "released");
    if (phase === "off") return "Off";
    if (phase === "releasing") return "Releasing";
    if (phase === "released") return "Killed";
    const card = factory.cards.find((each) => each.origin?.id === one.id);
    if (card) return (PRESENTATION[String(card.agent_state ?? "").trim()] ?? PROBLEM).label;
    return NO_AGENT.label;
  }

  /// Show a `more` toggle only where the trimmed message really is clipped, and
  /// keep that against the block's own key: a frame after this one draws the
  /// toggle the way it is, so the button is not hidden on every frame and shown
  /// again here.
  ///
  /// A message the reader has expanded keeps its `less`: the clamp is off, so
  /// there is no overflow left to measure and nothing to take the toggle away
  /// for.
  function measure(entry) {
    for (const wrap of entry.shadow.querySelectorAll(".ssf-said")) {
      const message = wrap.querySelector(".ssf-message");
      const more = wrap.querySelector(".ssf-more");
      if (!more || !message) continue;
      const toggle =
        message.dataset.clamped !== "true" ||
        message.scrollHeight > message.clientHeight + 1;
      if (wrap.ssfKey) overflowed.set(wrap.ssfKey, toggle);
      if (more.hidden !== !toggle) more.hidden = !toggle;
    }
  }

  /// Where the sidebar card goes: GitHub's issue sidebar, above Assignees --
  /// the container an item's own page and a project view's side panel both
  /// draw -- and the older discussion sidebar a pull request page still uses.
  /// Two different containers, so both are tried.
  function sidebar() {
    const modern = document.querySelector('[class*="IssueSidebar-module__sidebarContent"]');
    if (modern) {
      return { parent: modern, before: sectionTitled(modern, "Assignees"), slot: "sidebar" };
    }
    const legacy = document.querySelector("#partial-discussion-sidebar");
    if (legacy) {
      return {
        parent: legacy,
        before: sectionTitled(legacy, "Assignees"),
        slot: "sidebar-legacy",
      };
    }
    return null;
  }

  function sectionTitled(parent, title) {
    for (const child of parent.children) {
      const heading = child.querySelector("h2, h3");
      if (heading && heading.textContent.trim().startsWith(title)) return child;
    }
    return null;
  }

  /// The item a project view's side panel is showing, in the shape a detail
  /// page's own path parses to, or null when no item panel is open. The panel
  /// is a detail view of one item drawn inside the board -- the same sidebar
  /// container the item's page has -- so that item belongs at the top of that
  /// sidebar exactly as it does on its own page (#440).
  ///
  /// GitHub states the open panel and the item it shows in the query string,
  /// `?pane=issue&itemId=...&issue=owner%7Crepo%7Cnumber`, and that is the
  /// panel's own state rather than a reading of its DOM: the parameter is there
  /// while the panel is and gone when it is closed. Only the item panel carries
  /// an `issue`, so the project information panel, which shares the panel's
  /// accessible name, names no item; nor does a draft item's panel, a draft
  /// having no issue URL to name. A panel names no pull request either, since
  /// GitHub does not open a pull request in one.
  function sidePanel() {
    const query = new URLSearchParams(location.search);
    if (query.get("pane") !== "issue") return null;
    const named = (query.get("issue") ?? "").split("|");
    // The number is checked the way a detail page's own path checks it, so a
    // URL that names no item -- hand-edited, or pasted from somewhere that
    // mangled it -- draws no card and offers no form for an item that is not
    // there.
    if (named.length !== 3 || !/^\d+$/.test(named[2])) return null;
    return { owner: named[0], repo: named[1], number: named[2], pull: false };
  }

  /// The title links on a list, board or search page, one per issue. A side
  /// panel is not one of those lists: it is a detail view of a single item,
  /// whose links are the item's own and the prose around it, so nothing inside
  /// it is a row to chip (#440).
  function listAnchors() {
    const anchors = new Map();
    for (const anchor of document.querySelectorAll("a[href]")) {
      const match = LINK_PATH.exec(anchor.pathname);
      if (!match || !anchor.textContent.trim()) continue;
      if (anchor.closest("nav, [role='navigation'], [role='dialog']")) continue;
      const key = `${match[1]}/${match[2]}#${match[3]}`;
      if (!anchors.has(key)) anchors.set(key, anchor);
    }
    return anchors;
  }

  /// The item a pull request page is about. A factory's own card for the pull
  /// request wins: that is the factory's binding -- its session tag or its
  /// branch -- and a delegated pull request is bound to its own session, not
  /// to the issue its body closes (#462). Failing that, the first issue its
  /// body closes, or the first it refs, which is how a delivery pull request
  /// resolves through the issue it delivers. A factory with no card for that
  /// issue but a record of the pull request still resolves, so a pull request
  /// page is never emptier than its own number -- and, in a repository the
  /// factory watches, an item it has no record of resolves too, whether the
  /// body named it or it is the pull request itself, since starting a session
  /// on it is what the form on this page is for (#435).
  function pullTarget(page) {
    const own = `${page.owner}/${page.repo}#${page.number}`;
    if (matchesFor(own).some((match) => match.kind === "agent")) {
      return { key: own, label: null };
    }
    const body = document.querySelector(
      '[data-testid="issue-body"], #discussion_bucket .js-comment-body, .js-comment-body',
    );
    const linked = body ? linkedNumber(body.textContent) : null;
    if (linked) {
      const key = `${page.owner}/${page.repo}#${linked}`;
      if (matchesFor(key, { unrecorded: true }).length) return { key, label: `#${linked}` };
    }
    if (matchesFor(own, { unrecorded: true }).length) return { key: own, label: null };
    return null;
  }

  /// `Closes`/`Fixes`/`Resolves #N` first, then `Refs #N`, first match wins.
  function linkedNumber(body) {
    for (const pattern of [CLOSING_REF, REFS_REF]) {
      pattern.lastIndex = 0;
      const match = pattern.exec(body);
      if (match) return match[1];
    }
    return null;
  }

  function openPopover(name, key, anchor, unrecorded) {
    closePopover();
    const entry = shadowHost("data-ssf-popover", name);
    popover = { name, key, anchor, entry, unrecorded };
    document.body.append(entry.host);
    renderPopover();
  }

  function renderPopover() {
    if (!popover) return;
    // The reading the chip was drawn from, so a popover of a board card for an
    // item the factory has no record of still knows the repository is watched.
    const matches = matchesFor(popover.key, { unrecorded: popover.unrecorded });
    const unreadable = unreadableFactories();
    if (!matches.length && !unreadable.length) {
      closePopover();
      return;
    }
    const closed = closedInDom(popover.anchor, popover.key);
    const body = cards(popover.name, matches, unreadable, {
      popover: true,
      // The chip the popover belongs to sits in the item's own row, so the row's
      // state mark answers for this item here too.
      closed,
    });
    // A message too tall for the popover scrolls inside it, and the stream
    // repaints every couple of seconds: the popover node is kept where it is,
    // so its scroll is too, and this is only for the frame that had to draw a
    // different one.
    const previous = popover.entry.shadow.querySelector(".ssf-popover");
    const scrolled = previous ? previous.scrollTop : 0;
    withWrites(body, matches, popover.key, closed);
    reconcile(popover.entry.shadow, [body]);
    if (body.scrollTop !== scrolled) body.scrollTop = scrolled;
    measure(popover.entry);
    placePopover();
  }

  /// Keep the popover next to its chip, inside the viewport on both axes. The
  /// CSS bounds its height to the viewport, so the clamp always has room.
  function placePopover() {
    if (!popover) return;
    const { host } = popover.entry;
    const anchor = popover.anchor.getBoundingClientRect();
    const box = host.getBoundingClientRect();
    const left = `${Math.max(8, Math.min(anchor.left, innerWidth - box.width - 8))}px`;
    const below = anchor.bottom + 6;
    const preferred = below + box.height + 8 > innerHeight
      ? anchor.top - box.height - 6
      : below;
    const top = `${Math.max(8, Math.min(preferred, innerHeight - box.height - 8))}px`;
    // The popover keeps its node across frames, so its place usually has not
    // moved either: writing it identically would be a page mutation on every
    // frame, and a page mutation is what drives the render this is inside of.
    if (host.style.left !== left) host.style.left = left;
    if (host.style.top !== top) host.style.top = top;
  }

  function closePopover() {
    if (!popover) return;
    popover.entry.host.remove();
    popover = null;
  }

  function popoverOpen(name) {
    return popover?.name === name;
  }

  /// Render what the current page and the latest snapshot call for, removing
  /// anything that no longer belongs. Idempotent: the same page and the same
  /// snapshot always leave the same nodes behind.
  function render() {
    if (rendering || !snapshot) return;
    rendering = true;
    try {
      const wanted = new Map();
      const page = DETAIL_PATH.exec(location.pathname);
      // The one item this page is a detail view of: the page's own item, or --
      // on a project board -- the item an open side panel is showing (#440).
      const detail = page
        ? { owner: page[1], repo: page[2], number: page[4], pull: page[3] === "pull" }
        : sidePanel();
      if (detail) {
        const key = `${detail.owner}/${detail.repo}#${detail.number}`;
        const target = detail.pull ? pullTarget(detail) : { key, label: null };
        if (target) {
          // An item's own page -- and a side panel, which is the same page
          // inside a board -- is where an item the factory has no record of is
          // picked up: it is one row, the reader is looking straight at it, and
          // the Assign form is the whole of what is offered here (#435).
          const matches = matchesFor(target.key, { unrecorded: true });
          const unreadable = unreadableFactories();
          if (matches.length || unreadable.length) {
            wanted.set(`card:${key}`, {
              matches,
              unreadable,
              target,
              // `key` names the page; `target.key` names the item the card is
              // about, which on a pull request page is the issue it resolves
              // through. The form assigns that item.
              key: target.key,
              anchor: null,
              // The page's own fact about its item, as a chip reads its row's:
              // a finished page reads Done and offers no Assign form, neither
              // for itself nor for the issue its body names (#462).
              closed: pageClosed(),
            });
          }
        }
      }
      const repoPage = REPO_PATH.exec(location.pathname);
      if (repoPage) {
        const repo = `${repoPage[1]}/${repoPage[2]}`;
        const watching = (snapshot?.factories ?? []).filter((factory) =>
          (factory.repositories ?? []).some((one) => sameRepo(one, repo)),
        );
        if (watching.length) {
          wanted.set(`scratch:${repo}`, { scratch: true, repo, factories: watching, anchor: null });
        }
      }
      if (!page && chipPage(location.pathname)) {
        // A board card stands for one item, and picking an item up is what a
        // board is for, so an item the factory watches but has no record of
        // carries a chip there too. A list or a search result gets a chip only
        // for an item that moves: its rows are read in bulk, and a watched
        // repository's whole backlog as a column of grey chips is noise.
        const unrecorded = boardPage(location.pathname);
        for (const [key, anchor] of listAnchors()) {
          const matches = matchesFor(key, { unrecorded });
          if (matches.length) {
            wanted.set(`chip:${key}`, {
              matches,
              anchor,
              // The page's own fact about this item, read where the chip goes:
              // the row's state mark says whether there is anything left to run.
              closed: closedInDom(anchor, key),
              unrecorded,
            });
          }
        }
      }
      for (const [name, entry] of injected) {
        const want = wanted.get(name);
        if (!want) {
          entry.host.remove();
          injected.delete(name);
          if (popoverOpen(name)) closePopover();
          continue;
        }
        if (!update(entry, { ...want, name })) {
          entry.host.remove();
          injected.delete(name);
          if (popoverOpen(name)) closePopover();
        }
      }
      for (const [name, want] of wanted) {
        if (injected.has(name)) continue;
        const entry = shadowHost(
          want.anchor === null ? "data-ssf-card" : "data-ssf-list",
          name,
        );
        if (want.anchor !== null) {
          entry.host.dataset.ssfKey = name.slice("chip:".length);
          entry.host.addEventListener("click", (event) => chipActivate(event, entry));
          entry.host.addEventListener("keydown", (event) => {
            if (event.key !== "Enter" && event.key !== " ") return;
            chipActivate(event, entry);
          });
        }
        if (update(entry, { ...want, name })) injected.set(name, entry);
      }
      if (popover) renderPopover();
    } finally {
      rendering = false;
    }
  }

  /// A click or keypress on a chip opens its popover, and never reaches
  /// GitHub's own row click.
  function chipActivate(event, entry) {
    const target = event.composedPath()[0];
    if (!(target instanceof Element) || !target.closest(".ssf-chip")) return;
    event.preventDefault();
    event.stopPropagation();
    const name = entry.host.getAttribute("data-ssf-list");
    if (popoverOpen(name)) closePopover();
    else {
      // The reading the chip was drawn from, so a board card's popover knows
      // the same item is assignable as its chip does (#435).
      openPopover(name, entry.host.dataset.ssfKey, entry.host, entry.unrecorded === true);
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
    // The form reads the same frames: a snapshot that shows the item it
    // assigned with an agent is what ends its "Assigning…".
    globalThis.ssfWrites?.applySnapshot(payload);
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

  // A popover belongs to the page it was opened on, and follows its chip while
  // the page scrolls; the first click outside it dismisses it.
  addEventListener("click", (event) => {
    if (!popover) return;
    const path = event.composedPath();
    if (path.includes(popover.entry.host) || path.includes(popover.anchor)) return;
    closePopover();
  });
  addEventListener("keydown", (event) => {
    if (event.key === "Escape") closePopover();
  });
  let repositioning = false;
  for (const event of ["scroll", "resize"]) {
    addEventListener(
      event,
      () => {
        if (!popover || repositioning) return;
        repositioning = true;
        requestAnimationFrame(() => {
          repositioning = false;
          placePopover();
        });
      },
      { passive: true, capture: true },
    );
  }

  // GitHub navigates client-side and re-renders its own React trees, so react
  // to its navigation events as well as to the DOM, and re-render rather than
  // render once.
  let lastPath = location.pathname;
  function navigated() {
    if (location.pathname !== lastPath) {
      lastPath = location.pathname;
      closePopover();
      for (const entry of injected.values()) entry.host.remove();
      injected.clear();
      // What the reader had opened belongs to the page they left, and so does
      // what the last layout pass found.
      opened.clear();
      overflowed.clear();
    }
    scheduleRender();
  }
  for (const event of ["popstate", "turbo:load", "pjax:end", "soft-nav:end"]) {
    addEventListener(event, navigated);
  }
  const observer = new MutationObserver(navigated);
  observer.observe(document.documentElement, { childList: true, subtree: true });

  // The form draws itself again through this script's own scheduler, so the
  // two never render over each other.
  globalThis.ssfWrites?.onChange(scheduleRender);

  chrome.runtime.sendMessage({ type: "ssf:snapshot" }).then(apply, () => {});

  connect();
})();

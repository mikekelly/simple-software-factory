# Session dashboard

Run a live dashboard in any ordinary terminal:

```sh
ssf dashboard
ssf --server customer@cloud.example dashboard
ssf --server factory-one --server factory-two dashboard
SSF_SERVER=customer@cloud.example ssf dashboard
```

The supported Linux client includes the TUI. The macOS client is work in
progress. No browser, Python, Omarchy or Herdr
installation is required. Local factories, local VMs and remote factories use
the same long-running `ssf status --json --watch` stream. Configure SSH
authentication first; a remote dashboard keeps one noninteractive SSH channel
open for its lifetime rather than starting a connection on every refresh. Repeat
`--server` to group several factories in one dashboard. Each server has its own
watch stream, heading, freshness and error state, so one failed connection does
not make the others look unavailable. `SSF_SERVER` remains the single-server
selection when no explicit destination is supplied. When a client-side
[`servers.toml`](configuration.md#server-catalog) exists, these values are
configured names; without one they retain their legacy meaning as SSH
destinations. With no `--server` or `SSF_SERVER` selection, the dashboard connects
to all catalog entries automatically. An absent or empty catalog uses the local
endpoint.

The Ratatui dashboard stays open and refreshes automatically. Agent cards use
one, two or three columns as terminal width permits and fall back to a compact
list in very small panes. Use arrows or `h`/`j`/`k`/`l` to select a card,
Page Up/Page Down to page, Enter to focus its agent in Herdr, and `q` or Ctrl-C to quit.
Mouse selection and wheel scrolling work in terminals with mouse reporting.
The terminal's normal screen and input mode are restored on exit. Issue IDs
are hyperlinks in terminals supporting OSC8 links; use your terminal's normal
modifier-click gesture to open them.

Cards are created only when the session driver reports a live agent. Open items
that SSF still monitors but which have no agent are listed separately as
"Monitored without an agent"; issue monitoring state is never treated as agent
presence. Cards show the originating issue, additional active assigned issues,
agent state, last activity and latest message or summary. They use the server's
canonical ownership model; missing activity is shown as unknown. Connection
errors retain the last successful cards with an explicit stale-state warning.
VM, driver, inactive service and stale daemon states are shown as problems, not as an
empty healthy factory. Update the server too when upgrading from the old
browser-based client: the TUI requires its canonical dashboard status fields.

## Optional Herdr navigation

Run the same command in any Herdr pane, tab or workspace. Selecting an agent
matches its SSF `agent_session_id` to Herdr's agent session and focuses its
pane through `herdr agent focus`. The dashboard remains alive in its original
pane; return using Herdr's normal navigation.

Navigation is scoped to the **Herdr server where the dashboard runs**, including
other tabs and workspaces on that server. `--server` selects the SSF factory,
not a different Herdr server. A dashboard on your laptop cannot focus a pane
on a remote Herdr server just because it reads remote SSF status. Run it in a
pane on that Herdr server when pane navigation is wanted. Missing context,
unmatched conversations and stale panes show a message and leave the dashboard
usable. The [Herdr plugin](../herdr-plugin/README.md) is only a launch shortcut.

On Omarchy, the Factory menu's **Dashboard** entry opens the TUI in a terminal.

## Optional server web dashboard

The browser dashboard belongs to `ssf-server` and is **off by default**. Enable
it in that server's configuration, then restart the server:

```toml
[dashboard]
enabled = true
bind = "127.0.0.1"
port = 8787
```

The same settings can be set with `ssf config set dashboard.enabled true`,
`ssf config set dashboard.bind 127.0.0.1`, and
`ssf config set dashboard.port 8787`. In VM mode they remain on the host;
the host endpoint uses normal forwarding to read guest status. Changes to
these listener settings require a restart. `--once` does not serve the UI.

The server logs a capability URL such as `http://127.0.0.1:8787/<secret>/`.
Open that URL yourself on the server machine, or through your configured proxy.
The server never opens a browser. Keep the full URL private: it grants access
to repository details and session summaries. It changes on server restart.
The listener remains available until the server stops, even with no browser
open. An enabled listener that cannot bind causes an explicit startup error.

Only loopback addresses (including `::1`) and Tailscale addresses (IPv4 in
`100.64.0.0/10`, IPv6 in `fd7a:115c:a1e0::/48`) are accepted. Direct binds to a
LAN address, a public address, `0.0.0.0` or `::` are refused. `ssf doctor`
reports which of the two an enabled bind is.

When bound to a Tailscale address the transport is plain HTTP, encrypted by
WireGuard between tailnet devices and by nothing else. Paste the capability URL
only into the SSF Chrome extension or a browser on the tailnet, and never expose
the port through [Tailscale
Funnel](https://tailscale.com/kb/1223/funnel) or a router port forward.
Restrict who can reach the port with tailnet ACLs; anyone who can reach it and
learns the capability URL has the same access you do.

### Status API

Five endpoints under the capability path serve the canonical dashboard model:

- `GET /<capability>/api/status` returns the current snapshot as JSON, or a
  `502` with `{"error": ...}` when the status stream cannot be read.
- `GET /<capability>/api/events` is a [server-sent
  events](https://developer.mozilla.org/docs/Web/API/Server-sent_events) stream.
  It sends the current snapshot immediately as an `event: status` frame whose
  `data` is the same JSON `/api/status` returns, then another `status` frame for
  every snapshot the server's status stream produces, an `event: error` frame
  with `{"error": ...}` when a snapshot cannot be loaded, and a `: keepalive`
  comment line every 25 seconds while no snapshot arrives. The connection stays
  open until the client or the server closes it.

  Frames follow the status stream rather than dashboard changes: the stream
  publishes a fresh snapshot about every 2 seconds, `ssf status --json --watch`'s
  own cadence, so a frame means the snapshot was refreshed and only `refreshed_at`
  is guaranteed to differ. The keepalive covers a stream that has gone quiet,
  which is a stalled or disconnected status source rather than a dashboard that
  happens to be unchanged.
- `GET /<capability>/api/agents` returns what `ssf agents` lists, in the shape
  `ssf agents --json` prints: one object per harness ssf knows, with its `id`,
  display `name`, whether it is `installed` on the factory, whether it is the
  Omarchy `default`, the model ids ssf knows for it, and the effort levels it
  takes. This is the factory's own answer, so it names what is installed where
  the sessions run.
- `GET /<capability>/api/models/<harness>` returns what `ssf models <harness>
  --json` prints: the harness's `models` in the order their source lists them,
  and `source`, saying whether the harness's own catalogue file, its listing
  command or ssf's built-in table answered. A harness ssf does not know is
  `404`; one that takes no model setting (see `ssf agents`) is `400` with that
  reason, since the request's shape is not what is wrong with it.
- `POST /<capability>/api/assign`, `POST /<capability>/api/handover`,
  `POST /<capability>/api/release` and `POST /<capability>/api/message` run one
  `ssf` command each for one item and return its result. See
  [Writing from the API](#writing-from-the-api).

Each card in `/api/status` and `/api/events` carries, besides the fields the
dashboard and TUI render: `tool`, the session's current tool call formatted as
`ssf status --json` formats it (`null` when the agent is not making one),
`branch`, the workspace's branch (`null` when the item has no workspace), and
`factory`, the name the server answers for — its catalog target name when it was
started as one (`ssf-server --target …`), else its hostname. A client holding
several factories can then label a card without asking which stream it came
from; the TUI and the server's own browser page ignore the three fields and are
unchanged. `effort` is the level the card's stack is on (`harness` and `model`
are beside it), so a client that offers to move a session to another stack can
prefill the picker with what it is on now; it is empty while `next_launch` is
set, since the running session was launched with a stack ssf does not have on
the record.

Each item in `dashboard.monitored_items`, and each card's `origin` and
`additional` items, carries `has_workspace`: whether ssf has a workspace recorded
for that item, with `branch` when the driver reports one. An item that has one is
not one `ssf assign` accepts — it is refused, because `ssf release` is what frees
it — so a client that offers the write draws no form for it.

`dashboard.repositories` lists the repositories the factory watches, as
`owner/name`. A factory watches a repository rather than the items in it, so its
cards and monitored items say nothing about an item nobody has assigned yet —
and without this list a client cannot tell such an item, which takes a session,
from one the factory has never heard of, which is refused. The extension reads
it to draw the Assign form for an item in a watched repository that has no card
and is not monitored (#435). A server that does not publish it sends an empty
list, and a client that reads it then offers the form only for items it has a
record of, as before.

The read endpoints accept an `Origin` of `http://<bind>:<port>` or any
`chrome-extension://...` origin, so a Chrome extension's service worker can read
them; the `Host` header must still match the configured bind address and port.

Everything above the status stream is answered by *the factory*, through the
same client the TUI and the commands use: with `[vm] enabled` the listener is
bound by the host that supervises the VM while the daemon, the harnesses and
their model catalogues are in the guest, so the agent and model listings, and
every write, are asked there and forwarded rather than answered from the host. A
factory the host cannot reach — a VM that is not running, a stopped daemon — is
a `502` naming what could not be reached, never a listing of the host's own
harnesses.

#### Writing from the API

Four POST routes, each one `ssf` command for one item, named as the repository
and the number. Their bodies are the command's own arguments; a field a route
does not take is refused rather than ignored, so a misspelled one cannot ask for
something nobody meant.

```json
POST /<capability>/api/assign   {"repo": "owner/name", "number": 42, "harness": "claude", "model": "opus", "effort": "low"}
POST /<capability>/api/handover {"repo": "owner/name", "number": 42, "harness": "omp", "model": "deepseek/deepseek-flash", "effort": "high", "note": "carry on from here"}
POST /<capability>/api/release  {"repo": "owner/name", "number": 42}
POST /<capability>/api/message  {"repo": "owner/name", "number": 42, "text": "the test is red again"}
```

- **assign** starts a session on an item that has none: the bot is assigned on
  GitHub and the item's stack is written in the same request, exactly as
  `ssf assign` does it.
- **handover** moves the item's running session to another stack. `model` and
  `effort` are optional and default to what the harness itself uses; `note` is
  optional and is the summary the new session reads before the item's story
  (`ssf handover --summary`), checked the same way — at most 8,000 characters,
  and refused if it would read as a harness's sign-in screen, since it is pasted
  into the new session's terminal.
- **release** removes the item's session workspace, never forced: the workspace
  checks are the point of doing it from a browser, and a person who has looked
  at the workspace passes `--force` at a shell.
- **message** delivers `text` to the agent that acts on the item the way the
  item's own activity does — the daemon's delivery path, which brings a gone
  workspace and agent back first and holds a prompt for a session that is at its
  sign-in prompt. `text` is capped at 2 KiB here, counted in bytes so that a
  message the cap accepts always fits the request that carries it; anything
  longer belongs on the item as a comment.

Each answers with the same JSON its command prints under `--json`. The status
says what happened:

- `200` with that result.
- `400` with `{"error": ...}` when the request itself is at fault: a repository
  this factory does not watch, a harness ssf does not know, a model or effort
  that harness does not take, a message that is empty or over the cap, a summary
  that is empty or too long, or a body that is not the JSON above.
- `409` with `{"error": ...}` when the item is not in a state the write can be
  applied to — for assign, an item that already has a session, a handover or
  release pending on it, or one worked by another item's session; for handover,
  an item with no running session or one already on that stack; for release, a
  workspace that holds work that is not on origin, an item that still owns open
  ones, or one with no workspace left; for message, an item with no agent, or a
  session that is blocked at its harness's sign-in prompt.
- `502` with `{"error": ...}` when the factory could not do it: it could not be
  reached, the write took longer than a minute, or the harness is not installed
  or not signed in where the sessions run.

The `error` is the message the matching command would have printed, verbatim.
A release the workspace refuses is one of those: the daemon answers it as a
result rather than an error, and the endpoint words it exactly as `ssf release`
does, one check per line.

#### Write rules

Reads answer on the rules above. The writes are held to stricter ones, all of
them decided from the request line and headers before any body is read, and each
refused without anything having been done:

- The `Origin` must begin `chrome-extension://`. The bind host's own origin is
  refused for a write, so a page loaded from a browser on the tailnet cannot
  start a session even if it learns the capability URL; an ordinary HTTP client
  with no `Origin` is refused too (`403`).
- `Content-Type` must be `application/json` (`415`).
- The body must be at most 4 KiB, by its `Content-Length` (`413`); a chunked
  body is refused outright (`400`), and so is one with two lengths.
- The path must be one of the four write routes: `POST` anywhere else under the
  capability is `405`, and the write routes are not readable (`404`).

Every accepted write is logged at info with the origin and the item, so acting
on a session from a browser is visible in the server's journal.

The optional [Chrome extension](../chrome-extension/README.md) is the client
these endpoints are for: it overlays this state on github.com instead of in a
browser tab. It collapses every state ssf and the harnesses report onto five —
**Working**, **Waiting on you**, **Done**, **Problem** and **No agent** — each
with a fixed colour and icon, and shows the raw state the TUI prints on hover
(the one reading ssf did not make is *no record*, for an item in a watched
repository that has no card and is not monitored, which its tooltip says in
those words). An
issue or pull request page gets a card in the right sidebar above Assignees (a
pull request resolves through the issue its body closes, `Closes` before
`Refs`); lists, search results and project boards get one chip per tracked item —
a board also chipping one the factory has no record of, when it watches that
repository, since a board card is where an item is picked up — whose click opens
the same card as a popover. The card's `Details` section
carries the current tool call, the factory label, the workspace branch and the
agent session id. A factory whose stream has dropped, or which flags its own
snapshot as unreliable, is shown with the icon outlined and the time as "as of
HH:MM", never as a solid live state — the same distinction the TUI draws between
a live factory and an unavailable or stale snapshot.

An item in the **No agent** state carries an **Assign agent** form, on the card
and in the popover: harness from `api/agents`, model from
`api/models/<harness>`, effort from the levels that harness takes, and Assign.
That includes an item no factory has a record of — in none of its cards and in
none of its monitored items — when the factory watches the item's repository:
`dashboard.repositories` is what tells the two apart, and the form is drawn on
the item's own page and on its project board card. An item that already has a
workspace is refused by `ssf assign` (`ssf release` is what frees it), so its
card carries *Has a workspace; release it first* instead of a form, which is
what `has_workspace` above is published for.
An item whose state is **Working**, **Waiting on you**, **Done** or **Problem**
carries an **Actions** row on the card of the factory that has it — *Message* (a
textarea and Send), *Hand over…* (the assign pickers, prefilled with the stack
the card is on, plus a note) and *Release* (a confirm step naming the workspace
branch, which is also what frees an item the note above stands on). One row per
factory with an agent, so two factories working one item are two sessions to act
on, each through its own factory. Every one of them is sent from the
service worker and never from the content script, so the Origin is the
extension's; each configured factory has a **Writes** switch on its options page,
on by default, which hides both and refuses the write when off. A refusal is
shown in the daemon's own words with the form kept, and nothing is retried. That
page also names the version the browser is running and what each factory reports
— whether it answered, and how many watched repositories — so a form that is
missing because the loaded extension or the running server is older than the
change is answered there rather than guessed at
([the guide's table](../chrome-extension/README.md#updating-a-loaded-copy)).

| Message | Hand over… | Release |
| --- | --- | --- |
| ![An item's card with the Actions row, a message box and Send](../chrome-extension/docs/actions.png) | ![The hand-over step, prefilled with the item's stack and a note box](../chrome-extension/docs/action-handover.png) | ![The release confirm naming the item and its branch](../chrome-extension/docs/action-release.png) |
| ![The box after Send, reading "sent"](../chrome-extension/docs/action-message.png) | ![A hand-over refused: already on that stack, with the pickers kept](../chrome-extension/docs/action-handover-refused.png) | ![A release refused by the workspace's own checks, verbatim](../chrome-extension/docs/action-release-refused.png) |

The [extension's own guide](../chrome-extension/README.md#acting-on-an-agent)
has the full set, including the accepted hand-over and release results.

A chip on an item the page itself shows closed or merged renders a muted **Done**
rather than a red **Problem**: a released workspace reads `no-workspace` and an
item the daemon no longer tracks reads `unbound`, neither of which is a fault of
a finished item. The raw state word stays in the tooltip, a chip's popover agrees
with the chip, and a state the overlay does not read as Problem or No agent is
shown as the factory reports it either way.

The built-in endpoint provides neither TLS nor user accounts. Remote web access
from outside a tailnet requires a reverse proxy that:

- Terminates TLS and authenticates and authorizes each user before forwarding.
- Keeps the upstream loopback-only and preserves the capability path.
- Validates the external Host and Origin, then rewrites Host to the configured
  upstream address and port and rewrites/removes Origin. The server rejects
  foreign Host/Origin values; do not blindly discard these checks on an
  unauthenticated proxy.
- Avoids logging or sharing the capability path and does not cache responses.

For a tailnet, [Tailscale Serve](https://tailscale.com/docs/reference/tailscale-cli/serve)
can provide HTTPS access restricted by tailnet policy. Put a correctly configured
local proxy in front of SSF for the Host/Origin handling above and restrict the
allowed users/devices; do not expose this endpoint through public Funnel.
A future SSF Cloud deployment needs per-user authorization and tenant isolation
at its authenticated gateway; this capability URL is not a multi-tenant
identity system.

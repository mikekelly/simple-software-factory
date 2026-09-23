# Session dashboard

For the agent or operator who wants to watch a factory's sessions live, or to
build a client against the factory's HTTP API.

There are two dashboards: a terminal UI that ships with the client and needs
nothing else installed, and an optional HTTP listener on the server that a
browser or a browser extension can read.

## Terminal dashboard

```sh
ssf dashboard
ssf --server ssf-server dashboard
ssf --server factory-one --server factory-two dashboard
SSF_SERVER=ssf-server ssf dashboard
```

A good result is a full-screen dashboard that stays open and refreshes itself.

`--server` selects a configured entry from the
[server catalog](configuration.md#server-catalog); without a catalog it is an
SSH destination. Repeat it to group several factories in one dashboard: each
gets its own watch stream, heading, freshness and error state, so one failed
connection does not make the others look unavailable. `SSF_SERVER` is the
single-server selection when no destination is given. With neither, the
dashboard connects to every catalog entry; an absent or empty catalog uses the
local endpoint.

Local factories, local VMs and remote factories all read the same long-running
`ssf status --json --watch` stream. Configure SSH authentication before pointing
the dashboard at a remote factory: it keeps one noninteractive SSH channel open
for the dashboard's lifetime rather than connecting on every refresh.

### Keys and layout

| Key | Effect |
| --- | --- |
| arrows, `h` `j` `k` `l` | select a card |
| Page Up, Page Down | page through cards |
| Enter | focus that agent in Herdr |
| `q`, Ctrl-C | quit |

Agent cards use one, two or three columns as terminal width permits and fall
back to a compact list in very small panes. Mouse selection and wheel scrolling
work in terminals with mouse reporting. Issue IDs are hyperlinks in terminals
that support OSC 8; use the terminal's normal modifier-click gesture. The
terminal's normal screen and input mode are restored on exit.

### What the cards mean

A card exists only when the session driver reports a live agent. Open items the
factory still monitors but which have no agent are listed separately as
"Monitored without an agent": monitoring state is never treated as agent
presence.

Each card shows the originating issue, any additional active assigned issues,
agent state, the stack the session runs (harness, model and effort), last
activity and the latest message or summary, using the server's ownership
model. Where there is no activity time the card says why (see
`activity_note` below) rather than "unknown". On a connection
error the last successful cards are kept with an explicit stale-state warning.
VM, driver, inactive service and stale daemon states are shown as problems
rather than as an empty healthy factory.

### Herdr navigation

Run the same command in any Herdr pane, tab or workspace. Selecting an agent
matches its SSF `agent_session_id` to Herdr's agent session and focuses that
pane through `herdr agent focus`. The dashboard stays alive in its original
pane; return with Herdr's normal navigation.

Navigation is scoped to the Herdr server the dashboard runs on, including other
tabs and workspaces there. `--server` selects the SSF factory, not a different
Herdr server: a dashboard on a laptop cannot focus a pane on a remote Herdr
server just because it reads that factory's status. Run it in a pane on that
Herdr server when pane navigation is wanted. Missing context, unmatched
conversations and stale panes show a message and leave the dashboard usable.
The [Herdr plugin](../herdr-plugin/README.md) is only a launch shortcut.

Desktop menu entries that launch the dashboard, where a desktop provides them,
are covered in
[platform specifics](platform-specifics.md#arch-linux-and-omarchy).

## Optional server web dashboard

The HTTP listener belongs to the factory's server process and is off by
default. Enable it in that server's configuration and restart the server:

```toml
[dashboard]
enabled = true
bind = "127.0.0.1"
port = 8787
```

The same keys can be set with `ssf config set dashboard.enabled true`,
`ssf config set dashboard.bind 127.0.0.1` and
`ssf config set dashboard.port 8787`. In VM mode the listener settings stay on
the host, which forwards to read guest status. Changes to them require a
restart. `--once` does not serve the UI.

Setting it up, in order:

1. Enable the listener and restart the server. Copy the capability URL it logs.
2. Install the [Chrome extension](../chrome-extension/README.md) and add that
   URL there. Do it once: the secret is kept, so the URL survives every later
   restart.
3. Decide the exposure. The default loopback bind serves the server machine
   alone; a Tailscale bind serves the tailnet; anything else needs an
   authenticating TLS reverse proxy in front, per the rules below.

The server logs a capability URL such as `http://127.0.0.1:8787/<secret>/`.
Open it yourself on the server machine or through your proxy; the server never
opens a browser. Keep the whole URL private: it grants access to repository
details and session summaries. The listener stays up until the server stops,
even with no browser open. An enabled bind that cannot be bound is an explicit
startup error.

The secret is generated once, on the first start that serves the listener, and
kept at `dashboard-token` in the factory's state directory
(`~/.local/state/ssf/dashboard-token`, mode 0600; a catalog target has its own
state directory). Every later start reads it, so a URL someone has already
configured keeps working across restarts — including one saved in the Chrome
extension. A file this build did not write, made by hand or restored from a
backup, is tightened to 0600 when it is read.

Deleting that file and restarting generates a new one and invalidates every
saved copy of the old URL. `ssf uninstall --data` removes it with the default
factory's state; a catalog target's own directory is removed by hand, since
[`ssf uninstall` is not target-aware](uninstall.md#recovery-cases).

The dashboard's own file never stops the factory: a secret that cannot be read
or stored is a warning naming the path, not a startup error. A file that cannot
be read is left as it is, and that run serves a fresh URL; one that cannot be
stored means the URL will not survive a restart. Either way the log says so and
the log line below carries the URL actually being served.

The startup log line carries the whole URL on every start, so
`journalctl --user -u ssf.service | grep 'Server web dashboard'` finds it (the
unit is `ssf@NAME.service` for a named target).

### Bind rules

Only loopback addresses (including `::1`) and Tailscale addresses (IPv4 in
`100.64.0.0/10`, IPv6 in `fd7a:115c:a1e0::/48`) are accepted. A direct bind to a
LAN address, a public address, `0.0.0.0` or `::` is refused when the config is
read. `ssf doctor` reports which of the two an enabled bind is.

On a Tailscale address the transport is plain HTTP, encrypted by WireGuard
between tailnet devices and by nothing else. Paste the capability URL only into
a client on the tailnet, never expose the port through Tailscale Funnel or a
router port forward, and restrict who can reach it with tailnet ACLs: anyone who
can reach the port and learns the URL has the same access you do. See
[platform specifics](platform-specifics.md#tailscale).

The listener provides neither TLS nor user accounts. Remote access from outside
a tailnet needs a reverse proxy that:

- terminates TLS and authenticates and authorizes each user before forwarding;
- keeps the upstream loopback-only and preserves the capability path;
- validates the external Host and Origin, then rewrites Host to the configured
  upstream address and port and rewrites or removes Origin (the server rejects
  foreign Host and Origin values; do not discard those checks on an
  unauthenticated proxy);
- does not log, share or cache the capability path or the responses.

For a tailnet, Tailscale Serve can provide HTTPS restricted by tailnet policy,
still with a correctly configured local proxy in front for the Host and Origin
handling above.

## HTTP API

Everything below is served under the capability path. Four read endpoints and
three write endpoints make up the API; the capability root also serves the
server's own browser page and its CSS and JavaScript.

### Read endpoints

| Endpoint | Returns |
| --- | --- |
| `GET /<capability>/api/status` | the current snapshot as JSON, or `502` with `{"error": ...}` when the status stream cannot be read |
| `GET /<capability>/api/events` | a server-sent events stream of the same snapshots |
| `GET /<capability>/api/agents` | what `ssf agents --json` prints |
| `GET /<capability>/api/models/<harness>` | what `ssf models <harness> --json` prints |

`api/events` sends the current snapshot immediately as an `event: status` frame
whose `data` is the JSON `api/status` returns, then another `status` frame for
every snapshot the status stream produces, an `event: error` frame with
`{"error": ...}` when a snapshot cannot be loaded, and a `: keepalive` comment
every 25 seconds while no snapshot arrives. The connection stays open until
either side closes it. Frames follow the status stream, not dashboard changes:
the stream publishes about every 2 seconds, `ssf status --json --watch`'s own
cadence, so a frame means the snapshot was refreshed and only `refreshed_at` is
guaranteed to differ. A keepalive means the stream has gone quiet, which is a
stalled or disconnected status source rather than an unchanged dashboard.

`api/agents` returns one object per harness the factory knows, with its `id`,
display `name`, whether it is `installed`, whether it is the desktop `default`,
the model ids known for it and the effort levels it takes. `api/models` returns
the harness's `models` in the order its source lists them plus `source`, saying
whether the harness's own catalogue file, its listing command or ssf's built-in
table answered. A harness ssf does not know is `404`; one that takes no model
setting is `400` with that reason, since the request's shape is not what is
wrong with it.

### Snapshot fields a client can rely on

Besides the fields the TUI and the server's page render, each card carries:

| Field | Meaning |
| --- | --- |
| `tool` | the session's current tool call, formatted as `ssf status --json` formats it; `null` when the agent is not making one |
| `branch` | the workspace's branch; `null` when the item has no workspace |
| `factory` | the name the server answers for: its catalog target name when started as one (`ssf-server --target ...`), else its hostname |
| `effort` | the level the card's stack is on, beside `harness` and `model`; empty while `next_launch` is set, since the running session was launched with a stack ssf does not have on record |
| `worktree_path` | the workspace the session runs in; `null` when the item has none |
| `handover` | the hand-over waiting on the daemon for the item (`harness`, `model`, `effort`, `summary_chars`, `by`, `requested_at`), or `null` |
| `activity_note` | why `last_activity_at` is null: the harness keeps no local transcript ssf can read (`omp`), the session's conversation is not identified yet, or ssf has not found its transcript yet. Clients show that sentence where the time would be |

A client holding several factories can label a card without asking which stream
it came from.

Each item in `dashboard.monitored_items`, and each card's `origin` and
`additional` items, carries `has_workspace`, with `branch` when the driver
reports one. An item that has a workspace is not one `ssf assign` accepts:
`ssf release` is what frees it, so a client draws no assign form for it.

`dashboard.repositories` lists the repositories the factory watches, as
`owner/name`. A factory watches a repository rather than the items in it, so
its cards and monitored items say nothing about an item nobody has assigned
yet. This list is what lets a client tell such an item, which can take a
session, from one the factory has never heard of, which is refused. A server
that does not publish it sends an empty list.

The read endpoints accept an `Origin` of `http://<bind>:<port>` or any
`chrome-extension://...` origin, so an extension's service worker can read
them; the `Host` header must still match the configured bind address and port.

Everything above the status stream is answered by the factory, through the same
client the TUI and the commands use. With `[vm] enabled` the listener is bound
by the host that supervises the VM while the daemon, the harnesses and their
model catalogues are in the guest, so the agent and model listings, and every
write, are asked there and forwarded rather than answered from the host. A
factory the host cannot reach (a VM that is not running, a stopped daemon) is a
`502` naming what could not be reached, never a listing of the host's own
harnesses.

### Write endpoints

Three POST routes, each one `ssf` command for one item, named by repository and
number. Their bodies are the command's own arguments; a field a route does not
take is refused rather than ignored, so a misspelled one cannot ask for
something nobody meant.

```json
POST /<capability>/api/assign   {"repo": "owner/name", "number": 42, "harness": "claude", "model": "opus", "effort": "low"}
POST /<capability>/api/handover {"repo": "owner/name", "number": 42, "harness": "omp", "model": "deepseek/deepseek-flash", "effort": "high", "note": "carry on from here"}
POST /<capability>/api/release  {"repo": "owner/name", "number": 42}
```

- **assign** starts a session on an item that has none: the bot is assigned on
  GitHub and the item's stack is written in the same request, exactly as
  `ssf assign` does it.
- **handover** moves the item's running session to another stack. `model` and
  `effort` are optional and default to what the harness itself uses. `note` is
  optional and is the summary the new session reads before the item's story
  (`ssf handover --summary`), checked the same way: at most 8,000 characters,
  and refused if it would read as a harness's sign-in screen, since it is
  pasted into the new session's terminal.
- **release** removes the item's session workspace, never forced: the workspace
  checks are the point of doing it from a browser, and a person who has looked
  at the workspace passes `--force` at a shell.

There is no route that types at an agent. A person speaks to one by commenting
on the item, and the exchange stays on the item where everyone working it can
read it.

Each answers with the same JSON its command prints under `--json`:

| Status | Meaning |
| --- | --- |
| `200` | the command's result |
| `400` | the request itself is at fault: an unwatched repository, an unknown harness, a model or effort that harness does not take, an empty or oversized summary, or a body that is not the JSON above |
| `409` | the item is not in a state the write applies to: for assign, an item that already has a session, a pending handover or release, or one worked by another item's session; for handover, no running session or one already on that stack; for release, a workspace holding work that is not on origin, an item that still owns open ones, or one with no workspace left |
| `502` | the factory could not do it: unreachable, the write took longer than a minute, or the harness is not installed or not signed in where the sessions run |

The `error` is the message the matching command would have printed, verbatim.
A release the workspace refuses is one of those: the daemon answers it as a
result rather than an error, and the endpoint words it exactly as `ssf release`
does, one check per line.

### Write rules

Reads answer on the rules above. Writes are held to stricter ones, all decided
from the request line and headers before any body is read, and each refused
without anything having been done:

- The `Origin` must begin `chrome-extension://`. The bind host's own origin is
  refused for a write, so a page loaded from a browser on the tailnet cannot
  start a session even if it learns the capability URL; an ordinary HTTP client
  with no `Origin` is refused too (`403`).
- `Content-Type` must be `application/json` (`415`).
- The body must be at most 4 KiB by its `Content-Length` (`413`); a chunked
  body is refused outright (`400`), and so is one with two lengths.
- The path must be one of the three write routes: `POST` anywhere else under the
  capability is `405`, and the write routes are not readable (`404`).

Every accepted write is logged at info with the origin and the item, so acting
on a session from a browser is visible in the server's journal.

## Chrome extension

The optional [Chrome extension](../chrome-extension/README.md) is the client
these endpoints are for: it overlays session state on github.com instead of in
a browser tab, and offers assign, hand-over and release through the
write routes. Its own guide covers installation, the states it draws and the
options page.

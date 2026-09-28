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
agent state, the stack the session runs (harness, model and effort) and last
activity, using the server's ownership model. The terminal dashboard also shows
the latest message or summary. The web page's card leads with the item's
reference (`owner/repo#N`) as a link and its title below, then lists the items
the session **Owns** and the items it is **Following** (`ssf sub`) with the
level each is followed at (`state` or `all`), each with an issue or pull
request icon; a list with nothing in it is not shown. Where there is no activity time the card says why (see
`activity_note` below) rather than "unknown". On a connection
error the last successful cards are kept with an explicit stale-state warning.
An unreachable VM, an unavailable driver, a daemon that is not answering and
an overdue poll are shown as problems rather than as an empty healthy factory.

### What the warning banner means

The banner is about the *factory*, not about whatever supervises it. It is
drawn from whether a daemon answers on the factory's Unix socket
(`daemon_reachable` in `ssf status --json`), so a factory is treated as active
whenever the daemon is reachable — however it was started:

| Daemon | Service unit | Banner |
|---|---|---|
| answering | active | none |
| answering | stopped or disabled | none |
| not answering | stopped or disabled | `SSF service is inactive; showing latest saved state` |
| not answering | active | `SSF daemon is not answering; showing latest saved state` |
| answering, last poll overdue | either | `SSF daemon state is stale; last successful poll is overdue` |

`service_active` and `service_enabled` are still in the top level of
`ssf status --json`, as the service manager's own view of its unit — the HTTP
API's `api/status` serves the `dashboard` presentation alone (plus `build`,
the version and commit serving it, which the page's header shows, and
`terminal_input`, whether this page may open an item's terminal), so they are
not part of it — and `ssf ui service status` reports exactly that. They say
nothing about a daemon started outside the unit, which is the supported shape
for
[containers, other supervisors and foreground `ssf-server`](#running-without-systemd).

### Running without systemd

`ssf-server` is the factory; the service unit is one way to keep it running.
A host with no user systemd — a pod-style container, a machine where
`systemctl --user` cannot manage units — runs the same factory by starting the
daemon directly, and everything the dashboards read comes from that process:

```sh
/usr/bin/ssf-server            # foreground, or under the host's supervisor
```

`ssf setup` and `ssf ui service enable` need a service manager and are what
such a host skips; see
[host mode with standalone binaries](install.md#33-standalone-binaries-on-a-rented-host).
`ssf status` and `ssf doctor` then report the daemon rather than the unit —
`service: running (started outside ssf.service, which is stopped and disabled)`
— and the dashboard draws no warning at all.

### Herdr navigation

On a host ssf's agents live in the herdr session `ssf`, so run the dashboard in a pane of that session (`herdr session attach ssf`); in the VM guest, any pane of its herdr. The dashboard can be in any tab or workspace there. Selecting an agent
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
factory's state; a namespaced local target's own directory is removed by hand, since
[`ssf uninstall` refuses one](uninstall.md#recovery-cases).

The dashboard's own file never stops the factory: a secret that cannot be read
or stored is a warning naming the path, not a startup error. A file that cannot
be read is left as it is, and that run serves a fresh URL; one that cannot be
stored means the URL will not survive a restart. Either way the log says so and
the log line below carries the URL actually being served.

### Live terminal for an item's pane

Where this page may type into an item's pane, its card has an **Open
terminal** button. It opens the session's agent pane as a live terminal
(xterm.js) in a panel beside the cards, streamed from herdr as the pane draws
it. Panels can be tabbed, split and dragged, and the layout is kept per
browser; the **Agents** panel of cards has no close control, so it keeps its
place (and a layout restored without it gets it back). Nothing is
redacted: anyone who opens it sees the pane's output as it is, GitHub tokens
included.

The terminal is a full terminal: once you take control, keys, paste and the
wheel all reach the pane.
The link shows, and the server opens the terminal, only where both of these are
on:

- `dashboard.terminal_input` on the server (default `false`), which lets this
  page type at all. Otherwise the page follows the [write rules](#write-rules),
  which accept a terminal only from the Chrome extension's origin.
- `item_pane_input` for the item's repository (default `false`), decided where
  the factory's configuration is (in the guest, for a factory in a VM). Where
  it is off, the terminal closes with the factory's reason.

herdr lets one client control a pane, so the server holds **one control
stream per pane** and shares it among everyone who has the terminal open, from
this page or the extension, like a shared tmux session. Everyone sees the
same screen, but **one viewer at a time holds control**: only its typing,
paste and wheel reach the pane, and the pane takes its window's size. Everyone
else is view-only: they see the pane at that size, the font shrunk to fit
(13px down to 4px), the server drops what they type, and their title bar has
**Take control**, which makes them the controller (the pane takes their size)
and turns the previous controller view-only, with the button back. Every
terminal opens view-only, even on a pane no one controls: control is only
ever taken with Take control. When the controller leaves, no one holds it
until someone presses Take control, and the pane keeps its size meanwhile. A viewer who joins
late is sent the whole screen. The title bar says who is in control and lists
who is watching (`@login` for the extension, `dashboard` for this page); the
names are for display only.

Each wheel notch scrolls the pane's history, or the app in a full-screen TUI
that uses the mouse. Shift+Enter sends a newline that does not submit, and a
paste is always sent as a bracketed paste. A page with nothing typed for 30
minutes disconnects and offers **Reconnect**. The stream, and so the pane, is
released when the last viewer leaves, which gives the pane back at its own
size. It never takes a pane over: while someone else (a herdr client, say)
controls it, the page says so and tries again.

When the pane goes away (the harness exited, or it was relaunched in a new pane),
or someone else holds it, the terminal tries it again for about a minute and
carries on. A scratch session's pane is the same kind of terminal (#565),
opened by the extension; it always takes typing, whatever `item_pane_input`
says. One still running in tmux from before scratch sessions moved to herdr
has no terminal until it is next started.

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

Everything below is served under the capability path. The read endpoints and
the write endpoints below make up the API; the capability root also serves the
server's own browser page and its CSS and JavaScript.

### Read endpoints

| Endpoint | Returns |
| --- | --- |
| `GET /<capability>/api/status` | the current snapshot as JSON, or `502` with `{"error": ...}` when the status stream cannot be read |
| `GET /<capability>/api/events` | a server-sent events stream of the same snapshots |
| `GET /<capability>/api/agents` | what `ssf agents --json` prints |
| `GET /<capability>/api/models/<harness>` | what `ssf models <harness> --json` prints |
| `GET /<capability>/api/usage` | what `ssf usage --json` prints: each harness's remaining provider allowance |
| `GET /<capability>/api/term/<session>` | a WebSocket terminal on a session's herdr pane, an item's or a scratch session's ([Terminal](#terminal)) |
| `GET /<capability>/chrome-extension.zip` | this build's [Chrome extension](#chrome-extension) as a zip download (`ssf-chrome-extension.zip`), which the browser page links to |

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
wrong with it. A listing may start Claude Code once, to make it refresh a
catalogue that is missing or past its own `staleAt` stamp, so a request for
`claude` can take a second or two and needs the login the harness itself has
(see [Models and effort](harnesses.md#models-and-effort)); every other
harness answers from files and commands alone.

`api/usage` returns one object per harness ssf can ask about (`claude`,
`codex`, `omp`, `pi`, `opencode`, `grok`), each with `accounts`: one per
provider credential the harness has stored, with its `provider`
(`anthropic`, `chatgpt`, `deepseek`, `openrouter`), `state`, `windows`
(`label` `5h` or `week`, `used_percent` 0 to 100, `resets_at`) and
`balances` (`currency`, `amount`). `state` is `ok`, `stale` (the last numbers
ssf had, kept because the harness's token has expired or was refused; `note`
says it refreshes on the harness's next run) or `unavailable`. A harness with
nothing to show has a `note` instead (`unavailable`, or `no usage data` for
`grok`, since xAI offers no usage request its sign-in can make). The factory
reads each harness's stored credential read-only and never refreshes it, and
keeps each provider's answer for five minutes in `usage.json` under its state
directory; see [Provider usage](harnesses.md#provider-usage).

### Terminal

`GET /<capability>/api/term/<session>` is a WebSocket (version 13) upgrade
onto a session's herdr pane, an item's (`owner%2Fname%2342`) or a scratch
session's (`owner%2Fname~id`). Anything that is not a session is `400`, as is
a request that is not a WebSocket upgrade. At most 16 terminals are open at
once; a seventeenth is `503`.

A terminal is one viewer of the pane's shared stream: `ssf __pane control
<session>` run over pipes, through the same client transport as the status
stream (so a factory in a VM is reached in the guest), which runs `herdr
terminal session control` on the pane (never `--takeover`). The server starts
it for the pane's first viewer and stops it, releasing the pane, when the last
one goes. Since it types at an agent, it is opened only for a request whose
`Origin` is a `chrome-extension://...` origin, or the page's own origin when
`dashboard.terminal_input` is on (`403` otherwise), and, for an item, only
where `item_pane_input` is on for the repository (the socket closes with the
reason otherwise); a scratch session's pane always takes typing. A scratch
session still running in tmux from before #565 has no pane: the socket closes
saying so. The protocol:

- **binary frames** from the server are what herdr drew. From the client they
  are typed bytes.
- **text frames** from the client are JSON: first `{"type": "hello", "name":
  "@login", "cols": N, "rows": N}`, the viewer's display name (`@` and a
  GitHub login, `dashboard` or `extension`; anything else is shown as
  `viewer`) and size, which a stream that is just starting takes;
  `{"type": "resize", "cols": N, "rows": N}` is the viewer's size;
  `{"type": "scroll", "direction": "up"}` (or `"down"`) is one wheel notch;
  `{"type": "take"}` takes control. Anything else is ignored.
- **Control**: one viewer holds it. Only the controller's typing (binary
  frames), `scroll` and `resize` reach the pane; everyone else's are dropped,
  though a view-only viewer's latest `resize` is kept and applied when it
  takes control. Joining never takes it, nor sizes the pane (a new stream
  starts at the pane's own size); `take` moves it to the sender; when the
  controller leaves no one holds it until a viewer takes it.
- **text frames** from the server are JSON: `{"type": "size", "cols": N,
  "rows": N}` when the pane's size changes (and on joining);
  `{"type": "viewers", "names": [...], "controller": "dashboard", "control":
  false}` whenever a viewer joins or leaves or control moves (`controller` is
  the holder's name or `null`, `control` whether this viewer holds it); and
  `{"type": "notice", "text": "..."}` for anything else, such as a pane being
  tried again.
- A viewer that joins is sent the whole screen: the server asks herdr to draw
  it again. A stream whose pane went away, or that someone else controls, is
  started again after 1, 2, 4, 8, 10, 10, 10 and 15 seconds, and then the
  sockets close.
- `item_pane_input` is checked (`ssf __pane input-check`) as each viewer
  joins and every minute while the stream runs; found off, the stream ends
  for every viewer with the reason.
- The server pings each viewer every 20 seconds and drops one that has sent
  nothing, a pong included, for 60 seconds, which releases its control.

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
| `pane_input` | whether the session's live terminal takes typing: always for a scratch session, for an item's as `item_pane_input` says for its repository. Clients offer an item's terminal only where it is `true` |
| `following` | the items the session subscribes to without owning, each an item like `additional` plus `events`: `state` or `all`, the level it follows at |
| `activity_note` | why `last_activity_at` is null: the harness keeps no local transcript ssf can read (`omp`; ssf reads Claude Code, Codex and Grok), the session's conversation is not identified yet, or ssf has not found its transcript yet. Clients show that sentence where the time would be |

A client holding several factories can label a card without asking which stream
it came from.

Each item in `dashboard.monitored_items`, and each card's `origin` and
`additional` items, carries `has_workspace`, with `branch` when the driver
reports one, and `github_state` (`open`, `closed`, `merged` or `unknown`).
An item that has a workspace is not one `ssf assign` accepts:
`ssf release` is what frees it, so a client draws no assign form for it.

`dashboard.blocked` lists the items whose harness cannot take prompts (not
signed in, or could not start), each an item as above plus `repo` and
`blocked` (`reason`, `harness`, `harness_name`, `detail`, `since`, `fix`, as in
`ssf status --json`). `dashboard.released` lists the items no longer active
whose workspace `ssf release` or `ssf purge` removed, each an item as above
plus `repo` and `released_at`; scratch sessions are in `dashboard.scratch`
instead. `dashboard.last_error` is the daemon's last recorded error, or
`null`. `dashboard.doctor` is the top-level `doctor` (the cached doctor run's
failures and warnings, see [internals](internals.md)), or `null`.
`dashboard.host` is the top-level `host`: the factory host's `cpu_percent`
(averaged since the previous status), `mem_percent` (MemTotal less
MemAvailable) and `disk_percent` (the filesystem holding the factory's state),
each a number or `null` where it cannot be read (off Linux, and `cpu_percent`
on the first status after the daemon starts). The extension's top-bar HUD
reads all five.

`dashboard.repositories` lists the repositories the factory watches, as
`owner/name`. A factory watches a repository rather than the items in it, so
its cards and monitored items say nothing about an item nobody has assigned
yet. This list is what lets a client tell such an item, which can take a
session, from one the factory has never heard of, which is refused. A server
that does not publish it sends an empty list.

`dashboard.repository_projects` maps each watched repository (`owner/name`)
to the URLs of the Projects v2 linked to it, which the daemon reads from
GitHub at most every ten minutes (a failed read keeps the last list).
Repositories with no linked projects are left out, and a server that does not
publish it sends nothing: a client reads a missing field as no projects.

`dashboard.scratch` lists the factory's scratch sessions (`ssf scratch`),
released ones included until the factory drops them
(`daemon.scratch_release_grace_hours`, 24 hours by default), so a client can
offer to resume them: each has `id`
(`owner/name~id`), `repo`, `owner_login` (`null` for a shared session),
`active`, `agent_live`, `state`, `released_at`, `harness`, `model`,
`effort`, `branch` and `pane_input`. `state` is `live` (its harness runs),
`off` (its workspace is there and its harness is not: the harness
exited or the factory restarted), `releasing` (killed; the workspace goes on
the daemon's next pass) or `released` (no workspace); `ssf status --json`
carries it as `scratch_state`.

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

Six POST routes. The first three are one `ssf` command for one item, named
by repository and number; the scratch routes are `ssf scratch`'s, named by
repository or by session id. Typing into a session's pane is the
[terminal](#terminal)'s, not a write route. Their bodies are the command's own arguments; a field a route does not
take is refused rather than ignored, so a misspelled one cannot ask for
something nobody meant.

```json
POST /<capability>/api/assign   {"repo": "owner/name", "number": 42, "harness": "claude", "model": "opus", "effort": "low"}
POST /<capability>/api/handover {"repo": "owner/name", "number": 42, "harness": "omp", "model": "deepseek/deepseek-flash", "effort": "high", "note": "carry on from here"}
POST /<capability>/api/release  {"repo": "owner/name", "number": 42}
POST /<capability>/api/scratch         {"repo": "owner/name", "harness": "claude", "model": "opus", "effort": "low", "for": "octocat"}
POST /<capability>/api/scratch/release {"session": "owner/name~id", "force": false}
POST /<capability>/api/scratch/resume  {"session": "owner/name~id"}
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

- **scratch** starts a scratch session on a watched repository, as
  `ssf scratch create`; `model`, `effort` and `for` are optional, and `for` (a
  GitHub login) marks the session as that person's rather than shared.
- **scratch/release** kills a scratch session and removes its workspace. Unlike
  an item's release it can be forced: an unforced request that the workspace
  checks refuse is `409` carrying the daemon's whole answer, `check` included,
  so a client can show what would be lost and ask again with `"force": true`.
- **scratch/resume** starts a scratch session that is `off` again in its
  workspace, or a released one in a new workspace; one that is live is `409`.

A scratch session's terminal always takes typing. An item session's takes it
only where `item_pane_input` is on for its repository (`daemon.item_pane_input`,
overridden by `repo.item_pane_input`; off by default): a person speaks to an
item's agent by commenting on the item, where everyone working it can read the
exchange (#439). Each card and scratch entry in the snapshot carries
`pane_input`, whether its pane takes typing, so a client need not know the rule.

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
- The path must be one of the write routes: `POST` anywhere else under the
  capability is `405`, and the write routes are not readable (`404`).

Every accepted write is logged at info with the origin and the item, so acting
on a session from a browser is visible in the server's journal.

## Chrome extension

The optional [Chrome extension](../chrome-extension/README.md) is the client
these endpoints are for: it overlays session state on github.com instead of in
a browser tab, and offers assign, hand-over and release through the
write routes. The factory carries the extension that matches it: download it
from the browser page's **Download Chrome extension** link, or write it with
`ssf chrome-extension [--output PATH] [--force]`, then unzip it and load the
unzipped directory unpacked. Its own guide covers installation, the states it draws and the
options page.

On a pull request page the extension shows the factory's own card for the pull
request when there is one — the binding by session tag or branch, which is what
a delegated pull request has — and otherwise the card of the issue the body
closes or refs, marked `for #N`. A closed or merged item's page reads **Done**
and offers no Assign form.

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

On Omarchy, the Factory menu's **Dashboard** entry and widget's **Session
dashboard** button open the TUI in a terminal.

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
- `POST /<capability>/api/assign` runs `ssf assign` for one item and returns
  its result. See [Assigning from the API](#assigning-from-the-api).

Each card in `/api/status` and `/api/events` carries, besides the fields the
dashboard and TUI render: `tool`, the session's current tool call formatted as
`ssf status --json` formats it (`null` when the agent is not making one),
`branch`, the workspace's branch (`null` when the item has no workspace), and
`factory`, the name the server answers for — its catalog target name when it was
started as one (`ssf-server --target …`), else its hostname. A client holding
several factories can then label a card without asking which stream it came
from; the TUI and the server's own browser page ignore the three fields and are
unchanged.

The read endpoints accept an `Origin` of `http://<bind>:<port>` or any
`chrome-extension://...` origin, so a Chrome extension's service worker can read
them; the `Host` header must still match the configured bind address and port.

#### Assigning from the API

`POST /<capability>/api/assign` takes one assign request as JSON:

```json
{"repo": "owner/name", "number": 42, "harness": "claude", "model": "opus", "effort": "low"}
```

`repo` must be a repository this factory watches, and `harness` an id from
`/api/agents`; `model` and `effort` are optional and default to what the harness
itself uses, exactly as `ssf assign --model/--effort` leave them. The request is
the one the `ssf assign` command sends the daemon, so it answers with the same
JSON that command prints under `--json` (`session`, `title`, `from`, `to`,
`assigned`, `overrides_written`, `open`, `poll_interval_secs`). The status says
what happened:

- `200` with that result.
- `400` with `{"error": ...}` when the request itself is at fault: a repository
  this factory does not watch, a harness ssf does not know, a model or effort
  that harness does not take, or a body that is not the JSON above.
- `409` with `{"error": ...}` when the item is not in a state an assignment can
  be applied to — it already has a session (`ssf handover` is what moves one),
  a handover or release of it is pending, or it is worked by another item's
  session.
- `502` with `{"error": ...}` when the factory could not do it: the daemon is
  not running, or the harness is not installed or not signed in where the
  sessions run.

The `error` is the message `ssf assign` would have printed, verbatim.

#### Write rules

Reads answer on the rules above. The one write is held to stricter ones, all of
them decided from the request line and headers before any body is read, and each
refused without anything having been done:

- The `Origin` must begin `chrome-extension://`. The bind host's own origin is
  refused for a write, so a page loaded from a browser on the tailnet cannot
  start a session even if it learns the capability URL; an ordinary HTTP client
  with no `Origin` is refused too (`403`).
- `Content-Type` must be `application/json` (`415`).
- The body must be at most 4 KiB, by its `Content-Length` (`413`); a chunked
  body is refused outright (`400`), and so is one with two lengths.

Every accepted write is logged at info with the origin and the item, so starting
a session from a browser is visible in the server's journal.

The optional [Chrome extension](../chrome-extension/README.md) is the client these
endpoints are for: it overlays this state on github.com instead of in a browser
tab.

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

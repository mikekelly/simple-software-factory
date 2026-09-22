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

Two endpoints under the capability path serve the canonical dashboard model:

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

Both endpoints accept an `Origin` of `http://<bind>:<port>` or any
`chrome-extension://...` origin, so a Chrome extension's service worker can read
them; the `Host` header must still match the configured bind address and port.

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

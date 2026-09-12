# Session dashboard

Run a live dashboard in any ordinary terminal:

```sh
ssf dashboard
ssf --server customer@cloud.example dashboard
SSF_SERVER=customer@cloud.example ssf dashboard
```

Linux and macOS clients include the TUI. No browser, Python, Omarchy or Herdr
installation is required. Local factories, local VMs and remote factories use
the standard SSF client/server transport. Configure SSH authentication first;
remote polling uses noninteractive SSH and reuses one authenticated connection.

The dashboard stays open and refreshes automatically. Use arrows or `j`/`k` to
select a card, Enter to focus its agent in Herdr, and `q` or Ctrl-C to quit.
Mouse selection and wheel scrolling work in terminals with mouse reporting.
The terminal's normal screen and input mode are restored on exit. Issue IDs
are hyperlinks in terminals supporting OSC8 links; use your terminal's normal
modifier-click gesture to open them.

Cards show the originating issue, additional active assigned issues, agent
state, last activity and latest message or summary. They use the server's
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

Only loopback IP addresses are accepted, including `::1`. Direct binds to a
LAN address, Tailscale address, `0.0.0.0` or `::` are refused. The built-in
endpoint provides neither TLS nor user accounts. Remote web access requires
a reverse proxy that:

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

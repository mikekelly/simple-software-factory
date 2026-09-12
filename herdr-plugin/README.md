# Optional SSF dashboard shortcut for Herdr

`ssf dashboard` is the real-time terminal dashboard. Run it directly in any
ordinary terminal or any Herdr pane:

```sh
ssf dashboard
ssf --server customer@cloud.example dashboard
SSF_SERVER=customer@cloud.example ssf dashboard
```

Inside Herdr, Enter or a mouse click focuses the pane whose agent session ID
matches the selected SSF card. The dashboard stays alive in its original pane.
Navigation is scoped to the Herdr server in which the dashboard runs; a remote
SSF connection does not connect to that factory's Herdr server. Missing sessions
or closed panes produce an explanation in the dashboard.

This optional plugin is only a launch shortcut. It requires Herdr 0.8.2 or newer,
`ssf`, and `jq` on the Herdr server's PATH. It opens a normal tab and runs the same
client command there, passing through `SSF_SERVER`:

```sh
herdr plugin install mikekelly/simple-software-factory/herdr-plugin
# Or link a checkout:
herdr plugin link /path/to/simple-software-factory/herdr-plugin
herdr plugin action invoke ssf.dashboard.open-dashboard
```

No plugin, popup, browser, or Python runtime is needed by the TUI. See the
[dashboard guide](../docs/dashboard.md) for operation and optional server web UI.

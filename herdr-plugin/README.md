# SSF dashboard compatibility action for herdr

The dashboard now ships in the `ssf` client on Linux and macOS. Launch it on
the machine where you want the browser to open:

```sh
ssf dashboard
ssf --server customer@cloud.example dashboard
SSF_SERVER=customer@cloud.example ssf dashboard
```

See the [dashboard guide](../docs/dashboard.md) for operation and security.
The Python dashboard and its fixed-port SSH-tunnel workflow are retired;
there is one implementation, embedded in the client, with no Python dependency.

This plugin is retained as a compatibility launcher for graphical,
same-machine installations. It requires herdr 0.8.2 or newer and an updated
`ssf` on `PATH`. Update an existing plugin installation to get the launcher:

```sh
herdr plugin install mikekelly/simple-software-factory/herdr-plugin
# Or link a checkout:
herdr plugin link /path/to/simple-software-factory/herdr-plugin
herdr plugin action invoke ssf.dashboard.open-dashboard
```

Herdr executes actions on its server. If that server is in a VM or remote
host, invoke `ssf dashboard` on your desktop instead; the plugin cannot
open your desktop browser from the server. For a local SSF-managed VM, the
host client uses normal VM forwarding. For an SSH factory, use `--server`
or `SSF_SERVER` as above. The compatibility action detaches the client so
herdr can release its action slot; the client's idle timeout bounds its lifetime.

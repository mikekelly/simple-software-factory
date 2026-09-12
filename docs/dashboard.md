# Session dashboard

Run the dashboard on your desktop to open a browser grid of active sessions:

```sh
ssf dashboard
ssf --server customer@cloud.example dashboard
SSF_SERVER=customer@cloud.example ssf dashboard
```

The browser always opens on the client machine. A local client reads the local
factory, including SSF's normal VM forwarding; `--server` or `SSF_SERVER`
selects a factory over the standard SSH transport. No manual HTTP tunnel is
needed. Configure SSH authentication first (for example, with your SSH agent);
the dashboard uses noninteractive SSH and reports authentication failures.
Linux and macOS clients include the HTML, CSS and JavaScript in the
binary, with no Python, Omarchy or `ssf-ui` dependency. A browser and the
platform browser opener (`xdg-open` on Linux, `open` on macOS) are used to open
the page. `ssf dashboard --no-browser` prints its URL without opening it.

The command stays running while serving the page. Stop it with Ctrl-C, or
close the dashboard tabs: it exits after five minutes without browser polling,
plus up to 35 seconds to finish an in-flight request.
Each invocation chooses an ephemeral port on `127.0.0.1` and an unguessable
capability URL. Treat that URL as access to your dashboard; it can reveal
session summaries and repository details to another local process that knows
it. The listener is never exposed on the factory's network interface.

Cards group issues by their canonical owning session, including additional
active issues sharing a workspace. They show linked issues, activity and the
latest message or status summary. The server remains responsible for status
and ownership. Missing activity timestamps display **Unknown**; the dashboard
does not read full agent transcripts. VM and connection failures display a
problem state, rather than an empty factory.

On Omarchy, use the Factory menu's **Dashboard** entry or the widget's
**Session dashboard** button. The old [herdr plugin](../herdr-plugin/README.md)
is now only a compatibility launcher for graphical, same-machine installs.
For remote or headless herdr servers, launch the client on your desktop.

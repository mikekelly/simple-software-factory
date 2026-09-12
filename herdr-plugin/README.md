# SSF dashboard for herdr

This herdr action opens a browser dashboard of the active agent sessions from
`ssf status --json`. It shows one card per owning session and groups any other
active issues assigned to that session on the same card.

Install it from GitHub (or link it from a checkout), then invoke its action:

```console
herdr plugin install mikekelly/simple-software-factory/herdr-plugin
herdr plugin link /path/to/simple-software-factory/herdr-plugin
herdr plugin action invoke ssf.dashboard.open-dashboard
```

It requires herdr 0.8.2 or newer, Python 3.10 or newer, and `ssf` on `PATH`.

The action works directly when herdr and SSF run on a graphical host. Herdr
actions run on the herdr server, so an action inside SSF's default headless VM
cannot open a browser on the attaching computer. The easiest VM setup is to run
the dashboard script on the host, where the host `ssf` command forwards status
requests to the guest:

```console
python3 /path/to/simple-software-factory/herdr-plugin/dashboard.py
```

For SSH or a headless server, choose a fixed port and print its URL:

```console
python3 dashboard.py --no-browser --port 8765
ssh -L 8765:127.0.0.1:8765 your-server
```

Open the printed capability URL through that tunnel. The server binds only to
`127.0.0.1`, gives every server instance a random URL token, and stops five
minutes after the browser stops polling it. Re-running the action reuses a live
server.

For live Claude Code and Codex sessions, the activity time is the transcript's
last write time when herdr reports a conversation reference. Other sessions
say **Unknown** when the driver has no activity timestamp. The latest message
or summary is herdr's short agent title/status summary; the dashboard does not
read or expose the full transcript.

Run the self-contained test suite with:

```console
python3 -m unittest discover -s herdr-plugin/tests -v
```

# Optional SSF overlay for github.com

A Chrome extension (Manifest V3) that overlays live SSF agent state on
github.com: a badge beside the title of an issue or pull request, and a small
state indicator on issue and pull request lists, search results and project
boards. It is **read-only**. It reads the same canonical dashboard model as the
TUI and the server's web UI, and it never acts on the factory.

Both the endpoint it reads and the exposure rules for reaching it are described
in the [dashboard guide](../docs/dashboard.md).

## Install

1. Enable the server endpoint. It is off by default:

   ```sh
   ssf config set dashboard.enabled true
   ```

   In VM mode this belongs to the host. An enabled listener requires a server
   restart.

2. Find the capability URL. `ssf-server` logs it when it starts:

   ```sh
   journalctl --user -u ssf.service | grep 'Server web dashboard'
   # ssf@NAME.service for a named target
   ```

   It looks like `http://127.0.0.1:8787/<secret>/`. It changes on every server
   restart, so re-save it when the server restarts.

3. Load the extension: open `chrome://extensions`, turn on **Developer mode**,
   choose **Load unpacked**, and select this `chrome-extension/` directory.

4. Open the extension's options page (the toolbar icon, or **Details → Extension
   options**) and add one entry per factory: a label and the capability URL.
   Chrome asks permission for each factory's address the first time; that
   permission is what lets the extension read that factory and nothing else.
   **Save** stores the list in `chrome.storage.local`.

5. Open any issue or pull request in the factory's repository.

There is no build step, no npm dependency and no bundler: the extension is the
plain JavaScript, HTML and CSS in this directory. It is not part of the Arch
package; nothing here affects `makepkg`.

## What it shows

An issue or pull request page gets a badge beside the title, one row per factory
that knows the item:

![An issue page with the ssf badge beside the title, reading the harness, model,
agent state, last activity and the session's latest message](docs/issue-badge.png)

```
ssf  omp · deepseek/deepseek-flash · working · 12m ago
     π Build and test Chrome extension
```

Lists, search results and project boards get a small pill beside each item's
title:

![A list of issues, each with a small ssf pill after its title](docs/issue-list.png)

- The harness, model, agent state and last activity are the factory's own
  fields, and the second line is the session's latest message or summary.
- An item SSF monitors without an agent reads `no agent` with the item's title.
- A factory whose stream has failed keeps the cards it last sent and marks them
  `stale`, and a factory that has never answered is named on every issue page as
  `unknown` with the reason. A broken factory is never silently shown as an idle
  one; the TUI makes the same distinction in [the dashboard
  guide](../docs/dashboard.md#session-dashboard).
- Items the factories do not know about are left alone.

A card matches a page by `owner/repo#number`, from its originating issue or from
any of the item's additional issues, so a session started on another issue and
bound to this one still shows up here.

## Reaching a factory on a tailnet

A tailnet factory needs `dashboard.bind` set to a Tailscale address; see
[the dashboard guide](../docs/dashboard.md) for the accepted binds and the
exposure rules that go with them. The transport is plain HTTP, encrypted by
WireGuard between tailnet devices and by nothing else, so treat the capability
URL as a secret and paste it only into this extension or a browser on the
tailnet. Never expose the port through Tailscale Funnel or a router port
forward, and keep tailnet ACLs restrictive.

## How it works

- The **service worker** keeps one `EventSource` per configured factory on
  `api/events`, which the server answers with a snapshot on connect and on every
  change, and reconnects with exponential backoff (1s to 60s) when a stream
  fails. The latest snapshot per factory is held in memory and served to content
  scripts, one factory's failure never affecting another's.
- The **content script** on `https://github.com/*` renders from that snapshot,
  re-rendering on GitHub's client-side navigation and DOM updates without
  duplicating what it has already drawn. Every node is built with `textContent`
  and lives in a shadow root, so no factory text is ever parsed as HTML and no
  GitHub style leaks in.
- `host_permissions` is `https://github.com/*`. Factory addresses are
  `optional_host_permissions`, requested at runtime from the options page, so
  the extension only holds access to the factories you added.

## Limitations

- Read-only: there are no actions from the browser in this version.
- The capability URL changes when the server restarts; the options page must be
  updated to match, or the factory reads as unreachable.
- A project board indicator depends on the board rendering its cards as links to
  the issue or pull request, as GitHub's board and list views do.
- Chrome prompts for each factory address once; until it is allowed, the badge
  says so rather than showing state it cannot read.

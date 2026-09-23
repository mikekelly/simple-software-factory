# Workspaces and terminals

How ssf turns a repository into workspaces an agent can work in, and what an operator
does when one of them goes missing. For an agent operating a factory. Offline route:
`ssf skill drivers`.

ssf is built on [herdr](https://herdr.dev/): herdr provides the terminal workspaces that
hold coding agents, and ssf connects those workspaces to GitHub items, prompts, session
state and lifecycle events. `driver = "herdr"` is an explicit declaration of that, and
herdr is the only driver ssf supports.

## What a workspace is

Three things stack up, one per level:

| Level | What it is | Where |
|---|---|---|
| Clone | one git clone per watched repository | under `herdr.projects_dir` (default `~/ssf/projects`) |
| Worktree | one git worktree per item, on that item's branch | `<clone>.worktrees/issue-N-short-title` |
| Workspace | one herdr workspace (a tab) per session, running the harness in its root pane | herdr, labelled `<repo>-<issue-number>` |

ssf keeps the checkouts itself, because herdr has no project registry of its own. The
label uses the GitHub repository name even when `repo.path` points at a directory with a
different name; workspaces already open keep their labels until they are reopened. The
worktree path and branch name carry the item number and title, which is what makes a
lost workspace recoverable.

`herdr.command` selects the CLI ssf runs, and `herdr.projects_dir` selects where the
clones go. `ssf doctor` checks both the CLI and the running server.

## Host or guest

Herdr must be installed, and its server running, wherever the agents run. With the
factory in a [microVM](vm.md), that is inside the guest: the VM image installs and
starts herdr there, and the host only talks to it. In host mode, herdr is installed on
the host and the factory drives the host's own herdr server.

Either way the server has to be up before the daemon can open a workspace. `herdr
server` runs it headless, which is what a VM guest or a remote host uses; an interactive
`herdr` left running serves the same purpose on a desktop, and is what lets a person
watch the sessions and type in them. `ssf doctor` reports a herdr that is not answering.

## Reading and driving an agent

Herdr recognises the agent in a workspace's root pane and reports its state: `idle`,
`working`, `blocked` or `done`. ssf uses that state to decide when a freshly launched
agent has settled, when it is ready for input, and what to show on `ssf status` and the
[dashboard](dashboard.md). The reported state does not say whether a dialog is up, so
ssf also reads the pane's screen: it answers the known first-run trust dialogs itself,
and it treats a harness sign-in screen as a block on the session rather than typing into
a terminal that cannot act (see [A harness that is not signed
in](sessions.md#a-harness-that-is-not-signed-in)).

Herdr can only launch agents it recognises; `herdr agent start --help` lists them. `ssf
repo add` warns about a harness herdr does not know, and a start that never produces an
agent gives up after `herdr.tui_idle_timeout_ms`.

## Item activity delivery

Activity on an item reaches its running session through the harness's own channel where
that harness has one, and through the terminal otherwise. A held delivery is one ssf has
published but the session has not recorded: nothing is pasted and nothing is resent, the
item's bookkeeping is left as it was, and `ssf doctor` reports the session whose channel
is degraded, so what an operator sees is a session that stops receiving rather than one
that receives twice. The usual remedy is to restart that session so it comes up on the
current channel. The mechanics, including what to inspect before resolving a held
delivery, are in [per-harness delivery](internals.md#per-harness-delivery); the rule
that a custom `command` must keep whatever its harness's channel needs is in
[harnesses.md](harnesses.md).

## When a tab or a workspace disappears

Herdr keeps no item link of its own, so ssf recovers a workspace by its worktree. A
workspace id stored by ssf combines herdr's id with the checkout path, such as
`w7@/home/you/ssf/projects/widgets.worktrees/issue-42-fix`, and before prompting into a
workspace or removing it ssf asks herdr which worktree that workspace is bound to. The
path is what makes the check meaningful:

- A workspace id herdr has reused for something else is treated as **gone**, never
  touched.
- Changing a shell's working directory inside a pane does not change what the workspace
  owns.
- The clone's own herdr workspace, if a person opened one, is left alone.

A workspace that is gone is not a loss. On the item's next event, ssf re-creates the
workspace on the same worktree and branch and starts the harness again, resuming the
conversation where the harness keeps one; it says so on the item as a `resumed` or
`attached` post (see [What ssf says on the
item](sessions.md#what-ssf-says-on-the-item)).

**Closing a herdr tab does not remove its git worktree.** The checkout stays on disk
with whatever the agent left in it, and nothing lists it as a workspace any more. That
is the one case worth an operator's attention:

```sh
ssf doctor
```

`ssf doctor` names the checkouts holding commits that are on no other branch and not on
origin, uncommitted changes, or a stash entry made on their branch, with no agent in
their workspace. Push or salvage those before removing anything;
[`ssf purge`](sessions.md#workspaces-after-close-release-and-purge) sweeps the ones that
are safe and leaves the rest.

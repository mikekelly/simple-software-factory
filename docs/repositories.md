# Adding a repository

For the agent asked to put a repository into a factory that already runs, from preconditions to the first issue delivered. Offline: `ssf skill repo`.

Work through the steps in order. Stop at any step whose result does not
match, and take the linked remedy in [troubleshooting.md](troubleshooting.md).

## 1. Preconditions

Pick the target first. A client can hold several factories.

```sh
ssf server list
```

Every later command takes `--server NAME` for one of those names. Without a
server catalog the commands act on this machine's own factory, and `--server`
takes an SSH destination instead. Use the same target for every step.

```sh
ssf --server NAME doctor     # NAME from the listing; omit --server with no catalog
```

A good result ends with `all good`. Anything else is fixed before going on:
see [doctor fails](troubleshooting.md#doctor-fails). The line that matters
here is the one naming the bot: `GitHub token belongs to @bot-login`.

Then the access. Ask the person, who needs admin on the repository, to invite
`@bot-login` as a collaborator with **Write** (the bot has to push branches,
open pull requests and comment) and to give it access to any project (v2) board
the repository's items sit on (without it the prompt carries no board section).

The invitation has to be accepted. The daemon accepts one by itself only when
the inviter's login is listed in `github.auto_accept_invitations_from` in
`config.toml`; every other invitation stays pending until someone accepts it
in the bot's own GitHub account. Check and set the list with:

```sh
ssf config get github.auto_accept_invitations_from
```

That setting is account-wide and grants nothing else: accepting access never
enrolls a repository. If an invitation is pending, see
[invitation pending](troubleshooting.md#github-access).

Who may drive: by default the repository's collaborators with push access.
`daemon.allowed_users` narrows that for the whole factory, and
`--allowed-users` on the repository replaces it for this one. Comments from
anyone outside the list are ignored. `--allowed-users '*'` means anyone on
GitHub and needs `--accept-anyone-risk`; never pass it without the person
saying so in this conversation.

## 2. Choose harness, model and effort with the person

This is the person's decision, not the agent's: it sets what the sessions cost
and what they can do. Present the options and ask.

```sh
ssf agents            # ids, and which are installed
ssf agents --json     # per harness: whether model and effort are supported, and the effort levels
ssf models <harness>  # the model ids that harness takes
```

`ssf models` prefers the installed harness's own catalogue and says which
source answered, so the ids it prints are the ones that harness will accept.
Use only ids from that output.

What the choice means: a larger model and a higher effort level deliberate
better and cost more per session; a smaller model and a lower level are enough
for bounded implementation work. The repository's harness, model and effort are
its default stack, used for every item unless an item overrides it.

| Scope | Command | Lifetime |
| --- | --- | --- |
| The repository | `ssf repo add` / `ssf repo set` | every new session on the repository |
| One item, from the start | `ssf assign <item> --harness ... --model ... --effort ...` | stays with the item across restarts, until its workspace is released |
| One item, mid-flight | `ssf handover <item> --harness ... --model ... --effort ...` | same workspace, new session; stays with the item |

The harness has to be installed and signed in where the daemon runs, which is
inside the VM when the factory runs in one. `ssf doctor` reports both.

## 3. Enroll the repository

```sh
ssf repo add owner/repo --harness <harness> --model <model> --effort <level>
```

Model and effort are required in the resulting config for every harness that
supports them, which is why step 2 comes first. A good result is the repository
appearing in:

```sh
ssf repo list
```

Change settings later without restating the rest, and clear an optional field
by name:

```sh
ssf repo set owner/repo --model <model>
ssf repo set owner/repo --clear effort
ssf repo remove owner/repo      # stop watching; the checkout and worktrees stay
```

Read `ssf repo add --help` and `ssf repo set --help` in full before using
anything below; the other settings are:

| Setting | What it does |
| --- | --- |
| `--driver` | where this repository's sessions run, overriding the top-level `driver` |
| `--path` | an existing local checkout to use instead of cloning |
| `--clone-url` | non-default clone URL |
| `--base-branch` | base ref for issue worktrees (default: the checkout's remote HEAD) |
| `--command` | the command that starts the harness (default: its permission-free command, from `ssf agents --json`) |
| `--auto-compaction-tokens` | context the harness may fill before compacting (default 300000; `0` leaves its own default alone) |
| `--instructions` | extra text appended to this repository's initial prompt |
| `--prompt-file` | where the repository's agent guidance lives (default `SSF.md`) |
| `--allowed-users` | who may drive this repository |
| `--event-comments` | whether daemon events are posted as short `ssf` blocks on items |
| `--git-name` / `--git-email` / `--git-signing-key` / `--git-credential` (`repo set`) | commit identity and who pushes |

Enrollment is picked up on the daemon's next poll (`daemon.poll_interval_secs`,
10 s by default). No restart is needed.

## 4. SSF.md

`ssf doctor` checks that the repository has the file named by `prompt_file`
(`SSF.md` by default) on its base branch, because that file is the only
repository-owned guidance the main session gets. It reaches the main session
only, never its subagents.

A minimal working `SSF.md`, committed at the repository root:

```markdown
# SSF agent guidance

## Role

- Own the outcome on the assigned issue from clarification through delivery.
- Plan on the issue until the outcome is unambiguous; do not start
  substantial implementation before that.
- Bring the big decisions and matters of taste to people, one at a time;
  settle the small details yourself.

## Models

| Harness | Deliberation | Execution |
| --- | --- | --- |
| `<harness>` | `<model>`, effort `<level>` | `<model>`, effort `<level>` |

## Communication

- Post when starting, when blocked, and when delivering.

## Review and delivery

- Review in proportion to risk; fix confirmed defects and violations of the
  acceptance criteria.
- Delivered means the outcome, its validation and remaining limitations are
  posted on the issue with the pull request link.
```

Fill the placeholders with ids from `ssf models <harness>` and effort levels
from `ssf agents --json`. Do not invent model ids.

What goes elsewhere: build, test and implementation policy that applies to
every agent and every subagent belongs in `AGENTS.md`, not here. `SSF.md` is
read once per session, so keep it short. `SSF.example.md` at the repository
root is a fuller starting point, and [ssf-md.md](ssf-md.md)
(`ssf skill ssf-md`) explains each choice.

Project boards: if an item sits on GitHub project (v2) boards, the initial
prompt lists each board's name and URL, the card's current Status, the Status
options, and the `gh project item-edit` command that changes it. The agent is
told to keep the Status accurate and that which column fits is its call. ssf
prescribes no mapping from events to columns, so put the repository's column
conventions in `SSF.md`. The lookup uses the bot token's `project` scope; if it
fails the prompt simply carries no boards section.

## 5. Items that pre-date enrollment

A repository usually has items already assigned to the bot. The daemon does not
start those automatically: it snapshots them as adoption candidates, so
enrolling a busy repository does not launch a dozen sessions at once.

```sh
ssf candidates
ssf candidates --repo owner/repo --json
```

Adopt only the ones the person wants worked now. Adoption replays the item's
complete GitHub history into a fresh session:

```sh
ssf adopt owner/repo#12 owner/repo#15
```

Candidates left alone stay idle and cost nothing.

## 6. The first issue

Ask the person for a small, self-contained issue, or write one with them. A
good first issue says what "done" looks like (a test that passes, a file that
changes, a command that works) and names what to run before opening a pull
request.

Assign it to the bot. On GitHub that is the assignee field; @mentioning the bot
or requesting a review from it works too. From the client, to pick the stack for
this item only:

```sh
ssf assign owner/repo#N --harness <harness> --model <model> --effort <level>
```

`ssf assign` is refused for an item that already has a session; use
`ssf handover` for that one.

Within a poll interval the item appears in `ssf status` and a workspace named
`<repo>-<issue-number>` appears in herdr; the first item on a repository takes
longer, because the clone happens first. Within a couple of minutes the agent
comments on the issue with what it is about to do, under a
`🤖#N <harness>/<model>/<effort> says:` byline: that comment is the proof the
whole chain works, and `ssf peers` shows the session. Then comes a pull request
from the issue's branch, linked from the issue, and a comment on
the issue carrying the link and the validation results.

Steering is done in comments on the item, as with a colleague: say what is
wrong or what to do next, and the session receives it as an `[ssf]` message.
Comments are the interface; there are no chat commands. Only logins in the
allowed list are heard. To change the harness, model or effort mid-flight,
`ssf handover <item> --harness ... --summary "..."`: the daemon ends the
session on its next pass and starts a new one in the same workspace.

When the work is done, the person reviews and merges as they would for a
colleague and closes the issue. The session pushes what is left, comments once
more and gives its workspace back with `ssf release`, which refuses while the
tree is dirty or anything at HEAD is not on a remote. Later, `ssf purge` clears
the workspaces of closed items whose agent is gone.

## 7. Verification and common failures

```sh
ssf --server NAME doctor     # NAME from the listing; omit --server with no catalog
ssf repo list
ssf status
```

Doctor prints one line per repository: model and effort explicit, the GitHub
repository identity, the allowed users, the harness installed and signed in,
the `SSF.md` present on the base branch, the checkout, and any worktree holding
work that is on no remote.

| Symptom | Where to go |
| --- | --- |
| `ssf doctor` reports a failure | [doctor fails](troubleshooting.md#doctor-fails) |
| The repository is not listed after `ssf repo add` | [wrong target](troubleshooting.md#triage) |
| `SSF agent guidance missing` | [no SSF.md](troubleshooting.md#repository-and-items) |
| Item assigned, nothing happens | [nothing happens on assignment](troubleshooting.md#nothing-happens-on-assignment) |
| Bot cannot push, comment or see the board | [GitHub access](troubleshooting.md#github-access) |
| Session shows as blocked | [sessions](troubleshooting.md#sessions-and-delivery) |
| `ssf release` refused | [release refused](troubleshooting.md#release-refused) |

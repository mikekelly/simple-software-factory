# Identity and bylines: who the agent is on GitHub, and which session posted what

How `ssf launch` makes everything git and GitHub inside an agent's process act as the bot, and how the byline and origin tag tell one session's posts from another's and from a person's. For whoever reads the bot's posts or debugs attribution; agents get the short version from `ssf guide`.

## How the agent gets the bot's identity

Agents are started through `ssf launch`, which builds an environment in which
everything git and GitHub related is the bot, whatever the human's own
`~/.gitconfig`, `gh auth` or SSH agent say:

| What | How |
|------|-----|
| `gh` and the GitHub API | `GH_TOKEN`, `GITHUB_TOKEN` (read from gh's keyring for the bot account, or from a pasted token / `SSF_GITHUB_TOKEN`) |
| HTTPS pushes | a git credential helper (`ssf git-credential`) that answers with the token, placed ahead of any configured helper |
| SSH pushes | `GIT_SSH_COMMAND` pinned to the enrolled bot key with `IdentitiesOnly=yes` |
| Commit author and committer | `GIT_AUTHOR_*`, `GIT_COMMITTER_*` and `user.name`/`user.email` |
| Commit signing | `gpg.format=ssh`, `user.signingkey=<bot key>`, `commit.gpgsign=true` (or `commit.gpgsign=false` when no key is enrolled, so nothing is signed with the human's key) |
| Which issue this is | `SSF_REPO`, `SSF_ISSUE`, `SSF_ISSUE_URL`, `SSF_BOT`, and `SSF_ROLE=reviewer` in a reviewer session (`ssf launch --role reviewer`) |
| Which session posted what | a `gh` wrapper first on `PATH` that starts every post with the byline (below) |

Git settings go in through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`, which
outrank every config file, and only inside the agent's process tree. The
initial prompt tells the agent that plain `gh` and `git push` act as the bot.
The bot's own commits and cross-references are filtered out of follow-up
messages, and its comments are sorted per session by their byline, so
an agent's own posts are not echoed back to it (`daemon.include_own_events`
turns both off). `ssf token` still prints the token for any other use.

The bot identity is a default, not a security boundary: on bare metal the
agents run as your Unix user inside your session, so a determined agent can
still read your own gh token from the keyring or use your SSH agent. ssf
tells agents to act only as the bot and to report missing permissions
instead. For real isolation, run the factory [inside a microVM](vm.md).

## Bylines and origin tags: which session posted what

GitHub shows the same bot account for every session, so ssf puts the
session in the content. Everything an agent posts starts with one line that
is both a byline for people and a tag for the daemon, then a blank line:

```
🤖#16 says: <!-- ssf: origin=owner/repo#16 -->
```

The byline is `🤖#N says:` when the post is on the same repository as the
session's item and `🤖owner/repo#N says:` on another; GitHub renders the
item in either as a link to it, so a reader can tell a session's posts from
a person's at a glance and see which session wrote them, even when the
"bot" is someone's own account. A reviewer session's byline is
`🤖#N (reviewer) says:`. Posts from before #42 have the byline without
`says:`; the daemon reads those the same way.
The HTML comment after it (the *origin tag*) is invisible in the rendered
post and is what the daemon reads. (Because the byline links to the origin
item, GitHub adds a "referenced in ..." event on that item for every post:
the daemon skips the bot's own cross-references, and for people the trail
on the item shows where its session has posted.)

`ssf launch` links `~/.config/ssf/bin/gh` to the ssf binary and puts that
directory first on the agent's `PATH` (next to it, `ssf` links to the same
binary, so the `ssf` commands the prompts name run the daemon's own build
rather than an older package on the shell's `PATH`; `ssf doctor` says
when the two differ). Invoked as `gh`, ssf prepends the
line to the body of `issue create`, `issue comment`, `pr create`,
`pr comment` and `pr review` (whether given as `--body`, `--body=`, `-b`,
`--body-file` or `-F -`; a review without a body gets one that is only the
line) and runs the real gh with everything else untouched. To pick the
byline's form it works out the repository posted to the way gh does:
`--repo`/`-R`, an item given as a URL, `GH_REPO`, else the checkout's
`origin` remote (`git config --get remote.origin.url`); when none of those
says, the long form is used, which links from anywhere. Beyond that the
wrapper reads only its environment, writes nothing and leaves stdin and the
terminal alone, so it works inside read-only sandboxes and does not break
gh's interactive flows. Outside a session (no `SSF_ISSUE`) it is a plain
pass-through. Bodies that already start with the tag are not stamped
twice, and `ssf guide` tells the agent to add the line itself
whenever it posts some other way (`gh api`, `gh pr create --fill`, an
agent that resets `PATH`).

The daemon parses tags out of every item body and comment it reads, and
honours a tag only where the wrapper puts it: on the first non-blank line of
the body (the first tag on that line, so the wrapper's line, which goes before
anything the agent wrote by hand, is the one read). Failing that, a tag on
the last non-blank line still counts (the last one on that line): posts
made before the byline carried it there, and the daemon re-reads timelines
on relaunch and for delegation report-backs, so they stay attributed. The
first line wins when both carry one. A tag anywhere else, in a fenced or
indented code block, a pasted transcript or a quote reply, is content: it
neither attributes the post nor binds an item to the session it names, and
a bot post whose only tag is quoted counts as untagged. In
`ssf status --json` each tracked item shows `origin` (the session that opened
it, for PRs and issues an agent created), `origins` (timeline event key to
session, for tagged comments and reviews) and `untagged` (posts by the bot
that carry no tag). Untagged bot posts are also noted in the logs and
reported by `ssf doctor`, which additionally checks that the real gh is
installed and that the wrapper links to the running ssf. When posts are shown
to an agent, the byline and tag are stripped and replaced by "(from the
agent on owner/repo#N)".

**A person posting as the bot.** Since every session stamps its posts, a
comment, review or item by the bot login *without* a tag was typed by a
person using the bot account (someone who enrolled their own GitHub account
as the factory's bot, say). It is delivered to agents like any human's post,
with the login as actor and marked "(not from a session)", so the factory
hears that person. It still counts as untagged for `ssf status` and
`ssf doctor`, since nothing distinguishes it from a session whose wrapper was
not in effect, and an untagged item body binds the item to no session.

The tag can carry more fields. Two are defined: `mode=delegate`,
which the wrapper adds when an `issue create` or `pr create` assigns the bot
itself (`--assignee <bot>` or `@me`): the item is a hand-off rather than the
session's own (see [Ownership](sessions.md#ownership-one-session-per-item));
and `role=reviewer`, which the wrapper adds to everything
posted from a reviewer session (`SSF_ROLE=reviewer` in its environment), so
a review by the bot on its own pull request is told apart from the author's
posts and shown as "(from the reviewer session on owner/repo#N)". The
byline does not encode the mode.

# Identity and bylines: who the agent is on GitHub, and which session posted what

How everything git and GitHub inside an agent's process acts as the bot, and how the byline and origin tag tell one session's posts from another's and from a person's. For whoever reads the bot's posts or debugs attribution; agents get the short version from `ssf guide`.

## The bot inside a session

Agents are started through `ssf launch`, which builds an environment in which
everything git and GitHub related is the bot, whatever the person's own
`~/.gitconfig`, `gh auth` or SSH agent say: `GH_TOKEN` and `GITHUB_TOKEN` are
the bot's, a git credential helper (`ssf git-credential`) placed ahead of any
configured one answers HTTPS pushes with the same token, `GIT_SSH_COMMAND` is
pinned to the enrolled bot key with `IdentitiesOnly=yes`, the commit author,
committer and signing key are the bot's (unless `[git]` names a person, below),
`SSF_REPO`, `SSF_ISSUE`, `SSF_ISSUE_URL` and `SSF_BOT` say which item this is,
and a `gh` wrapper first on `PATH` stamps every post with the session's byline.
Git settings go in through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`, which outrank
every config file and apply only inside the agent's process tree. The mechanics
of the `gh` and `git` wrappers, flag parsing, how large bodies travel, and how
they recover the session's environment from an ancestor process when a harness
tool drops it, are in [the gh and git
shims](internals.md#the-gh-and-git-shims). The initial prompt tells the agent
that plain `gh` and `git push` act as the bot, or who `git push` acts as
instead. `ssf token` prints the token for any other use.

The bot's own commits and cross-references are filtered out of follow-up
messages, and its comments are sorted per session by their byline, so an agent's
own posts are not echoed back to it (`daemon.include_own_events` turns both
off). The exception is the catch-up story a session started again is given,
which carries its own posts so it can see what it already said and promised (see
[What the agent is told](prompts.md)).

The bot identity is a default, not a security boundary: on bare metal the agents
run as your Unix user inside your session, so a determined agent can still read
your own gh token from the keyring or use your SSH agent. ssf tells agents to
act only as the bot and to report missing permissions instead. For real
isolation, run the factory [inside a microVM](vm.md).

## Bylines and origin tags: which session posted what

GitHub shows the same bot account for every session, so ssf puts the session in
the content. Everything an agent posts starts with one line that is both a
byline for people and a tag for the daemon, then a blank line:

```
🤖#16 claude/opus/high says: <!-- ssf: origin=owner/repo#16 -->
```

The byline is `🤖#N says:` when the post is on the same repository as the
session's item and `🤖owner/repo#N says:` on another. GitHub renders the item in
either as a link to it, so a reader can tell a session's posts from a person's
at a glance and see which session wrote them, even when the "bot" is someone's
own account. The HTML comment after it, the *origin tag*, is invisible in the
rendered post and is what the daemon reads. Because the byline links to the
origin item, GitHub adds a "referenced in ..." event on that item for every
post: the daemon skips the bot's own cross-references, and for people the trail
on the item shows where its session has posted.

Between the item and `says:`, a session the daemon started also names what it
runs: the harness, and the model and effort it was launched with, `/` between
them and the parts it does not have left out. The byline above is a session on
Claude Code with `--model opus --effort high`; a harness with nothing set reads
`🤖#16 omp says:`, and an effort set with no model keeps the model's place
(`🤖#16 claude/-/high says:`). The harness is the id `--harness` takes
(`ssf agents` lists them), and the model and effort are what the repository's or
the item's overrides had when the session was started, so a config edit shows up
only in the next session's byline, exactly as it does in the daemon's own
`attached` post. A launch that names no harness, `ssf launch` by hand, or a
session ssf cannot say the stack of, keeps the bare `🤖#N says:`.

Where its harness can tell, the byline also says how full the session's
context was when it posted: `🤖#16 claude/opus/high (12% of 1M) says:`. The
`gh` shim reads it from the session's own transcript at the moment of the
post, and leaves it out when it cannot (no transcript, or a model whose window
ssf does not know). Claude Code sessions have it from the latest turn in
`~/.claude/projects/*/<CLAUDE_CODE_SESSION_ID>.jsonl`, and Codex sessions from
the latest `token_count` event in
`$CODEX_HOME/sessions/**/rollout-*-<CODEX_THREAD_ID>.jsonl` (the last turn's
input against the window Codex reports, e.g. `12% of 258k`). Oh My Pi and Pi
sessions that ssf launched have it from the latest assistant turn in the
newest transcript under `$SSF_DELIVERY_MAILBOX/session/`, against the context
window the harness lists for that turn's model: `omp models --json`, or the
`context` column of `pi --list-models` (which Pi rounds, e.g. `262.1K`, so the
percentage is approximate). A listing can take seconds, so ssf caches it in
`~/.cache/ssf/omp-models.json` or `~/.cache/ssf/pi-models.txt` (under
`$XDG_CACHE_HOME` when set) and refreshes it in the background once a day, or
when it lacks the model; a post made before the cache has the model leaves the
usage out. Grok sessions have it from
`$GROK_HOME/sessions/<URL-encoded cwd>/<GROK_SESSION_ID>/signals.json`
(`~/.grok` without `$GROK_HOME`), whose `contextTokensUsed` and
`contextWindowTokens` Grok keeps current (e.g. `5% of 500k`); a command a Grok
subagent runs carries the subagent's session, so the usage shown is its
parent's, the conversation's. Only a command that can post (`issue`/`pr` `create`, `comment`,
`review`) reads any of this.

The three travel to the session as `SSF_HARNESS`, `SSF_MODEL` and `SSF_EFFORT`.
`SSF_HARNESS` is the same variable the [OMP and Pi
launcher](internals.md#per-harness-delivery) names the harness in for the
delivery bridge's injection mode, and it means the same thing here: which
harness this session runs. A launcher used by hand sets it too, and drops a
model and effort inherited from the pane it was run in, since they belong to
that session, so a hand-run launcher's posts read `🤖#N pi says:`.

### Where a tag counts

The daemon parses tags out of every item body and comment it reads, and honours
a tag only where the wrapper puts it: on the first non-blank line of the body,
taking the first tag on that line, so the wrapper's line, which goes before
anything the agent wrote by hand, is the one read. Failing that, a tag on the
last non-blank line still counts (the last one on that line). The first line
wins when both carry one. A tag anywhere else, in a fenced or indented code
block, a pasted transcript or a quote reply, is content: it neither attributes
the post nor binds an item to the session it names, and a bot post whose only
tag is quoted counts as untagged.

In [`ssf status --json`](internals.md#ssf-status---json) each tracked item shows
`origin` (the session that opened it, for PRs and issues an agent created),
`posts_by_session` (how many tagged comments and reviews each session made on
it) and `untagged_posts` (how many posts by the bot carry no tag); `state.json`
keeps the detail behind them as `origins`, a timeline event key to a session,
and `untagged`. Untagged bot posts are noted in the logs and reported by
`ssf doctor`. When posts are shown to an agent, the byline and tag are stripped
and replaced by "(from the agent on owner/repo#N)".

The tag can carry more fields. Two are defined: `mode=delegate`, which the
wrapper adds when an `issue create` or `pr create` assigns the bot itself
(`--assignee <bot>` or `@me`), marking the item a hand-off rather than the
session's own (see [Ownership](sessions.md#ownership-one-session-per-item)); and
`event=<name>`, which only the daemon writes (below). The byline does not encode
the mode.

### A session that took an item over

A [handover](sessions.md#handover) replaces the agent, not the item: the new
session has the same identity, so its posts carry the same `🤖#N ... says:`
byline and the same origin tag as the ones before it, and everything counted per
session on the item keeps adding up. The stack in the byline is the one the new
session was handed over to, so the change of harness is visible there as much as
in the daemon's own `handed-over` and `attached` posts.

### A person posting as the bot

Since every session stamps its posts, a comment, review or item by the bot login
*without* a tag was typed by a person using the bot account, someone who
enrolled their own GitHub account as the factory's bot, say. It is delivered to
agents like any human's post, with the login as actor and marked "(not from a
session)", so the factory hears that person. It still counts as untagged for
`ssf status` and `ssf doctor`, since nothing distinguishes it from a session
whose wrapper was not in effect, and an untagged item body binds the item to no
session.

### The daemon speaking

The third kind of bot post is the daemon's own: the short `ssf` blocks it leaves
on an item when it attaches a session, brings one back, holds its deliveries,
gives up on a binding or releases a workspace (the list is in [What ssf says on
the item](sessions.md#what-ssf-says-on-the-item)). Its first line is the byline
`🤖 ssf`, with no item because the daemon is not a session, and a tag naming the
item posted on with an `event` field:

```
🤖 ssf <!-- ssf: origin=owner/repo#N event=attached -->
```

The `🤖 ssf` byline directly before a tag with the `event` field is what tells it
apart; a bot post with an `event=` tag but a session's byline, a pasted example
say, is the session's. Such a post is neither a person's (it is not delivered as
human input) nor a session's (it is not delivered to the item's session or to
subscribers, is not counted in `posts_by_session` or `untagged_posts`, and is
never taken for an agent's final comment). `daemon.event_comments = false` stops
the daemon posting them.

## Committing as a person while gh stays the bot

Optional setup, off unless you configure it. A factory can drive GitHub as the
bot (issues, comments, PRs, labels, boards) while the commits carry a person's
name, so the history and the contribution graph attribute the work to them
rather than to the bot. In VM mode these commands edit guest configuration: signing-key and token paths
name guest files, and `token:<login>` requires a guest gh sign-in.

The `[git]` table in `config.toml` says who, instance-wide, and a `[repo.git]`
table on a `[[repo]]` overrides it key by key:

```toml
[git]
name = "Ann Person"
email = "ann@example.com"           # verified on Ann's GitHub account (or her id+login@users.noreply.github.com)
# signing_key = "~/.ssh/id_ed25519" # sign with this SSH key; false for unsigned (the default for a person)
# credential = "bot"                # who pushes over HTTPS: bot | token:<gh login> | file:<token file> | a credential helper

[[repo]]
name = "owner/repo"
harness = "claude"
[repo.git]
credential = "token:ann"            # this repository's pushes go out as @ann, with the token gh holds for her
```

```sh
ssf config set git '{ name = "Ann Person", email = "ann@example.com" }'
ssf repo set owner/repo --git-signing-key ~/.ssh/id_ed25519 --git-credential token:ann
ssf repo set owner/repo --clear git            # back to [git]; --clear git.credential for one key
```

What `ssf launch` then does, per repository:

- **Author and committer** are the person, in `GIT_AUTHOR_*`, `GIT_COMMITTER_*`
  and `user.*`, always the same identity. GitHub attributes a commit to the
  account whose verified email is the *author* email, and a committer that is
  someone else reads as "applied by". `name` and `email` go together; setting
  one without the other is refused.
- **Signing** is off for a person unless `signing_key` names a key. A signature
  shows as *Verified* only when the key is registered as a signing key on the
  account that owns the author email, so the bot's key under a person's name
  would be worse than no signature. The key must be readable by the user the
  daemon runs as; a missing file leaves the commits unsigned and says so on
  stderr. `signing_key = false` turns signing off for the bot too.
- **Pushes** are separate from authorship. `credential = "bot"` (the default)
  pushes the person's commits with the bot's token. `token:<login>` pushes as
  that account with the token `gh` holds for it on the machine the agents run on
  (`gh auth token --user <login>`). `file:<path>` reads a token from a file, and
  any other value is used as `credential.helper` verbatim
  (`!gh auth git-credential`, `store`, ...). `ssf git-credential` answers
  according to `SSF_REPO`, so one daemon can push as different people for
  different repositories; outside a session it answers the bot's token whatever
  `[git]` says. SSH remotes are not affected: `GIT_SSH_COMMAND` stays pinned to
  the bot's enrolled key, so pushing as a person means an HTTPS clone URL. Where
  gh keeps tokens in the desktop keyring, a session started by the service may
  not reach them even though `ssf doctor` in a terminal does; if doctor passes
  but pushes fail in a session, use `file:<path>`.
- **`gh` and the API** are the bot in every case: `GH_TOKEN` is the bot's, posts
  carry the bot's byline, the daemon polls as the bot. The daemon's own clones
  and fetches never run under `ssf launch` and stay the bot as well.

Things to know before switching it on:

- The email has to be one GitHub knows as the person's for the avatar, the
  profile link and the contribution graph; an unknown email gives a commit with
  a name and no account behind it.
- Whoever's credential pushes needs write access to the branch, and branch
  protection applies to that account. The bot still needs write access for
  everything `gh` does.
- The [allow-list](configuration.md#who-may-drive-the-factory) is untouched:
  commits are the one timeline event without a login. Ownership binds items to
  sessions through PR heads and origin tags, never through commit authors.
- `token:<login>` puts that person's token within the agent's reach for the
  length of the session. On bare metal that is no wider than what the agent
  already has as your Unix user; in the [VM](vm.md), provision the token inside
  the guest, where its persistent home holds it. Prefer a token scoped to the
  repositories the factory works on.
- The first prompt names who `git push` acts as when it is not the bot (`@ann`
  for `token:ann`, a description for a `file:` token or a helper), so a push
  refused by branch protection is no surprise to the agent.

`ssf doctor` prints the effective identity per repository (who commits, signed
with what, who pushes) and checks the key and the token are there; `ssf config
show` prints the same from the file alone, and `ssf auth status` shows the
instance-wide identity next to the bot's own.

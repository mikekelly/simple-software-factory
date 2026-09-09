# Identity and bylines: who the agent is on GitHub, and which session posted what

How `ssf launch` makes everything git and GitHub inside an agent's process act as the bot (or, for commits, as a person you name), and how the byline and origin tag tell one session's posts from another's and from a person's. For whoever reads the bot's posts or debugs attribution; agents get the short version from `ssf guide`.

## How the agent gets the bot's identity

Agents are started through `ssf launch`, which builds an environment in which
everything git and GitHub related is the bot, whatever the human's own
`~/.gitconfig`, `gh auth` or SSH agent say:

| What | How |
|------|-----|
| `gh` and the GitHub API | `GH_TOKEN`, `GITHUB_TOKEN` (read from gh's keyring for the bot account, or from a pasted token / `SSF_GITHUB_TOKEN`) |
| HTTPS pushes | a git credential helper (`ssf git-credential`) that answers with the token, placed ahead of any configured helper |
| SSH pushes | `GIT_SSH_COMMAND` pinned to the enrolled bot key with `IdentitiesOnly=yes` |
| Commit author and committer | `GIT_AUTHOR_*`, `GIT_COMMITTER_*` and `user.name`/`user.email`: the bot's login and email, unless `[git]` names a person (below) |
| Commit signing | `gpg.format=ssh`, `user.signingkey=<bot key>`, `commit.gpgsign=true` (or `commit.gpgsign=false` when no key is enrolled, so nothing is signed with the human's key); a person's key when `[git]` gives one |
| Which issue this is | `SSF_REPO`, `SSF_ISSUE`, `SSF_ISSUE_URL`, `SSF_BOT` |
| Which session posted what | a `gh` wrapper first on `PATH` that starts every post with the byline (below) |

Git settings go in through `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`, which
outrank every config file, and only inside the agent's process tree. The
initial prompt tells the agent that plain `gh` and `git push` act as the bot
(or, with a person's credential, who `git push` acts as instead).
The engine is the sole writer of `state.json`'s `bot_login`; auth commands
write the credential and configuration, never the live daemon state.
The bot's own commits and cross-references are filtered out of follow-up
messages, and its comments are sorted per session by their byline, so
an agent's own posts are not echoed back to it (`daemon.include_own_events`
turns both off). `ssf token` still prints the token for any other use.

The bot identity is a default, not a security boundary: on bare metal the
agents run as your Unix user inside your session, so a determined agent can
still read your own gh token from the keyring or use your SSH agent. ssf
tells agents to act only as the bot and to report missing permissions
instead. For real isolation, run the factory [inside a microVM](vm.md).

## Committing as a person while gh stays the bot

A factory can drive GitHub as the bot (issues, comments, PRs, labels,
boards) while the commits carry a person's name, so the history and the
contribution graph attribute the work to them rather than to `acme-bot`.
The `[git]` table in `config.toml` says who, instance-wide, and a
`[repo.git]` table on a `[[repo]]` overrides it key by key:

```toml
[git]
name = "Ann Person"
email = "ann@example.com"           # verified on Ann's GitHub account (or her id+login@users.noreply.github.com)
# signing_key = "~/.ssh/id_ed25519" # sign with this SSH key; false for unsigned (the default for a person)
# credential = "bot"                # who pushes over HTTPS: bot | token:<gh login> | file:<token file> | a credential helper

[[repo]]
name = "acme/widgets"
harness = "claude"
[repo.git]
credential = "token:ann"            # this repository's pushes go out as @ann, with the token gh holds for her
```

```sh
ssf config set git '{ name = "Ann Person", email = "ann@example.com" }'
ssf repo set acme/widgets --git-signing-key ~/.ssh/id_ed25519 --git-credential token:ann
ssf repo set acme/widgets --clear git            # back to [git]; --clear git.credential for one key
```

What `ssf launch` then does, per repository:

- **Author and committer** are the person, in `GIT_AUTHOR_*`,
  `GIT_COMMITTER_*` and `user.*`. They are always the same identity:
  there is no way to make them differ. GitHub attributes a commit to the
  account whose verified email is the *author* email, and a committer that
  is someone else reads as "applied by", which is not what attribution is
  for. `name` and `email` go together; setting one without the other is
  refused.
- **Signing** is off for a person unless `signing_key` names a key. A
  signature only shows as *Verified* when the key is registered as a
  signing key on the account that owns the author email, so the bot's key
  under a person's name would be worse than no signature. The key must be
  readable by the user the daemon runs as; a missing file leaves the
  commits unsigned and `ssf launch` says so on stderr, `ssf doctor` before
  that. `signing_key = false` turns signing off for the bot too.
- **Pushes** are separate from authorship. `credential = "bot"` (the
  default) pushes the person's commits with the bot's token, as today.
  `token:<login>` pushes as that account with the token `gh` holds for it
  on the machine the agents run on (`gh auth token --user <login>`; the
  bot's `GH_CONFIG_DIR` and `GH_TOKEN` are set aside for that one lookup).
  `file:<path>` reads a token from a file. Any other value is used as
  `credential.helper` verbatim (`!gh auth git-credential`, `store`, ...).
  `ssf git-credential` answers according to `SSF_REPO`, so one daemon can
  push as different people for different repositories; without `SSF_REPO`
  (outside a session) it answers the bot's token, whatever `[git]` says.
  SSH remotes are not affected: `GIT_SSH_COMMAND` stays pinned to the
  bot's enrolled key, so pushing as a person means an HTTPS clone URL.
  `token:<login>` reads gh's store from inside the session; where gh keeps
  tokens in the desktop keyring, a session started by the systemd unit may
  not reach it even though `ssf doctor` in a terminal does. If doctor
  passes but pushes fail in a session, use `file:<path>` instead.
- **`gh` and the API** are the bot in every case: `GH_TOKEN` is the bot's,
  posts carry the bot's byline, the daemon polls as the bot. The daemon's
  own clones and fetches never run under `ssf launch` and stay the bot as
  well.

Things to know before switching it on:

- The email has to be one GitHub knows as the person's for the avatar,
  the link to their profile and their contribution graph; an unknown
  email gives a commit with a name and no account behind it.
- Whoever's credential pushes needs write access to the branch, and
  branch protection (required status, "restrict who can push") applies to
  that account. The bot still needs write access for everything `gh` does.
- The [allow-list](configuration.md#who-may-drive-the-factory) is
  untouched: commits are the one timeline event without a login and pass
  whoever authored them. Ownership binds items to sessions through PR
  heads and origin tags, never through commit authors.
- `token:<login>` puts that person's token within the agent's reach for
  the length of the session (the helper hands it to git, and an agent can
  call the helper). On bare metal that is no wider than what the agent
  already has as your Unix user; in the [VM](vm.md) the token is copied
  onto the seed disk, so the guest holds it. Prefer a token scoped to the
  repositories the factory works on.
- The first prompt names who `git push` acts as when it is not the bot
  (`@ann` for `token:ann`, a description for a `file:` token or a helper),
  so a push refused by branch protection is no surprise to the agent.

`ssf doctor` prints the effective identity per repository (who commits,
signed with what, who pushes) and checks that the key and the token are
there; `ssf config show` prints the same lines from the file alone, and
`ssf auth status` shows the instance-wide identity next to the bot's own.

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
"bot" is someone's own account. Posts from before #42 have the byline
without `says:`, and posts by the reviewer sessions of before #115 read
`🤖#N (reviewer) says:`; the daemon reads both the same way.
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
`pr comment` and `pr review` (and `issue new` and `pr new`, gh's own
names for the same two creates), in every spelling gh takes the body in
after the command words:
`--body`, `--body=`, `-b`, `-b=`, `-bX`, `--body-file`, `--body-file=`,
`-F`, `-F -`, `-F=` and `-FX`,
and any of those behind the value-less letters of a cluster, so a
review's `-ab hi` and a create's `-eF notes.md` are stamped as much as
`-b hi` is. Which letters are value-less depends on the command, since
`-a` approves on a review and names an assignee on a create. Flags before the command words are handled too: after locating
those words, the shim binds separated values the way gh does, so
`gh -ab pr review hi` stamps `hi`.

The last repeated `--body-file` wins, as in gh; earlier files and stdin
are not read. Mixing `--body` and `--body-file` is passed through for gh
to reject. Invalid UTF-8 produces an explicit error rather than posting
altered or untagged text. An ordinary unreadable file is left for gh to
report; a stdin read error stops the shim because stdin may already have
been consumed.

Large stamped bodies travel through an inherited anonymous file instead
of argv, avoiding operating-system argument limits. Linux uses a memory
file; other Unix systems use a private temporary file that is immediately
unlinked. Failure to prepare that file stops the command without posting.
GitHub still enforces its body-size limit. A body gh builds for itself
(`--fill`, `--fill-first`, `--fill-verbose`, `--editor`, `--web`, or the
interactive prompt) currently carries no byline unless the agent supplies
one itself; the shim does not replace commit-generated text with an empty
byline-only body.

An approving review needs no body of its own, so one that is only the
line is added, in every spelling of the approval: `--approve`,
`--approve=true`, `-a`, `-a=true` and the `-a` inside a cluster all
count, while `--approve=false` does not, because gh does not read it as
an approval either. The shim rejects explicit empty or whitespace-only bodies on comment and
request-changes reviews before posting. Missing bodies are left for gh to
reject (`body cannot be blank
for comment review`) and that refusal is the more useful answer: a
review carrying nothing but a byline says nothing, and a request for
changes carrying nothing but a byline blocks the pull request. So `gh pr
review 3 --comment` fails and asks for a body, where it once posted.

It runs the real gh with everything else untouched. To pick the
byline's form it works out the repository posted to: `--repo`
or `-R` in any spelling, an item given as a URL, `GH_REPO`, else the
checkout's `origin` remote (`git config --get remote.origin.url`); when
none of those says, the long form is used, which links from anywhere.
That is gh's own list, but not quite gh's reading of it: gh prefers the
URL to `--repo`, and takes the last `--repo` where this takes the
first, and a `--repo` that will not parse ends the search here rather
than letting a URL further along answer. Each needs a line naming the
repository twice over, or naming it unparseably, which is not a line an
agent writes; the cost is the short form on a post landing elsewhere,
never a lost tag.

The URL has to be the item's own: a URL that is the value of a flag
taking one is not read as the item, which matters
most for `--parent`, `--blocked-by` and `--blocking` (the flags `gh
issue create` documents as taking numbers or URLs), since that answer
outranks `GH_REPO` and a post landing elsewhere with the short `#N`
would link to that repository's issue N. An item after `--` still
counts, as it does for gh. Beyond that the
wrapper reads its environment and explicit body files or stdin. It leaves
the terminal alone and does not break gh's interactive flows. Small bodies
need no writable filesystem; large bodies use the anonymous-file transport
described above. Outside a session (no `SSF_ISSUE`) it is a plain
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
it, for PRs and issues an agent created), `posts_by_session` (how many
tagged comments and reviews each session made on it) and `untagged_posts`
(how many posts by the bot carry no tag); `state.json` keeps the detail
behind them as `origins` (timeline event key to session) and `untagged`. Untagged bot posts are also noted in the logs and
reported by `ssf doctor`, which additionally checks that the real gh is
installed and that the wrapper links to the running ssf. When posts are shown
to an agent, the byline and tag are stripped and replaced by "(from the
agent on owner/repo#N)".

**A session that took an item over.** A
[handover](sessions.md#handover) replaces the agent, not the item: the
new session has the same identity, so its posts carry the same
`🤖#N says:` byline and the same origin tag as the ones before it, and
everything counted per session on the item keeps adding up. The change of
harness is visible only in the daemon's own `handed-over` and `attached`
posts.

**A person posting as the bot.** Since every session stamps its posts, a
comment, review or item by the bot login *without* a tag was typed by a
person using the bot account (someone who enrolled their own GitHub account
as the factory's bot, say). It is delivered to agents like any human's post,
with the login as actor and marked "(not from a session)", so the factory
hears that person. It still counts as untagged for `ssf status` and
`ssf doctor`, since nothing distinguishes it from a session whose wrapper was
not in effect, and an untagged item body binds the item to no session.

**The daemon speaking.** The third kind of bot post is the daemon's own:
the short `ssf` blocks it leaves on an item when it attaches a session,
brings one back, holds its deliveries, gives up on a binding or releases
a workspace (the list is in
[What ssf says on the item](sessions.md#what-ssf-says-on-the-item)). Its
first line is the byline `🤖 ssf`, with no item because the daemon is not
a session, and a tag naming the item posted on with an `event` field:

```
🤖 ssf <!-- ssf: origin=owner/repo#N event=attached -->
```

The `🤖 ssf` byline directly before a tag with the `event` field is what tells it apart (a bot post with an `event=` tag but a session's byline, a pasted example say, is the session's): such a post is neither a
person's (it is not delivered as human input) nor a session's (it is not
delivered to the item's session or to subscribers, is not counted in
`posts_by_session` or `untagged_posts`, and is never taken for an agent's
final comment). `daemon.event_comments = false` stops the daemon posting
them.

The tag can carry more fields. Two are defined: `mode=delegate`, which
the wrapper adds when an `issue create` or `pr create` (under either
name) assigns the bot itself (`--assignee <bot>` or `@me`): the item is
a hand-off rather than the session's own (see [Ownership](sessions.md#ownership-one-session-per-item));
and `event=<name>`, which only the daemon writes (above).
Posts made before #115 by a reviewer session carry `role=reviewer`; the
field is read and ignored, so such a post counts as the item's session's.
The byline does not encode the mode.

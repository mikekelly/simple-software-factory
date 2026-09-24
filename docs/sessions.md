# Sessions: ownership, subscriptions, handover, release and purge

The item lifecycle: which session acts on an item, how sessions follow and message each
other, how an item changes harness, and what happens to a workspace when the item
closes. For an agent operating a factory, or a session that has read `ssf guide` and
needs the whole rule. Offline route: `ssf skill sessions`.

## Ownership: one session per item

### Adopting items when a repository is enrolled

Adding a watched repository enrolls it. On the first successful poll, ssf lists the
items already involving the bot as adoption candidates and starts none of them, so a
second factory watching the same repository does not pick up work in flight.

```sh
ssf candidates --repo owner/repo
ssf adopt owner/repo#N owner/repo#M
```

`ssf candidates` prints the queue (`--repo` narrows it, `--json` gives the rows as
data). `ssf adopt` takes one or more explicit `owner/repo#N` references and gives those
items to this factory. Ask the person first whether another factory is working them.
Each adopted item starts a fresh harness conversation whose first message reconstructs
the item and its timeline from GitHub; no transcript is copied from anywhere. Everything
after that, new items, daemon restarts, reactivating released sessions, is automatic.

### What binds an item to a session

GitHub gives the bot one identity, so without a rule a person's @mention or review
request on a pull request the bot wrote would start a second agent beside the one that
wrote it. ssf binds every item to at most one owning session, decided once when the item
is first seen. First binding wins.

| Case | Bound to |
|---|---|
| A pull request whose body carries a session's origin tag | that session |
| A same-repo pull request whose head branch is a tracked workspace's branch | that workspace's session |
| An item the bot is assigned to or mentioned on, with no owner yet | a new session of its own |
| An assignment, mention or review request on an item that already has an owner | the owner, as activity |
| An item a session creates *and assigns the bot to* in the same `gh` command | a fresh session; the creating session is subscribed to it |
| Anything else (a bot-opened issue nobody triggered, no usable tag or branch) | nothing; it is left alone |

Some consequences worth knowing:

- **The bot's own items are polled.** A session hears about the pull request it opened
  without anyone assigning or mentioning the bot: one message saying the item is now
  tracked for it, then every later comment, review, review request, assignment and the
  closure, into the same agent, with `SSF_ISSUE` unchanged. An issue opened without
  assigning the bot stays unbound; its origin tag records attribution only.
- **Triggers go to the owner.** An owner that has retired, or whose workspace was
  removed, is brought back (workspace re-created from its branch, conversation resumed)
  rather than replaced, and cannot be released or purged while items bound to it are
  still open.
- **Hand-offs.** An item created with `--assignee <bot-login>` by a session is a
  delegation: its tag carries `mode=delegate`, it gets its own session, and the creating
  session is subscribed to it. The parent hears when the child moves (its closure
  included) and gets one message with the outcome and the child's final comment when it
  closes; what is said on the child reaches the child's own session, not the parent. The
  child is told it was handed off and to leave a clear final comment. An agent that
  wants a separate worker therefore uses `--assignee`; an unassigned issue is a
  placeholder with no session. A session opening the pull request it will merge itself
  must leave `--assignee` off: the assignment starts a second session, on the same
  branch and in the same checkout, which redoes the verification and the review the
  first has already done. Its second pair of eyes is a reviewer subagent (see [Second
  opinions](#second-opinions)).
- **Unbound items are revisited.** An item left alone is looked at again when it changes
  on GitHub *or* when it appears on another listing, whichever comes first, because an
  assignment can be older than the `updated_at` the item was ignored with. A listing
  that comes back short is not taken as the item being gone. Once the item leaves the
  listings, ssf forgets a record that answers to nothing — no session, no subscriber, no
  workspace, no prompt ever sent or attempted — instead of keeping it on `open` for
  ever: a closed or merged item stops showing in `ssf status`, `ssf peers --all` and the
  dashboard. A record with a session stays, as it does today, for the retirement and the
  workspace's release.
- **No cross-repository binding.** An item opened from a session on a different
  repository is never bound to it.

`ssf status --json` shows the binding as `owner` / `shares_workspace_of` and hand-offs
as `delegated_by` (see [`ssf status --json`](internals.md#ssf-status---json)); `ssf
peers` prints them as "owned by ..." and "handed off by ...". The
[dashboard](dashboard.md) groups items into one card per owning session, including a
retired origin that still owns active items; subscriber-only items make no card.

Item activity reaches a live session through the harness's own channel where there is
one, and through its terminal otherwise; see [per-harness
delivery](internals.md#per-harness-delivery).

## Second opinions

ssf runs one session per item and starts no second session on a pull request a session
wrote: a review requested on an owned pull request is delivered to its owner as
activity. The session arranges its own review according to the repository's notes in its
`SSF.md` (see [`SSF.example.md`](../SSF.example.md) for the template and what it says
about review). A pull request the bot did *not* write, a person's pull request the bot
is asked to review, or one assigned to it with no session on its branch, gets a session
of its own on that branch, which reviews when asked.

## Subscriptions and cross-session comments

Exactly one session acts on an item; any number can hear about it. Each item carries a
list of subscriber sessions next to its owner, and, for any of them that asked for more
than the default, the level it asked for. Every delivery about the item (new activity, its
closure, the bot being dropped from it, or the item getting a session of its own) is fanned
out to the subscribers with FYI framing: `[ssf] FYI: new activity on <item>:`, `[ssf] FYI:
<item> has been closed.`, and so on, each ending with one line saying it is for information
only and how to stop them. A level decides only what counts as that *new activity*; the
lifecycle notices always go. Subscriptions live in the state file, so they survive
relaunches and a session being brought back; a session that retires is unsubscribed
everywhere.

The CLI takes the session identity from `SSF_REPO`/`SSF_ISSUE` inside a session, or
`--as owner/repo#N` from a shell (an item bound to another session counts as that
session):

```sh
ssf sub owner/repo#N       # or a bare N inside a session
ssf sub N --events all     # ... and hear the comments, reviews and commits too
ssf unsub N
ssf subs --json
```

- `ssf sub` / `ssf unsub` follow or drop an item. Every FYI is a prompt in the
  follower's own session, so a follow defaults to the item's own state changes:
  closed, merged, reopened, assigned, labeled, renamed, a review request moving.
  What is *said* on the item — comments, reviews, review-line comments, commits,
  references — is not delivered unless the follow asks for it with `--events
  all`; an event kind ssf does not classify is delivered either way. Following
  an item this session already follows changes its level, and nothing that was
  withheld before the change is replayed. `ssf subs` shows the level of each
  follow. Subscribing to an item nothing tracks yet makes it tracked as
  *subscriber-only*: polled every pass for activity, no workspace, no owner, and
  nothing before the subscription is replayed. If the bot is later assigned to
  it, it gets a session as usual and the subscribers are told; when nobody
  follows it any more it is dropped.
- `ssf subs` lists what this session follows and who follows its items; `ssf peers`
  shows subscribers per session.
- Delegating parents are subscribed to their children automatically, at the
  default level: a parent hears when its child moves (including its closure, as
  before) and not every comment between. A parent that asks for more on its
  child with `ssf sub <n> --events all` keeps that level: the hand-off
  subscription is written once, so re-onboarding the child (after a run of
  delivery failures gave its binding up, say) does not put the parent back.

`sub`, `unsub`, `handover`, `assign`, `release` and `purge` talk to the running daemon
over a Unix socket in the state directory (`ssf.sock`), because the daemon owns the
state and the delivery path; `ssf doctor` reports whether it answers. `subs`, `peers`
and `status` read the state file and work without it.

**Cross-session comments.** Comments the bot posts carry the origin tag of the session
that made them (see [Identity and bylines](identity-and-bylines.md)), and delivery is
decided per recipient rather than by author. A bot comment whose tag names a different
session is delivered like a person's, labelled "(from the agent on owner/repo#M)". A
comment tagged with the recipient's own session is the self-echo and stays filtered. An
untagged bot comment is a person's and reaches every recipient, marked "(not from a
session)". So one session talks to another by commenting on its issue with `gh`: the
other agent receives it labelled as coming from the first, and the first does not get
its own comment back, even when it is subscribed. This is the only channel between
sessions, and `ssf guide` says so.

## Directing an item: comments, `ssf handover`, `ssf assign`

A comment is a comment. ssf reads no command out of one, whatever its first line says: a
`/ssf <request>` comment, on an item attached to a session now or left months ago, is
ordinary activity, delivered like anything else a person wrote. Nothing about it is
stored, run or replayed on an attach, resume, relaunch or handover.

Comments are how people and sessions collaborate: the session acts on what it is told
there. Changing the stack an item runs on, or giving an item that has no session its
first one, is a terminal command instead:

- `ssf handover` moves a session's item to a new session on another harness, model or
  effort ([Handover](#handover)).
- `ssf assign` starts the first session of an item that has none, on the stack you name
  ([Assigning a stack before there is a
  session](#assigning-a-stack-before-there-is-a-session)).

Both write the item's launch overrides, which every later launch, resume and re-creation
uses. A session can run either one itself: `ssf handover` from inside a session is the
ordinary way an item changes stack.

## What ssf says on the item

Most of what the daemon does is only in its journal. The moments a person reading the
issue needs are posted on the item itself, as the bot, so the timeline tells the whole
story. Each is one short comment: the byline `🤖 ssf` and one fenced `ssf` block of `key:
value` lines, nothing else.

> **bot-login** commented
>
> 🤖 ssf
>
> ```ssf
> ssf attaching agent to issue:
> harness: <harness>
> model: <model>
> effort: <level>
> driver: herdr
> branch: bot/issue-N-short-title
> ```

The events, and nothing else:

| Event | When | Lines |
|-------|------|-------|
| `attached` | a session is started for the item: on onboarding (`ssf attaching agent to issue:`), or again once its workspace had to be re-created or was kept (`ssf attaching agent to issue again:`) | `harness`; `model` and `effort` as configured, or `the harness's default` (`command:` when the repository sets one, and then `the command's`); `driver`; `branch`; `handed off from: owner/repo#M` for a delegated item; `handed over from: <harness>` after a handover; on a re-creation `re-created: workspace gone` and `conversation: resumed` or `fresh`; on a kept workspace `workspace: kept` and `conversation: resumed`, `fresh` or `kept` |
| `attached` | a pull request bound to another item's session rather than given one of its own | `session: owner/repo#M`, `shares: workspace of #M` |
| `resumed` | the harness was started again in its existing workspace: the startup pass after a daemon or machine restart, or a terminal found gone at delivery time | `harness`, `conversation: resumed` or `fresh`, `after: restart` or `after: lost terminal` |
| `blocked` | deliveries are held because the harness is at its sign-in prompt, its first-run setup is incomplete, or it could not be started at all | `harness`; `reason: not signed in` with `fix:` the command that signs it in, `reason: setup incomplete`, or `reason: could not be started: <error>` with `fix: start <harness> by hand in the workspace, or fix the model or effort and hand over again` |
| `unblocked` | the hold is lifted | `harness`, `held for`, and `conversation: resumed`, `fresh`, `kept` (a person signed in at the terminal) or `handed over` |
| `gave-up` | five looks at the item in a row failed and its binding is dropped; the item is onboarded afresh on its next look | `failures`, `last error`, `next: re-onboarding the item` |
| `released` | the workspace was removed by `ssf release` or `ssf purge`, posted on the session's own item | `by:`, `forced: yes` when `--force` was passed, `branch` |
| `handed-over` | the daemon carried out a pending [handover](#handover), or refused one | `from`, `from model`, `from effort` (as they were, or `the harness's default`, or `the command's` with a configured command named on `from command`); `to`, `to model`, `to effort`, `to command`; `summary: yes` or `no`; `by: owner/repo#N` for a session, `a person at the terminal` for an operator. A refusal has the `to` lines, `by` and `refused:` with the reason, and no `from` lines |

When the harness a pane is running is not the one the item's record names, a config edit
under a live session, only `from` is written, followed by `from stack: unknown (the
harness on the pane is not the record's)`: ssf has no model, effort or command of that
session's to report.

The first line carries the origin tag with an `event` field (`🤖 ssf <!-- ssf:
origin=owner/repo#N event=attached -->`, see [Identity and
bylines](identity-and-bylines.md#bylines-and-origin-tags-which-session-posted-what)),
and the daemon reads it back: an event post is not a person typing as the bot, and it is
not activity. It is delivered to no session, `ssf status` does not count it, and it is
never taken for an agent's final comment when a hand-off closes. Every event is posted
at the moment it happens, from records the daemon already keeps, so a daemon restart
reposts nothing.

`daemon.event_comments = false` turns the posts off for every repository; `ssf repo set
owner/repo --event-comments false` for one. Nothing else changes, the hold on a blocked
session included, and every event is still in the journal.

## Branch conflicts

ssf checks whether an active session's committed branch would conflict with the
repository's base on origin. A clean merge produces no message, even when the branch is
far behind; a conflict produces a `[ssf]` message in the session, naming the base commit
and the conflicting files. ssf never rebases for the session.

```sh
ssf config set daemon.conflict_check_interval_secs 300   # the default; 0 turns checks off
```

Set `conflict_check_interval_secs` on a `[[repo]]` to override it for one repository.
These terminal notices are independent of `event_comments`, which controls posts on
GitHub.

Each interval fetches the base once per repository with eligible sessions. The base is
`repo.base_branch`, or origin's default branch when unset. Local git checks compare
committed branch tips, including unpushed commits; uncommitted edits are outside the
check, and the merge simulation leaves the working tree and index alone. A delivered
notice is remembered across daemon restarts, so an unchanged divergence is not announced
every pass. Retired and released sessions receive nothing, items sharing a session do
not produce duplicate notices, and blocked sessions and sessions awaiting handover are
skipped.

## A harness that is not signed in

A harness login can go away under a running session: the token expires, or it is revoked
(a logout elsewhere on a copied credential does that, see [Inside a
microVM](vm.md#harness-logins)). The harness then shows its sign-in screen and waits.
Nothing inside the session can fix it, and to the driver the agent looks alive and idle,
so without help ssf would keep pasting activity into a terminal that cannot act.

So every pass, for each session whose agent is idle, ssf reads the bottom of its screen
and, if it shows that harness's sign-in prompt, marks the session **blocked**. The
phrases are the harnesses' own, and they are checked against the harness the driver
reports for that pane, not the one the item's record would launch, because a config edit
under a live session leaves the pane on the harness it started with. Two things keep an
agent's own screen from tripping this: only the bottom of an idle agent's screen counts,
and a line carrying ssf's `> ` quote marker or inside echoed `[ssf]` text is skipped.
The item's own words are relayed under that marker, so a phrase someone quoted in a
comment is not read as the harness's prompt. What remains is an agent quoting the exact
phrase itself, which costs one `blocked` post and one restart, and nothing more.

- **Told once.** One `blocked` post lands on the item (see [What ssf says on the
  item](#what-ssf-says-on-the-item)) with the harness, `reason: not signed in` and
  `fix:` the command to run. The log gets a warning, and the session shows as blocked in
  `ssf status` (a `BLOCKED:` line naming the harness, since when and the command;
  `--json` carries `blocked` on the session and `blocked_sessions` at the top), in `ssf
  peers` and on the dashboards.
- **Nothing is delivered.** Activity, FYIs and the closing message are held, and the
  item's bookkeeping is left as it was, so everything that happened meanwhile is
  delivered in full once the session is back. A delivery to a blocked session is refused
  with the reason.
- **Checked every pass.** ssf asks each harness in use where it runs (the host, or the
  guest in VM mode): its own status command, its credential file from the [login
  table](vm.md#harness-logins), or an API key in the environment. When the login is
  back, ssf quits the stuck harness, starts it again with its conversation resumed,
  gives it one message saying what happened, and fetches every listing in full so the
  held activity follows. If the restart comes back to the prompt, the item is not told
  again and the next attempt waits twice as long: ten minutes, twenty, forty, then
  hourly. A person who signs in at the terminal lifts the block on the next pass with no
  restart. Either way the item gets the `unblocked` post, saying how long the hold
  lasted.

`ssf doctor` prints one line per harness in use: signed in, `FAIL ... not signed in ...`
with the command to run, or `note ... cannot tell` for a harness ssf has no check for.
The harnesses in use are the ones the configured repositories name plus any a handover
or an assignment put on an item; the line for one of those says which item put it there
and how, and such a harness is checked for being installed too. With the factory in a VM
the check is forwarded into the guest, so it happens where the agents are.

## Handover

An item can change stack without changing workspace. `ssf handover` asks the daemon to
end the session working on the item and start a new one on another harness, model or
effort, in the same worktree, on the same branch, with a summary the outgoing session
writes. A person asks for it on the issue and the agent runs one command; an operator
does the same from a shell. An item with no session yet takes its first stack from [`ssf
assign`](#assigning-a-stack-before-there-is-a-session) instead.

```sh
ssf handover --harness <harness> --model <model> --effort <level> --summary "..."
ssf handover owner/repo#N --harness <harness> --no-summary
ssf handover N --harness <harness> --summary-file /path/to/handover.md
ssf handover N --cancel
```

- **Which item.** Inside a session the command takes no item: it is the session's own.
  From a shell the item comes first, as `owner/repo#N` or a bare `N` with `SSF_REPO` set
  or `--as owner/repo#N`, exactly like `ssf release`. An item bound to another session's
  workspace counts as that session.
- **Which stack.** `--harness` is required; the same harness with a different model or
  effort is a valid handover. `--model` and `--effort` are optional and are checked the
  way `ssf repo set` checks them: the effort level against the levels that harness
  offers, the model id for its shape only, since new ids appear before any catalogue
  does (`ssf agents` lists the harnesses, `ssf models <harness>` the models). A model id
  the harness itself rejects shows up later, as the harness failing to start. Left out,
  the new session runs on that harness's defaults; see [Per-item
  overrides](harnesses.md#per-item-overrides) for how they combine with the repository's
  settings.
- **The summary.** Exactly one of `--summary`, `--summary-file` and `--no-summary` is
  required, so nobody hands an item over without deciding. The summary is the first
  thing the new session reads, before the item itself, and it is written for an agent
  that has never seen the work: what the item is about, what is done, what is left,
  where things are (branch, pull request, files, what is unverified). It is at most
  8,000 characters; an over-long or empty one is refused with the cap and the count.
  `--summary-file` is read where the command runs, so with the factory in a
  [microVM](vm.md) the path is a path in the guest.

The command answers as soon as the daemon has recorded the handover (`--json` gives the
same as data), and tells the outgoing agent that the handover is pending, not done: it
should stop working now, because anything it starts is thrown away with its pane.

**Refused straight away**, with the reason, and nothing recorded:

- the item has no running session (use `ssf assign` to give it its first);
- the harness id is not one ssf knows, the effort level is not one that harness offers,
  or the model id is not shaped like one;
- the harness is not installed where the daemon runs, or its login probe says it is
  signed out (in VM mode this is the guest's login, see [A harness that is not signed
  in](#a-harness-that-is-not-signed-in));
- a handover on the item is already pending, or a release is;
- the item is already on that harness with that model and effort, judged against the
  harness its pane is running rather than the one its record would launch: `ssf handover
  <item> --harness <the configured one>` is the accepted way to bring a session left
  behind by a config edit onto the configured stack;
- the summary is longer than 8,000 characters, or would read as a harness's own sign-in
  screen. The summary is pasted into the new session's terminal, where ssf reads the
  bottom of the screen for exactly those phrases, so such a summary would hold the new
  session's deliveries; the refusal names the line and asks for it to be reworded.

A session that is itself **blocked** on a sign-in prompt may hand over: that is one way
out of the block, so the check is on the target harness, not the one being left.

**On the next pass** (within `daemon.poll_interval_secs`, and before the repository's
items are polled) the daemon:

1. checks the item is still active, its workspace still known, and that the driver can
   say what is running in it;
2. ends the outgoing agent's pane, leaving the worktree and its branch exactly as they
   are;
3. retires the outgoing session on the record (its conversation id, terminal and any
   block go; the worktree, branch, driver and subscribers stay) and stores the target as
   the item's per-item overrides, so every later launch, resume and re-creation uses the
   new stack. The retired conversation, and the last one the outgoing harness wrote in
   that workspace, are both remembered as never to resume: that transcript is the one
   lying there for the next harness to adopt;
4. starts the new session in the same worktree, with what the item's overrides make of
   the repository's launch settings (another harness runs its own permission-free
   command; the same harness keeps the repository's `command`), and gives it the
   [handed-over first prompt](prompts.md): the summary, if there is one, then the item's
   story, which then counts as seen;
5. posts `handed-over` and then the new session's `attached` on the item.

If the item has closed or the bot was dropped from it meanwhile, the workspace is gone
and cannot be brought back, the item cannot be read, or the outgoing agent cannot be
stopped, the handover is **refused at that point**: one `handed-over` post carrying
`refused: <reason>`, and, if the old agent is still there, one `[ssf] Handover to
<harness> refused: <reason>. Carry on.` message to it. The old session keeps the item.

If the new harness comes up on its own sign-in screen, the `handed-over` post is made
(the handover did happen) and the item is **blocked** the usual way. The old session is
not brought back: the item is on the new harness from here on, and signing that harness
in is what starts it. If the new harness cannot be started **at all** (it exits the
moment it is launched, because the model id is one it refuses, say), the item is blocked
the same way with `reason: could not be started: <error>`, and there is no `attached`
post, because no session attached. Recovery is the same restart on the same backoff -
ten minutes, twenty, forty, then hourly, and the block lifts as soon as one of those
takes the prompt.

Under both blocks the summary waits with the item: it is kept on the record until a
session has actually read it, so the harness started again later is given the summary
and then the story. A message that went into a sign-in screen was never read, so it does
not count, and while a summary is waiting the block lifts on that first message landing
rather than on the screen looking idle.

Handing the item over again is the way out of either block. The new session is told it
took over from the session that did the work, since a harness that never came up wrote
nothing. A summary still waiting is carried forward: with `--no-summary` the new session
is given the one nobody has read, and the `handed-over` post says `summary: yes`. A
second handover that writes a summary replaces it, so where both matter, write the new
summary with what the old one said in it. `ssf status` and `ssf peers` show an unread
summary on the item's line (`handover note waiting: from <harness>, summary 1234
chars`).

**Calling it off.** `ssf handover [ITEM] --cancel` drops a handover the daemon has not
carried out yet: nothing about the item changes, and the session that is there is told
in one `[ssf] The handover to <harness> was cancelled: this session keeps the item.
Carry on.` message, since it was told to stop working when the handover was recorded.
The flags that describe a handover are refused with `--cancel`, as is a cancel with
nothing pending. Nothing is posted on the item: the handover was never announced there.

While a handover is pending, `ssf release` on the item is refused with that as the
reason, and the startup pass leaves the item alone rather than resuming the old harness
only to stop it. The overrides last until a later command writes new ones: releasing or
purging leaves them on the record, so a workspace re-created afterwards comes back on
the same stack.

## Assigning a stack before there is a session

An item's stack can also be chosen before it has a session at all, so its *first*
session comes up on it instead of on the repository's. `ssf assign` assigns the bot on
GitHub and writes the item's per-item overrides in the same request; the daemon answers
the CLI between polls, so the two cannot be separated by a pass that onboards the item
with the old stack.

```sh
ssf assign N --harness <harness> --model <model> --effort <level>
ssf assign owner/repo#N --harness <harness> --json
```

- **Which item.** The item comes first, as `owner/repo#N` or a bare `N` with `SSF_REPO`
  set or `--as owner/repo#N`, like `ssf handover` and `ssf release`.
- **Which stack.** `--harness` is required; `--model` and `--effort` are optional and
  checked exactly as `ssf handover` checks them, and then, in the daemon, against what
  is installed and signed in where the daemon runs. A stack equal to the one the item
  would run anyway is assigned *without* writing overrides, so asking for the
  repository's own stack leaves the item following `ssf repo set`.
- **The assignment stays in `gh`'s hands.** ssf opens no issue: create it with `gh issue
  create`, then assign it a stack. `gh issue create --assignee <bot-login>` still works
  on its own, and is what a session uses to hand work off. `ssf assign` is for when that
  first session should not come up on the repository's stack: an orchestrator item, an
  architectural review, a deep audit. There is no `--cancel`: the inverse is `gh issue
  edit --remove-assignee <bot-login>`, and once there is a session, `ssf handover` is
  the tool.
- **A closed item** is assigned and given its stack like any other, but no pass onboards
  one (every listing ssf reads is `state=open`), so no session starts until the item is
  open again. The answer says so rather than promising a session, and `--json` carries
  it as `open`.

The item is picked up on the daemon's next pass, within `daemon.poll_interval_secs`: the
overrides are already on the record, so the `attached` post and the launch log both name
the stack that was asked for. Onboarding does not clear the overrides, and the startup
pass, a re-created workspace and a resumed conversation all use them.

**Refused straight away**, with the reason, and nothing assigned or written:

- the item has a session already, `ssf handover` moves that one;
- a handover or a release on the item is pending;
- the item is bound to another item's session, so its own overrides would be inert; the
  refusal names the owner and the command that fits it. A `mode=delegate` item is not
  bound and takes a stack of its own;
- the repository is not watched, the harness is not one ssf knows, it is not installed
  where the daemon runs, its login probe says it is signed out, or the effort level is
  not one it offers;
- the model id is not shaped like one.

A *retired* open item, the bot no longer assigned, so nothing running, is not a session
and stays assignable: the assignment brings it back, in the workspace it kept or one
re-created from its branch. A conversation recorded for the harness the item last ran is
dropped when the new stack runs a different harness, so the resume path never hands an
old harness's conversation id to a new one.

`ssf status` and `ssf peers` say which command put the item on its stack (`harness pi`
after an assignment, `handed over to pi` after a handover; `ssf status` prints
`assigned: harness=pi` and `handed over: harness=pi`).

## Workspaces after close: release and purge

ssf never deletes a workspace that might hold unpushed work. Closing an issue is a
signal anyone can send, an agent included, and the moment the agent looks idle is not
the moment its last commit is safe. So when an item closes (or the bot is dropped from
it) the agent gets one message and the worktree is left exactly as it is. That message
tells it to commit what is worth keeping, push, leave a final comment, and then, only if
everything is on origin, run `ssf release`.

What counts as being dropped from an item follows the trigger it was taken on: an
assignment can be removed and a review request withdrawn, but a mention cannot be taken
back, so an item the bot was only ever mentioned on stays the bot's until it closes.
`ssf release` refuses while the item is still the bot's and names the remedy that fits.

```sh
ssf release                       # inside the session
ssf release N --as owner/repo#N   # from a shell
ssf purge --dry-run
ssf purge --older-than 7
```

- **`ssf release`** asks the daemon to remove the session's workspace after checking, in
  the worktree, that the tree is clean (no modified or untracked files; ignored build
  artefacts do not count), that every commit at HEAD is reachable from a remote-tracking
  ref, and that no stash entry was made on that branch. Reachability is by ancestry
  through a remote-tracking ref: a merge commit puts the workspace's commits in the base
  branch's history, where `--squash` replaces them with one new commit and `--rebase`
  with rewritten ones — neither reachable from anything. A session merging its own pull
  request therefore uses `gh pr merge --merge`, not `--squash` or `--rebase`. After a
  squash or rebase merge the workspace passes only while the branch's own
  remote-tracking ref survives, so a prune of it (`git fetch --prune`, `git remote prune`)
  or a `git push --delete` of the branch leaves the check refusing a workspace whose
  content is already in the base. Where that has happened, take one of three ways out:
  keep the branch on origin, re-push it, or point the workspace at the base once the
  merge is confirmed (`git reset --hard origin/<base>`). If a check fails it prints what
  would be lost and refuses; nothing is removed. A person who has looked can
  pass `--force` (from a shell, not inside the session). The daemon removes the
  workspace and its terminal on its next pass, running the checks once more first unless
  forced. If that re-check finds work, the release is dropped and the agent gets one
  `[ssf] Release of this workspace refused` message naming what would be lost. After
  three such refusals on the same item the daemon stops telling the agent, refuses
  further `ssf release` from it, and marks the item "release given up, workspace kept"
  for a person to deal with.

  A release is also refused while the item is still the bot's, or while the
  session still owns open items, and one already accepted is dropped if the
  item comes back to life before the pass. `ssf release --as owner/repo#N
  --force` releases a retired session's workspace even when it owns open
  follow-ups, and skips the worktree checks, so unpushed work can be lost; the
  follow-ups stay open and keep their ownership, and later activity can
  re-create the workspace from its old branch. An item that is itself still
  the bot's must stop being the bot's first, even with `--force`, and a
  pending handover also blocks release.
- **`ssf purge`** is the sweep for what agents left behind: every workspace whose item
  is closed and whose session has no running agent, listed with its state (`clean and
  pushed`, `dirty`, `unpushed commits`, `unknown` for a detached head or an unreachable
  origin, `agent running`). The clean and pushed ones are removed; the rest are reported
  and kept unless `--force`. Workspaces of open items, of sessions that still own open
  items, and with a running agent are never touched. `--older-than DAYS` limits it to
  items retired longer ago than that; `--dry-run` lists and removes nothing; `--json`
  gives the rows as data. A workspace the driver no longer has (closed by hand) whose
  git checkout is still on disk is judged by that checkout, shown as `(workspace gone,
  checkout still on disk)`, and removed with git when clean and pushed or forced; only
  when the checkout is gone too is the row `already gone` and the record forgotten.
- **`ssf doctor` names the checkouts worth worrying about** before anyone purges or
  deletes by hand. Per repository it walks the clone's worktrees, fetches origin once
  and counts what each holds beyond the base branch and beyond origin. A worktree
  holding commits on no other branch and not on origin, uncommitted changes, or a stash
  entry made on its branch, with no agent in its workspace, gets a `WARN` line naming
  it, what it holds, whether its workspace is open, and whether its item is active.
  Commenting on an active item starts its session again in that checkout; a retired item
  gets nothing, so push its branch by hand (`git -C <checkout> push -u origin
  <branch>`). "On origin" means on any remote-tracking branch as of that fetch, which
  does not prune. Worktrees outside the clone's worktree directory are not ssf's to
  report.
- **What the record says.** A released or purged item is marked `released` (with
  `released_at`), and `ssf status` / `ssf peers --all` show "retired, workspace kept",
  "retired, workspace released" or "retired, release given up, workspace kept" for
  closed items, so a person can see what is lying around.
- **Coming back.** A released or purged workspace is re-created from its branch on
  origin on the item's next event (reopening, re-assignment, a comment on a bound pull
  request), on the stack the item carries: its [per-item
  overrides](harnesses.md#per-item-overrides), or the repository's own settings where
  there are none. The conversation resumes where the harness keeps one.

## Scratch sessions

A scratch session is an agent session on a repository that works on no
item: exploration, a question about the code, a change that has no issue
yet. `ssf scratch create owner/repo --harness ID [--model M] [--effort E]
[--for LOGIN]` starts one and prints its id, `owner/repo~<id>` (four
generated characters; nobody names it). Without `--for` it is shared; with
it, it is that person's, and `ssf status --json` reports the login as
`owner_login` (null when shared) on a row of `kind` `scratch`. A repository
can have any number of them. Its harness starts at an empty composer: ssf
sends no first message, and the person at the terminal gives the first
prompt.

Each has its own worktree on branch `scratch/<id>`, cut from the default
branch. Inside it `SSF_SESSION` names the session and `SSF_ISSUE` is unset;
`ssf sub`, `ssf unsub`, `ssf subs` and `ssf release` act for it, and the
items it follows deliver to it as they would to an item's session, without
its own posts. What it posts on GitHub carries the byline `🤖~<id> says:` and
its own origin tag, so it names no item; there is no item for ssf to report a
signed-out harness on either. While a release is pending it is told nothing. `ssf handover` does not apply: start another
scratch session on the other stack instead.

A scratch session runs in a detached tmux session of its own, not in a herdr
pane: `ssf-<owner>_s<repo>_t<id>` on the default tmux server of the user the
factory runs as (`/` is written `_s`, `~` `_t`, `.` `_d` and `_` `__`, so
`o/site.io~ab12` is `ssf-o_ssite_dio_tab12`; `tmux ls` lists them), with the worktree as its directory, `window-size latest`, so it takes
the size of whichever client attached last, and `detach-on-destroy on`, so a
terminal attached to it ends when it does rather than moving to another
session on the server (whatever a `tmux.conf` sets). `tmux attach -t
=<name>` reaches it from a shell on that machine (in the
guest, for a factory in a VM), and the web endpoint's `api/term` from a
browser ([dashboard.md](dashboard.md#terminal)). The harness is the tmux
session's only command, so the session is live exactly while the harness
runs; tmux reports no agent state, so `ssf status` shows a live one as
`running`, with its last activity read from the harness's transcript where
the harness keeps one. What it follows is pasted into it (`tmux load-buffer`
and `paste-buffer`, then Enter), for every harness: the native Claude Code and
Codex channels reach a harness through herdr and are not used for it. A tmux
server the daemon starts is put in a systemd scope of its own
(`systemd-run --user --scope`) where `systemd-run` is installed and works, so a
restart of the service does not end every scratch session with it. A scratch
session made before tmux was used is left running in its herdr pane and told
there; the next time it is started (a resume, or a restart) it starts in tmux.
tmux is required: `ssf doctor` checks it.

A scratch session lives until it is killed. Closing, merging and `ssf purge`
never touch it, and a daemon restart resumes it (a new tmux session running the
harness's resume command) when its tmux session is gone. A session whose
harness exits while the daemon runs -- Ctrl+C in its terminal, or the harness
quitting -- is **off**: its worktree is there and its tmux session is not.
Nothing starts it again by itself until the next daemon restart or the next
message it is sent; `ssf scratch resume owner/repo~<id>` starts it at once, on
the same worktree, resuming the conversation. `ssf release --as
owner/repo~<id>` kills it (`tmux kill-session`, then the worktree goes) with the same checks as any release (refused, with
the reasons, while work is not on origin; `--force` is for a person at a
shell). The record, its branch and the harness conversation are kept, and
`ssf scratch resume owner/repo~<id>` recreates the worktree, on that branch
when it still exists, and resumes the conversation.

A killed session is forgotten by the factory 24 hours after its release
(`daemon.scratch_release_grace_hours`; `0` keeps released sessions for
ever): the record goes on the next pass, so it leaves `ssf status` and the
extension's list, and `ssf scratch resume` no longer knows it (the refusal
says so). What it followed is unsubscribed with it. Its branch stays in the
checkout (and on origin, where it was pushed), and the harness's own
transcript is left where the harness keeps it: ssf forgets which
conversation was whose, it does not delete one. A session that is live or
merely **off** is never dropped whatever its record says.

However a scratch session is started again -- a resume, a daemon restart, or
a message that finds it gone -- it starts as it did when it was created: the
harness resumes its conversation where it can (fresh where it cannot, or where
the resumed one exits at once), ssf waits for it to settle (answering a folder
trust prompt), and nothing is pasted to announce the start. The person at the
terminal gives the next prompt; a message that found it gone is pasted alone,
as it would have been into a running session.

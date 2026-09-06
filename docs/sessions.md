# Sessions: ownership, reviewers, subscriptions, release and purge

The rules that decide which session acts on an item, how a bot pull request gets reviewed, how sessions follow and message each other, and what happens to a workspace when its item closes. For whoever operates a factory or wants to know why an item went where it did; agents get the same rules from `ssf guide`.

## Ownership: one session per item

GitHub has one bot identity, so without help a human's @mention or review
request on a pull request the bot opened would start a second agent next
to the one that wrote it. ssf instead binds every item to at most one
owning session, decided once when the item is first discovered (first
binding wins):

- **Opened by a session.** An issue or PR whose body carries a session's
  origin tag belongs to that session. The bot's own open items are polled
  (a fourth listing, `creator=<bot>`), so a session hears about the PR it
  opened without anyone assigning or mentioning the bot: it gets one
  message saying the item is now tracked for it, then every later comment,
  review, review request, assignment and the closure, all into the same
  agent, with `SSF_ISSUE` unchanged. The agent's own posts on it are
  filtered out as usual.
- **A PR on a session's branch.** A same-repo pull request whose head branch
  is the branch of a tracked workspace belongs to that workspace's session,
  tag or no tag (this is what a PR opened by hand from an agent's branch, or
  with `gh pr create --fill`, falls back to).
- **Triggers go to the owner.** Assigning or mentioning the bot on an owned
  item is delivered to the owner's agent as activity, never to a new
  session. When an issue a session filed itself is assigned to the bot, that
  activity ends with one line saying the issue is the session's to work on
  and that nobody else is spawned for it (an assignment otherwise reads as
  bookkeeping, and the session waits for a second session that never
  comes). If the owner has been retired or its workspace removed, it is
  brought back the way any lost session is (workspace re-created from its
  branch, conversation resumed), rather than replaced. A retired owner's
  workspace cannot be released or purged while items bound to it are still
  open. The one exception is a review asked on an owned pull request (a
  review request, or the `review` label), which gets a reviewer session
  (below): the author is told that, and not to review its own work.
- **Hand-offs.** An item a session creates *and assigns the bot to* in the
  same `gh ... create` command is a delegation: the tag carries
  `mode=delegate`, the item gets a fresh session of its own, and the creating
  session is subscribed to it (see below): it sees the child's activity as
  FYI messages, and when the child is closed or merged it gets a single
  message with the outcome and the child's final comment (the last comment
  the bot left on it). The child is told it was handed off and to leave a
  clear final comment. `ssf guide` explains this rule to agents, so an
  agent that wants a separate worker uses `--assignee`, and one that wants
  to keep an item simply opens it.
- **Nothing to bind to.** A bot-opened item with no usable tag, no branch
  match and no human trigger is left alone (logged once) rather than given
  a session nobody asked for; assigning or mentioning the bot on it later
  starts one as usual. It is looked at again when it changes on GitHub *or*
  when it shows up on another listing (assigned, mentioned, review
  requested), whichever comes first: an assignment made just before the
  first pass saw the item can be older than the `updated_at` it was ignored
  with, so listing membership is part of what "unchanged" means. What it
  was ignored with is kept in `state.json` (under `ignored`, per
  repository), so a daemon restart does not fetch every such item again the
  next time a listing changes; the record goes when the item leaves every
  listing. (A state file from before this record existed still costs one
  walk on the first pass that sees a change.) Items opened from a session
  on a *different* repository are not bound across repositories.

`ssf status --json` shows the binding as `owner` / `shares_workspace_of`
and hand-offs as `delegated_by`; `ssf peers` prints them as "owned by ..."
and "handed off by ...".

## Reviewer sessions

A session must not review its own pull request, so a review asked of the
bot on a PR that one of its sessions owns (opened from it, or on its branch)
does not go to that session. ssf starts a **reviewer session** instead. A
review is asked for in one of two ways:

- **The `review` label** (`daemon.review_label`; set it to `""` to turn the
  trigger off). This is the way for a PR the bot opened: GitHub refuses a
  review request from a pull request's own author (`gh pr edit --add-reviewer
  <bot>` silently adds nothing on such a PR), so a human, or the author's
  agent, adds the label instead (`gh pr edit N --add-label review`). The
  label is the request: once the reviewer has posted a review newer than the
  label, ssf removes the label and stands the reviewer down, and adding it
  again asks for another look. The label must exist in the repository, and
  the bot needs write (triage) access to take it off; if removing it fails,
  the failure is logged against the reviewer session and retried on the
  next poll.
- **A review request** from the bot, on a PR the bot did not open but which a
  session owns (a PR opened by hand from an agent's branch). GitHub drops the
  request once the review is posted, or when it is withdrawn.

The reviewer session is:

- a second workspace, `review-<n>-<title>`, checked out at the PR's head
  (`origin/<branch>`) on a local branch of its own, so nothing the reviewer
  does can move the PR; the reviewer is told it is a read-only checkout and
  how to refresh it after the author pushes;
- its own agent, launched with `SSF_ROLE=reviewer` (so the gh wrapper tags its
  posts `role=reviewer`), and a review-specific prompt: the PR, its
  description and history, then how to review (`git fetch origin && git
  diff origin/<base>...origin/<head>`, or `gh pr diff`; `gh pr review <n>
  --comment`, since GitHub refuses approve and request-changes from the
  account that opened the PR, so the verdict goes in the body), never
  commit, push, merge or touch the board, and that the author is another
  session of the same bot;
- the session id `owner/repo#N:reviewer`. It is listed by `ssf peers` as
  kind `rev` ("reviewer session for owner/repo#N"), can be reached with
  `ssf tell N:reviewer "..."` (or `owner/repo#N:reviewer`), and can `ssf sub`
  other items as itself; it owns nothing and cannot be subscribed to (follow
  the PR instead).

The author session keeps the PR: the label (or review request) is delivered
to it as activity with a note that a reviewer session has it, and the review
itself arrives as activity marked "from the reviewer session on
owner/repo#N". The author answers on the PR and pushes fixes as it would for
a human reviewer; its replies reach the reviewer marked "from the agent on
owner/repo#A". To get another look it adds the `review` label again (`ssf guide`
says so).

The reviewer lives as long as the request: while the label is on the PR (or
the bot is a requested reviewer), new activity on the PR (pushes, replies) is
delivered to it as `[ssf] New activity on #N`. Posting the review fulfils
the request (ssf removes the label, or GitHub drops the review request), and
the reviewer is stood down (told to stop, its record kept). Only a review
counts, not a comment: a review by the bot with the reviewer's origin tag,
or without any tag; one tagged with another session's origin is that
session's doing. A repeated request brings the same session back, with what
happened in between, resuming its conversation (and re-creating its
workspace at the PR's current head if that was removed). When the PR is
closed or merged the reviewer is told, its workspace is marked completed
and, being a read-only checkout that never holds work of its own, removed
on its own once the agent is idle (or after `daemon.cleanup_grace_secs`).
Reviewer state lives next to the items in `state.json` under `reviewers`,
keyed by PR number; its `triggers` say what asked for the review
(`review_label`, `review_requested`).

Only same-repository PRs owned by a session get a reviewer. A PR the bot did
not write (a human's PR the bot is asked to review, or one assigned to it
without a session of its own on the branch) is handled as before: a session
of its own, on the PR's branch, which reviews when asked.

## Subscriptions and cross-session comments

Exactly one session acts on an item; any number can hear about it. Each
item carries a list of subscriber sessions next to its owner, and every
delivery about the item (new activity, closure, the bot being dropped from
it, or the item getting a session of its own) is fanned out to them with
FYI framing: `[ssf] FYI: new activity on issue #N "title" (url):`, `[ssf]
FYI: issue #N ... has been closed`, and so on, ending with one line saying
it is for information only and how to stop them. Subscriptions live in the
state file, so they survive relaunches and a session being brought back; a
session that retires (its item closed, or the bot dropped from it) is
unsubscribed everywhere.

The CLI takes the session identity from `SSF_REPO`/`SSF_ISSUE` (plus
`SSF_ROLE` for a reviewer) inside a session, or `--as owner/repo#N` (or
`owner/repo#N:reviewer`) from a human shell (an item bound to another
session counts as that session):

- `ssf sub <n|owner/repo#n>` / `ssf unsub ...` follow or drop an item.
  Subscribing to an item nothing tracks yet makes it tracked as
  *subscriber-only*: polled every pass for activity, no workspace, no owner;
  what happened before the subscription is not replayed. If the bot is later
  assigned to it (or it is otherwise bound), it gets a session as usual and
  the subscribers are told; when nobody follows it any more it is dropped.
- `ssf subs` lists what this session follows and who follows its items
  (`--json` for detail); `ssf peers` shows subscribers per session.
- `ssf tell <n> "message"` pastes a message into the terminal of the session
  acting on that item (`<n>:reviewer` for a PR's reviewer session), through
  the daemon's own delivery path (so the agent is relaunched or resumed first
  if its terminal is gone). It arrives as an
  `[ssf] Message from the agent session on owner/repo#A ("title") ...` prompt,
  or "from a human at the terminal" without `--as`. For an operator it is the
  steering tool ("stop, I'm changing the spec"). Between agents it is the
  exception: the default channel is a comment on the item (below), and
  `ssf guide` tells agents to keep `tell` for operational nudges that would
  be noise on the item ("master moved, rebase", "terminal is being replaced")
  and for reaching a session whose item is already closed. Tells are not
  mirrored to GitHub, so anything someone might need to find later
  (decisions, questions that change scope, status) goes on the item.
- Delegating parents are subscribed to their children automatically.

`sub`, `unsub`, `tell`, `release` and `purge` talk to the running daemon
over a Unix socket in the state directory (`ssf.sock`), because the daemon
owns the state and the delivery path; `ssf doctor` reports whether it
answers. `subs`, `peers` and `status` read the state file and work without
it.

**Cross-session comments.** Comments the bot posts carry the origin tag of
the session that made them (see [Identity and bylines](identity-and-bylines.md)),
and delivery is sorted out per recipient rather than by author: a bot
comment whose tag names a different session is delivered like a human's,
labelled "(from the agent on owner/repo#M)", while a comment tagged with
the recipient's own session (or an item that session acts on) is the
self-echo and stays filtered. An untagged bot comment is a person's and
reaches every recipient, marked "(not from a session)". So session A talks
to session B by commenting on B's issue with `gh`: B's agent receives it
labelled as coming from A, and A does not receive its own comment back,
even when A is subscribed to B's issue. This is the default channel between
agents: `ssf guide` says so, and a `tell` message repeats in one line that
the answer goes on the item.

## A harness that is not signed in

A harness login can go away under a running session: the token expires,
or it is revoked (a logout elsewhere on a copied credential does that,
see [Inside a microVM](vm.md#logins)). Claude Code then answers every
prompt with `Login expired · Please run /login` and waits; the other
harnesses show their sign-in screens. Nothing inside the session can fix
it, and to the driver the agent looks alive and idle, so without help ssf
would keep pasting activity into a terminal that cannot act.

So every pass, for each session whose agent is idle, ssf reads the bottom
of its screen and, if it shows the harness's sign-in prompt (the phrases
are the harnesses' own, as seen on their screens; `driver::login_dialog`
has the list per harness), marks the session **blocked**. Two things keep
an agent's own screen from tripping this: only the bottom of an idle
agent's screen counts, and a line inside echoed `[ssf]` text (a pasted
prompt, or activity delivered from the item, where a person may well have
quoted the phrase) is skipped. Every text ssf itself puts on a screen or
that agents read (the comments below, the message after a restart, the
`BLOCKED:` lines, the refusal `ssf tell` prints) is worded without those
phrases, and a test pins that. What remains is an agent quoting the exact
phrase in its own answer, or a quoted comment line the terminal wrapped
past the quote marker; that costs one blocked comment and one restart
after the retry wait, with the "signed in again" comment if it took long,
and nothing more: the restarted screen is clean.

- **Told once.** One comment lands on the session's item, as the session
  (`🤖#N says:` ... `[ssf] This session's Claude Code is at its sign-in
  prompt ... Sign in with `claude auth login` on the host`; inside the VM
  the fix is `ssf vm login claude`), the log gets a warning (with the
  screen line), and the session shows as blocked in `ssf status` (a
  `BLOCKED:` line naming the harness, since when and the command to run;
  `--json` carries it as `blocked` on the session, with `harness_name`,
  `detail`, `since` and `fix`, and `blocked_sessions` at the top), in
  `ssf peers`, and in the bar widget (urgent, with the same line on the
  session's row).
- **Nothing is delivered.** Activity on the item, tells, FYIs and the
  closing message are held: the item's bookkeeping is left as it was
  (`updated_at`, the seen events), so what happened meanwhile is
  delivered in full once the session is back. `ssf tell` to a blocked
  session is refused with the reason. A harness that ssf starts again
  (after a reboot, say) and that comes up on its login screen is caught
  the same way.
- **Checked every pass.** ssf asks the harness where it runs (the host,
  or the guest with the factory in a VM), once per harness per pass:
  `claude auth status --json` for Claude Code, `codex login status` for
  Codex, the credential file from the [login table](vm.md#harness-logins)
  (`~/.gemini/oauth_creds.json`, `~/.pi/agent/auth.json`, ...) or an API
  key in the environment for the others. When the login is back (a new
  credential file, or a signed-in answer), ssf quits the stuck harness,
  starts it again with its conversation resumed, gives it one message
  saying what happened, and fetches every listing in full so the held
  activity follows. If the restart comes back to the prompt (a revoked
  token the status command still reports as signed in, or a harness ssf
  cannot check) the item is not told again and the next attempt waits
  twice as long: ten minutes, twenty, forty, then every hour. A harness
  whose terminal is gone (a reboot, a closed terminal) is judged by its
  restart the same way: the block is lifted only once it has taken a
  prompt. A person who runs `/login` in the terminal instead lifts the
  block on the next pass with no restart. If the block lasted more than
  five minutes the item gets one more comment saying the session resumed.

`ssf doctor` prints one line per harness the configured repositories use
(`Claude Code signed in on the host (claude auth status: signed in)`, or
`FAIL ... not signed in ...` with the command to run; `note ... cannot
tell` for a harness ssf has no check for, such as Copilot's keyring), and
with the factory in a VM it is forwarded into the guest, so the check
happens where the agents are.

## Workspaces after close: release and purge

ssf never deletes a workspace that might hold unpushed work. Closing an
issue is a signal anyone can send, an agent included, and the moment the
agent looks idle is not the moment its last commit is safe. So when an item
closes (or the bot is unassigned) the agent gets one message and the
worktree is left exactly as it is, whatever state it is in. The message
tells the agent to commit what is worth keeping, push, leave a final
comment, and then, only if everything is on origin, run `ssf release`.

- **`ssf release`** (inside the session, or `ssf release 12` /
  `--as owner/repo#12` from a shell) asks the daemon to remove the
  session's workspace after checking, in the worktree, that the tree is
  clean (no modified or untracked files; ignored build artefacts do not
  count), that the checked-out branch is on origin with no unpushed
  commits, and that no stash entry was made on that branch. If any check
  fails it prints what would be lost and refuses; nothing is removed.
  A person who has looked can pass `--force` (from a shell, not inside the
  session). The daemon removes the workspace, and its terminal, on its next
  pass, running the checks once more first. If that re-check finds work
  (the tree changed after the agent asked, or a push did not land) the
  release is dropped and the agent gets one `[ssf] Release ... refused`
  message naming what would be lost, so it can fix that and ask again.
  After three such refusals on the same item the daemon stops telling the
  agent, refuses further `ssf release` from it, and marks the item
  "release given up, workspace kept" (in `ssf status`, `ssf peers --all`
  and `ssf purge --dry-run`) for a person to deal with; only the daemon's
  own refusals count, not the ones `ssf release` prints straight away.
  A release is also refused while the item is still open and assigned, or
  while the session still owns open items (a pull request bound to it,
  say), and one already accepted is dropped if the item comes back to life
  before the pass. Reviewer sessions look after themselves and are not
  released by hand.
- **`ssf purge [--dry-run] [--older-than DAYS] [--force]`** is the sweep
  for what agents left behind: every workspace whose item is closed and
  whose session has no running agent, listed with its state (`clean and
  pushed`, `dirty`, `unpushed commits`, `unknown` for a detached head or an
  unreachable origin, `agent running`). The clean and pushed ones are
  removed; the rest are reported and kept unless `--force`. Workspaces of
  open items, of sessions that still own open items, and with a running
  agent are never touched. `--older-than` limits it to items retired more
  than that many days ago; `--json` gives the same rows as data.
- **What the record says.** A released or purged item is marked
  `released` (with `released_at`), and `ssf status`/`ssf peers --all` show
  "retired, workspace kept", "retired, workspace released" or "retired,
  release given up, workspace kept" for closed items, so a person can see
  what is lying around. The old `daemon.cleanup_on_close` key is accepted
  but does nothing.
- **Coming back is unchanged.** A released or purged workspace is re-created
  from its branch on origin on the item's next event (reopening,
  re-assignment, a comment on a bound pull request), and the conversation
  resumes.

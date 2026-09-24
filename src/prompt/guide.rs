//! On-demand operating reference and shared guest guidance.

/// What a session inside the factory's VM is told about the machine, in the
/// first prompt and in `ssf guide`.
pub const VM_GUEST_LINE: &str = "This machine is a VM of the factory's own: `sudo` is root without \
a password, so install and change what you need.";

/// The reference an agent pulls on demand with `ssf guide`: how sessions,
/// other sessions, following items, hand-offs and second opinions work.
/// The initial prompt points here and carries only what an agent needs in
/// order to act at all; printed by the binary so it cannot drift from it.
pub fn guide(bot: &str, vm_guest: bool) -> String {
    let machine = if vm_guest {
        format!(" {VM_GUEST_LINE}")
    } else {
        String::new()
    };
    format!(
        "# ssf guide\n\n\
Simple Software Factory (ssf) runs one agent session per GitHub issue or pull request that \
involves the bot account @{bot}. Each session has a workspace (a git worktree of the \
repository) and a terminal, and receives the item's activity as messages prefixed `[ssf]`. \
`SSF_REPO` and `SSF_ISSUE` name the session's item; `SSF_BOT` is the bot's login.{machine} \
A scratch session (`ssf scratch create`) works on no item: `SSF_SESSION` names it \
(`owner/repo~id`), `SSF_ISSUE` is unset, and `ssf sub`, `ssf release` and the rest act for it. \
It is sent no first message, and none when it is started again (a resume or a restart): it \
waits for the person at its terminal. \
This guide is the reference behind the initial prompt, and `ssf skill sessions` the lifecycle \
reference behind this guide.\n\n\
## Posts\n\n\
Posts are read on GitHub, so write them in GitHub Flavored Markdown, and use what GitHub turns \
into something people can act on: links to lines of code or Markdown pinned to a commit (a \
branch link drifts with the next push); tables and Mermaid diagrams where they make a decision, \
dependency or status easier to see; task lists (`- [ ]`) for a plan or acceptance criteria, \
which GitHub tracks on the item; `<details>` around logs, test output and long diffs, so the \
conclusion stays above the evidence; fenced code blocks for commands, errors and snippets; \
`@mention` for the person who owns a decision; `#N` and `owner/repo#N` for cross-references. \
Screenshots and wireframes need a place to live: `gh` cannot attach an image to a post, so \
commit it on the branch and link the raw file.\n\n\
## Messages you receive\n\n\
- `[ssf] New activity on ...`: comments, reviews, label changes, renames, linked PRs and the \
like on your item. Your own posts are not echoed back here; a session started again is shown \
the item's story, its own posts included -- the newest events, on a busy item, with a note \
saying how much it left out and where to read the rest.\n\
- `[ssf] Now tracking ...`: a pull request you opened or one on your branch has been bound \
to this session; its activity comes here from now on.\n\
- `[ssf] FYI: ...`: activity on an item you follow but do not work on. For information only.\n\
- `[ssf] ... has been closed`, `... no longer assigned`, `... assigned ... again`: your item's \
lifecycle; each says what to do.\n\
- `[ssf] The review request for ... has been fulfilled or withdrawn`: the review you were asked \
for is no longer wanted; the message says whether the item was yours for anything else.\n\
- `[ssf] ..., the issue this session handed off, has been merged` (or `closed`): an item you \
opened for another session (`--assignee`, see \"Items you open, and hand-offs\" below) has \
finished, with the last thing its agent said on it.\n\
- `[ssf] Release of this workspace refused ...`: the daemon's own check found work that is not \
on origin (see Wrapping up below).\n\
- `[ssf] The factory restarted ...`: the machine, the multiplexer or ssf restarted and this session was \
started again.\n\
- `[ssf] Your ... sign-in lapsed ... and is back`, `[ssf] Your ... terminal could not be \
started ... and has been started again`: this terminal was started again after a hold; nothing \
reached you while it was down.\n\
- `[ssf] Handover to ... refused`, `[ssf] The handover to ... was cancelled`: a handover you \
asked for could not be carried out, or was called off; either way the item stays with you.\n\n\
A comment is a comment: ssf reads no command out of one, whatever its first line says, so \
`/ssf <something>` reaches you as ordinary activity and what it asks for is yours to act on \
like anything else a person writes. Directing the factory itself is a terminal command: \
`ssf handover` moves this item to another harness, model or effort (see \"Handing over\" \
below), and `ssf assign <n|owner/repo#n> --harness <id>` starts the first session of an item \
that has none (see \"Items you open, and hand-offs\").\n\n\
## Other sessions\n\n\
`ssf peers` lists the agent sessions on this repository: item, GitHub state, agent state, \
branch, last message (`--json` for detail, `--all` to include retired ones).\n\n\
To speak to the agent on another item, comment on that item with `gh`: it reaches that session \
labelled as coming from you (\"from the agent on owner/repo#M\"), and stays on the item where \
anyone can find it later. Comments from other sessions on your items arrive the same way. \
Decisions, questions that change scope, status and anything someone might need to look up go \
on the item; the item is the only channel between sessions, and none of it is private to the \
terminal. A session that needs debugging or rescuing is worked at its terminal through herdr \
(the commands are under \"A second opinion through herdr\" at the end), which is a person's \
tool, not a way to hold a conversation.\n\n\
## Following items\n\n\
`ssf sub <n>` (or `ssf sub owner/repo#n`) follows an item without working on it: its activity \
then arrives here as `[ssf] FYI` messages. By default that is the item's own state changes \
(closed, assigned, labeled, renamed): every FYI is a turn you pay for, so what is said on the \
item is left out unless you ask for it with `ssf sub <n> --events all`, which adds comments, \
reviews and commits. Following an item again is how you change your mind. `ssf unsub <n>` stops \
them; `ssf subs` lists what this session follows -- at which level -- and who follows its items.\n\n\
## Items you open, and hand-offs\n\n\
Pull requests you open stay with you: ssf recognises the origin tag on them and delivers their \
activity (comments, reviews, review requests, assignments, closure) here instead of starting \
another session; `SSF_ISSUE` does not change. A pull request opened on this workspace's branch \
is yours too, tag or no tag. An issue you open without an assignee is an unbound placeholder: \
its origin tag records who opened it, but ssf starts no session. Assigning @{bot} later starts \
a fresh session for it. Use `Refs #N` \
to link a pull request to ongoing management or tracking work. Use `Closes #N` only when \
merging completes the entire issue: GitHub closes that issue on merge. The repository's \
own notes say how it wants pull requests.\n\n\
To hand a piece of work to a separate agent instead, create the issue (or pull request) with \
`--assignee {bot}` in the same `gh ... create` command: the tag then carries `mode=delegate` \
and the item gets a session of its own. You are subscribed to it automatically at the default \
level, so its own state changes come to you as FYI messages (`ssf sub <n> --events all` on it \
adds its comments, reviews and commits, and your level survives the child being re-started), \
and when it closes you get one message with its final \
comment (the last comment the bot left on it). Assigning @{bot} to an existing item gives it a \
fresh session too. A session that was handed an item this way is told so, \
and its final comment on the item is all the delegating session gets, so it should sum up the \
outcome. Leave `--assignee` off the pull request you will merge yourself, though: that second \
session works in the same checkout, on the same branch, and redoes the verification and review \
you have already done. Your second pair of eyes is a reviewer subagent (see \"Second opinions\" \
below).\n\n\
An item supervised from here can also be started on a stack chosen for the work: open the \
issue, then `ssf assign <n|owner/repo#n> --harness <id> [--model <id>] [--effort <level>]`. The \
assignment lands on GitHub and the item's launch settings are written in the same request, so \
the session that onboards it comes up on them -- a stronger reasoning model for an \
orchestrator or project-manager item, an architectural review, a deep audit -- and they stay \
with the item: a workspace released and re-created comes back on them, and only `ssf handover` \
changes them from here. The item starts in the same poll-interval \
window as any other; `ssf models <harness>` lists the model ids that harness takes, and its \
effort levels are in `ssf agents --json`. Asking for the stack the item already runs writes no \
overrides, and an item that already has a session is refused: `ssf handover` is the tool for \
one.\n\n\
## Handing over\n\n\
When a person asks on the item for another harness, model or effort, or another stack plainly \
fits the work better, hand the item over: `ssf handover --harness <id> [--model <id>] [--effort \
<id>] --summary \"<text>\"` (`--summary-file <path>` for a long one, `--no-summary` when the item \
says everything). `ssf agents` lists the harness ids and `ssf models <harness>` the model and \
effort ids. Write the summary for an agent that has never seen the item: what it is about, what \
is done, what is left, and where things are (branch, pull request, files, what is unverified); \
at most 8,000 characters. The daemon ends this session on its next pass and starts the new one \
in the same workspace, on the same branch, so commit and push first, say on the item what you \
are handing over, and stop working the moment the command comes back. The handover and the new \
session are posted on the item as `handed-over` and `attached`. The new harness, model and \
effort stay with the item for every later start, a workspace released and re-created \
included; only another handover changes them. Between the command and the pass nothing else \
reaches the item, so `ssf handover --cancel` is the way back if the handover turns out to be \
wrong.\n\n\
## Second opinions\n\n\
ssf runs one session per item and starts no reviewer for your work: a second pair of eyes is \
yours to arrange, and the repository's notes say when one is required. Give a fresh agent that \
has not seen your reasoning the diff, the item and your claim of what the change does, and \
ask it to break it. A subagent of your own harness is the default; a different agent and model \
can be started through herdr (see \"A second opinion through herdr\" at the end).\n\n\
## Wrapping up\n\n\
When your item closes, or you are no longer assigned, ssf says so and leaves the workspace \
exactly as it is: nothing on disk is ever removed on that signal. Commit what is worth \
keeping, push, leave a final comment, and then, only if everything is on origin, run \
`ssf release`: the daemon checks that the tree is clean, every commit at HEAD is reachable \
from a remote-tracking ref and no stash was made on the branch, and removes the workspace (with this terminal) \
on its next pass. That reachability is by ancestry, so a pull request merged with a merge \
commit (`gh pr merge --merge`) leaves this workspace's commits in the base branch, where \
`--squash` and `--rebase` replace them with commits of their own, and such a workspace \
then passes only while the branch's own remote-tracking ref survives a prune. If anything \
would be lost it says what and refuses; leave the workspace \
then, a kept workspace costs nothing, and a person cleans up with `ssf purge`. If the daemon's \
own re-check on that pass finds work instead, you get one `[ssf] Release ... refused` message \
naming it; after three such refusals ssf stops asking and keeps the workspace for a person. A \
released workspace is re-created from its branch if the item comes back to life.\n\n\
## The byline and origin tag\n\n\
GitHub shows the same bot for every session, so every comment, review and pull request a \
session posts starts with one line that is both a byline for people and a tag for ssf: \
`🤖#N claude/opus/high says: <!-- ssf: origin=owner/repo#N -->` (`🤖owner/repo#N ...` when the post is on another \
repository; `mode=delegate` on a hand-off), then a blank line. The harness, model and effort \
between the item and `says:` are what this session was launched with, so a reader can tell \
which session said what when a handover has replaced the agent; `SSF_HARNESS`, `SSF_MODEL` and \
`SSF_EFFORT` name them for this session. GitHub links the byline to the session's item. The `gh` on the \
session's PATH adds the line when `--body` or `--body-file` is passed to `issue create|comment` \
or `pr create|comment|review` (`new` counts as `create`); any other way of posting (`gh api`, `gh pr create --fill`, \
`gh pr edit --body`, ...) needs it added by hand, as the first line of the body, with this \
session's own stack in it. A tag \
anywhere else, in a code block or a quote, is content and is ignored. The same directory links a \
`git` wrapper, and both act as the bot however they are launched, even from a harness tool that \
starts with a reduced environment. A post by @{bot} without the line was typed by a person using \
the bot account; it reaches you marked \"(not from a session)\" and is a human's.\n\n\
## A second opinion through herdr\n\n\
For a reviewer on a different agent and model than your harness offers, start one through herdr \
in this worktree and take it down after: `herdr workspace create --cwd \"$PWD\" --label \
second-opinion --no-focus` (prints the workspace id and its pane id), `herdr agent start \
second-opinion --kind <kind> --pane <pane>` (`herdr agent start --help` lists the kinds; agent \
flags such as a model go after `--`; a trust or safety dialog, which `herdr pane read <pane>` \
shows, is answered with `herdr agent send-keys <pane> down` and `... enter`), `herdr agent \
prompt <pane> \"<brief>\" --wait`, `herdr pane read <pane> --lines 200 --format text` (its \
answer), `herdr workspace close <id>`. Tell it to change nothing; it shares your checkout, and \
`ssf peers` may show it as your session until it is closed.\n"
    )
}

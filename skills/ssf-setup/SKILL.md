---
name: ssf-setup
description: How to use ssf / Simple Software Factory, including setup, configuration options, and operating agent sessions.
---

# Simple Software Factory (ssf)

SSF is built on top of herdr. Herdr must be installed and running on a host
factory; VM setup supplies it in the guest.

SSF release packages support Arch-family Linux (including Omarchy) and
Debian-family Linux (including Ubuntu). macOS support is planned next but is
not supported yet. Run a Linux factory directly on a dedicated Linux host,
VPS, or container, or inside a microVM for the recommended local setup.

For installation, follow the [repository instructions](https://github.com/mikekelly/simple-software-factory#install).

On a host where an always-on assistant should be the user's liaison for
SSF-tracked repositories, follow the repository's
[liaison guide](https://github.com/mikekelly/simple-software-factory/blob/master/docs/bot-dedicated-vps.md)
after installing the factory. Configure the assistant's GitHub access and
event delivery separately from the factory bot's `ssf auth`.

Once installed, run `ssf skill` for authoritative guidance bundled with your
binary, then follow its `ssf skill <topic>` commands for details.

After `ssf repo add`, inspect pre-existing bot allocations with `ssf
candidates [--repo OWNER/NAME]`; the first successful poll leaves them idle.
After verifying that no other factory owns an item, opt in explicitly with
`ssf adopt OWNER/NAME#N [...]`. Adoption starts a fresh harness conversation
from the complete GitHub history. New allocations after enrollment and normal
daemon restarts remain automatic.

An issue opened by an agent without assigning the bot is an unbound
placeholder. Create it with `--assignee <bot>` to hand it to a fresh session;
assigning the bot later also starts a fresh session.

Optional factory-wide agent context goes in `~/.ssf/SSF.md`, with
`~/.ssf/SSF.<harness>.md` for harness-specific additions. These files are read
from the guest user's home in VM mode or the host user's home in host mode,
before repository-specific instructions, and `ssf doctor` does not require
them.

To let a server accept pending repository invitations from trusted GitHub
users automatically, run `ssf config set github.auto_accept_invitations_from
'["OWNER"]'`. The login match is case-insensitive. Acceptance grants the bot
account access but deliberately does not add a `[[repo]]`, select a named
server, or enable its service; use `ssf repo add` separately on the intended
server.

For OMP on headless/herdr hosts, read `ssf skill headless` for one-time
interactive setup and pane credential guidance before spawning sessions.
SSF's default OMP command uses a 15-minute provider-stream idle timeout because
long unattended turns can exceed OMP's normal five-minute window. A custom
repository `command` replaces that default; include
`PI_STREAM_IDLE_TIMEOUT_MS=900000` in a custom OMP command to keep the same
behavior. Setting it to `0` disables stall detection and can leave a genuinely
wedged stream waiting forever.

SSF's default OMP and Pi commands also use the shipped
`"$SSF_PI_LAUNCHER" pi|omp ... -e "$SSF_PI_BRIDGE"`. The launcher isolates and
resumes the harness transcript; the extension wakes idle sessions for item
activity without submitting a person's composer draft. A custom OMP/Pi
repository `command` must use both too. `ssf launch` supplies their paths; `ssf
doctor` reports a live session whose channel is unavailable, and the session
must be restarted after an upgrade or command correction.

Claude's default command adds `--settings '{"crossSessionInbound":"accept"}'`
for native item activity through its peer inbox. Custom commands must retain
that inline setting and bypass permissions; restart existing sessions to load
it. `ssf doctor` reports an unavailable inbox (legacy terminal fallback).
Ambiguous native sends are held, never blindly resent; inspect their journal
and target transcript as described in `ssf skill drivers` before intervening.

Codex native delivery is experimental and opt-in: a launcher/Herdr must provide
an item-specific private Unix app-server and start the normal TUI with explicit
`--remote unix://PATH` plus SSF's bypass-approvals/sandbox and bypass-hook-trust
flags. Remote resume omits the permission-bypass flag and retains server permissions;
custom launchers must handle this when SSF appends `resume <id>`.
Do not point multiple items at a shared conversation or start a separate
headless session as a substitute. SSF pins the endpoint and exact conversation,
journals events and reconciles rollout receipts. Explicit channel failures and
ambiguous sends are held; standalone sessions retain terminal fallback. Run
`ssf doctor` and read `ssf skill drivers` before changing a saved binding; never
delete an uncertain journal to force delivery. Do not enable this launch mode
without the operator accepting its experimental status and server ownership.

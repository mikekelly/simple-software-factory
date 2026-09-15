---
name: ssf-setup
description: How to use ssf / Simple Software Factory, including setup, configuration options, and operating agent sessions.
---

# Simple Software Factory (ssf)

SSF is built on top of herdr. Herdr must be installed and running on a host
factory; VM setup supplies it in the guest.

SSF release packages support Arch-family Linux (including Omarchy) and
Debian-family Linux (including Ubuntu). macOS support is planned next but is
not supported yet. Run a Linux factory either directly on a VPS (for example a
Grok Bot or Meta Muse machine, or a Hetzner server) or, for the recommended
local setup, inside a microVM.

For installation, follow the [repository instructions](https://github.com/mikekelly/simple-software-factory#install).

On a Bot-dedicated-VPS where Grok Bot should be the user's liaison for
SSF-tracked repositories, follow the repository's
[liaison guide](https://github.com/mikekelly/simple-software-factory/blob/master/docs/bot-dedicated-vps.md)
after installing the factory. Grok Bot's Cursor GitHub event connection is a
separate enrollment from the factory bot's `ssf auth`.

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

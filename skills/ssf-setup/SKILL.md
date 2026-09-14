---
name: ssf-setup
description: How to use ssf / Simple Software Factory, including setup, configuration options, and operating agent sessions.
---

# Simple Software Factory (ssf)

SSF is built on top of herdr. Herdr must be installed and running on a host
factory; VM setup supplies it in the guest. Orca is not a supported driver.
Before upgrading an older Orca-based installation, push any work held only in
Orca worktrees and remove its driver settings from `config.toml`.

For installation, follow the [repository instructions](https://github.com/mikekelly/simple-software-factory#install).

On a Bot-dedicated-VPS where Grok Bot should be the user's liaison for
SSF-tracked repositories, follow the repository's
[liaison guide](https://github.com/mikekelly/simple-software-factory/blob/master/docs/bot-dedicated-vps.md)
after installing the factory. Grok Bot's Cursor GitHub event connection is a
separate enrollment from the factory bot's `ssf auth`.

Once installed, run `ssf skill` for authoritative guidance bundled with your
binary, then follow its `ssf skill <topic>` commands for details.

An issue opened by an agent without assigning the bot is an unbound
placeholder. Create it with `--assignee <bot>` to hand it to a fresh session;
assigning the bot later also starts a fresh session.

Optional factory-wide agent context goes in `~/.ssf/SSF.md`, with
`~/.ssf/SSF.<harness>.md` for harness-specific additions. These files are read
from the guest user's home in VM mode or the host user's home in host mode,
before repository-specific instructions, and `ssf doctor` does not require
them.

For OMP on headless/herdr hosts, read `ssf skill headless` for one-time
interactive setup and pane credential guidance before spawning sessions.

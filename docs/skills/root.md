# Simple Software Factory (ssf)

SSF turns GitHub issues assigned to a bot into coding-agent sessions in managed
workspaces. A client (`ssf`) controls a factory daemon (`ssf-server`), locally,
in a VM, or over SSH.

## Key commands

- `ssf setup`: prepare a packaged installation for this user.
- `ssf server list`: inspect the client-owned server catalog.
- `ssf auth login`: enroll the separate bot account.
- `ssf agents` / `ssf models <harness>`: inspect harness and model choices.
- `ssf repo add` / `ssf repo set`: configure watched repositories and agents.
- `ssf config` / `ssf config set`: inspect or change factory settings.
- `ssf doctor` / `ssf status`: diagnose and inspect a factory.
- `ssf dashboard`: watch factories in a terminal.
- `ssf vm`: manage the factory VM from its host.
- `ssf guide`: read the session-specific agent collaboration reference.
- `ssf peers`, `ssf handover`, `ssf assign`, `ssf sub`: coordinate sessions.
- `ssf release` / `ssf purge`: retire workspaces after checking their work.

Use `ssf <command> --help` for arguments. Inspect existing targets and their
health before changing setup. Bot credentials belong to the bot, never the
person's account. Preserve uncommitted and unpushed work.

## Read only the topic needed

| Command | Bundled document / purpose |
| --- | --- |
| `ssf skill setup` | `docs/setup.md`: first factory, authentication, harness/model selection |
| `ssf skill agent` | `docs/agent-guidance.md`: agent operating rules and safety boundaries |
| `ssf skill liaison` | `docs/liaison.md`: the assistant that acts for a person, on the factory host or from their machine |
| `ssf skill client-cli` | `docs/skills/client-cli.md`: everyday CLI and target selection |
| `ssf skill server` | `docs/skills/server.md`: daemon, services, diagnosis |
| `ssf skill config` | `docs/configuration.md`: options, repositories, allowed users |
| `ssf skill vm` | `docs/vm.md`: VM lifecycle and host/guest ownership |
| `ssf skill headless` | `docs/headless-host.md`: VPS / headless Linux host path |
| `ssf skill install-binaries` | `docs/install-binaries.md`: standalone / client-only installation |
| `ssf skill drivers` | `docs/drivers.md`: harnesses and workspace drivers |
| `ssf skill sessions` | `docs/sessions.md`: collaboration and workspace lifecycle |
| `ssf skill dashboard` | `docs/dashboard.md`: terminal and web interfaces |
| `ssf skill uninstall` | `docs/uninstall.md`: removal and retained data |

For setup, agents should read both `setup` and `agent`, or `headless` and `agent`
for a stripped host. Deeper documents retain their repository-relative links;
the table maps those documents to offline CLI commands. Other links refer to
files in https://github.com/mikekelly/simple-software-factory.

All skill topics print from the executing binary, even with `--server` or
`SSF_SERVER` set. They need no configuration, daemon, VM, or network connection.
When client and server versions differ, run `ssf skill` on the server machine
for its version's guidance. `ssf guide` remains the context-aware reference for
an agent already working on a factory issue.

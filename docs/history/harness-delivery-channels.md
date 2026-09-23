# Waking an idle agent with an event: harness delivery channels

> **Historical record.** This is the investigation behind per-harness delivery
> channels, kept for context. It shipped for the harnesses named below.
> Current behaviour is documented in
> [Internals](../internals.md#per-harness-delivery) and
> [Drivers](../drivers.md#item-activity-delivery).

Investigation for [#334](https://github.com/mikekelly/simple-software-factory/issues/334),
2026-09-15; sources re-audited 2026-09-17 at
`c217acdb101b0010b576001ddd043c3f5c5fa18f`, where the Orca driver removed in #301 is gone.
herdr on the investigating machine: **0.9.0**. `cc-peer` read at `e3df969f16fc`
(2026-08-22). Harness mechanisms below are third-party surfaces that change without
notice; each is a hypothesis to prove live, not a contract.

The plan shipped for `omp`/`pi`
([#340](https://github.com/mikekelly/simple-software-factory/pull/340)), `claude`
([#343](https://github.com/mikekelly/simple-software-factory/pull/343)) and opt-in `codex`
([#345](https://github.com/mikekelly/simple-software-factory/pull/345)); `opencode`/`crush`
stayed deferred and the rest keep the paste. The behaviour that runs today, per harness, is
in [`docs/drivers.md#item-activity-delivery`](../drivers.md#item-activity-delivery) — this
document is the analysis behind it, not the shipped description.

## Decision

**Deliver an item event through a harness's own event channel where one exists, and keep
the terminal paste only for harnesses that have none.** The order is `omp`/`pi` (in-process
extension), `claude` (the session's peer inbox socket), `codex` (app-server `turn/start`
over an explicit `--remote` endpoint), then `opencode`/`crush` (their local HTTP servers).
`gemini`, `grok` and `copilot` expose no channel into a *running interactive* session and
keep the paste.

The gate for every candidate is the same, and it is the one thing that must hold: **the
event must wake an agent that is idle and waiting for input, and it must never write to
the terminal's line editor.** A mechanism that can only join a turn already in flight
does not qualify (see [The gate](#the-gate-waking-an-idle-agent)).

Claude Code's research-preview *channels* also satisfy the gate in principle, and are the
cleanest semantics of the lot, but they cannot be loaded unattended today; that is a
known upstream gap, not a design choice (see [`claude`](#claude)).

## The paste path, and why it is the problem

For a harness with no channel, and for a pane whose channel is unavailable, activity,
closures, `ssf tell` and handover notices still reach an agent one way: bytes into its pty.

- [`src/herdr.rs:996`](../../src/herdr.rs) `send_prompt` → `herdr agent prompt <pane>
  <text>`, which herdr documents as "text followed by encoded Enter as one ordered
  submission"; the only thing it refuses is `agent_blocked`.
- [`src/herdr.rs:1018`](../../src/herdr.rs) `paste_raw` → `pane send-text` with
  bracketed-paste markers plus `pane send-keys enter`, for when herdr will not send.

Two costs:

1. **The composer is shared with a person.** A paste lands in whatever the harness's input
   box already holds. With a human mid-draft the event text is appended to it and the
   following Enter submits both, as one turn. Reported 2026-09-15.
2. **Delivery is unobservable, so it is inferred from the screen.** The first-prompt
   confirmation, `agent_prompt_stalled` handling, the composer-recovery paths and the
   "never paste a copy twice" rules ([`src/herdr.rs:1105-1200`](../../src/herdr.rs),
   [`docs/internals.md`](../internals.md#polling-and-delivery)) exist because nothing
   tells ssf whether the bytes were received, submitted or swallowed. A harness that
   reports "you have a new turn" makes all of it unnecessary for delivery.

Herdr offers no alternative: its agent surface is `prompt` and `send-keys`.

## The gate: waking an idle agent

Three states matter, and only the first is the acceptance criterion:

1. **Idle at the composer.** No turn in flight. The event must start a turn. Herdr calls
   this state `idle`; `agent_state` in `ssf status --json` reports it.
2. **Mid-turn.** The event must be queued (delivered at the turn boundary) or steered; it
   must arrive exactly once and not be lost.
3. **At a dialog** (trust, permission, plan-mode question). Out of scope. No channel here
   answers a dialog, and ssf's existing dialog handling stays.

Primitives that fail the gate, and why they are worth stating explicitly:

- Codex `turn/steer`: "The request fails if there is no active turn on the thread" — it
  cannot wake an idle session.
- Codex `thread/inject_items`: "append raw Responses API items to a loaded thread's
  model-visible history **without starting a user turn**" — invisible until something else
  starts a turn.
- Claude Code hooks: the only externally triggerable event is `FileChanged`, which has no
  context-output fields; Gemini CLI hooks run at harness events and nothing fires while a
  session is idle.

## Capability matrix

Column meanings: **Wakes idle** is the gate above. **Launch change** is what `ssf launch`
must add to the permission-free command. **Evidence** names where the claim comes from —
the vendor's own docs, or a vendored/open-source implementation where the mechanism is
undocumented.

| Harness | Mechanism | Wakes idle | Launch change | Evidence |
|---|---|---|---|---|
| `omp` | extension API `pi.sendUserMessage`, `pi.sendMessage(..., {triggerTurn:true})` | yes | `-e <bridge.ts>` | OMP extension docs |
| `pi` | the same API (OMP's extension API is Pi's) | yes | `-e <bridge.ts>` | Pi extension docs + official `file-trigger.ts` example |
| `claude` | per-session inbox socket; NDJSON `{"type":"user",…}` | yes | `--settings '{"crossSessionInbound":"accept"}'` | Claude docs + `cc-peer` PROTOCOL.md (reverse-engineered) |
| `codex` | app-server `turn/start` on the thread | yes | `--remote unix://<control socket>` | Codex app-server docs + `codex-rs` source |
| `codex` | `thread/queue/add` (behind `codex queue --thread --message`) | not established | as above | Codex source: experimental-gated |
| `opencode` | `POST /session/:id/prompt_async` against the TUI's server | yes | `--hostname`/`--port` | OpenCode server docs |
| `crush` | `POST /v1/workspaces/{id}/agent` against the TUI's server | yes | `CRUSH_CLIENT_SERVER=1` | Crush source (`internal/server/endpoints.go`) |
| `copilot` | remote control injects into the session | yes | none documented | GitHub docs: interactive-only, policy-gated, no API |
| `gemini` | ACP mode is a separate, non-TUI process | no | — | Gemini CLI ACP docs |
| `grok` | `grok agent serve` is a separate agent mode | no | — | Grok docs |

## `omp` and `pi`

The cheapest path, because ssf already owns argv and the injection API is in-process.

An extension receives `pi` and can inject turns:

- `pi.sendUserMessage(content, { deliverAs })` — "When not streaming, the message is sent
  immediately and triggers a new turn"; "Always triggers a turn". `deliverAs` is *required*
  only while streaming (`steer` or `followUp`).
- `pi.sendMessage(message, { deliverAs, triggerTurn })` — `triggerTurn: true` "If agent is
  idle, trigger an LLM response immediately" (`steer` and `followUp` only).

Extensions load from `-e/--extension` on the command line (and from settings or
`<cwd>/.extensions`), so `models::default_command("omp")`/`("pi")` plus the
`launch_command` wrapper in
[`src/engine/implementation/releases.rs:29`](../../src/engine/implementation/releases.rs)
are the whole wiring cost. OMP's own docs describe the mirror of this pattern for MCP
pushes (`pi.on("mcp_notification", … pi.sendUserMessage(…, { deliverAs: "steer" })`).

Two constraints from the OMP extension docs, both of which the bridge must respect:

- The extension factory runs in invocations that never start a session, so background
  resources (sockets, watchers, timers) must start on `session_start` and be closed by an
  idempotent `session_shutdown` handler — not in the factory.
- Extensions run with the session's full permissions; the bridge is part of the
  installation, like the `gh` shim, and must be shipped as such (packaging and the VM's
  guest seed tree, not just the repo).

`omp`'s own agent hub, launch broker and RPC modes do **not** help here: the hub bus and
broker are process-global, and `--mode rpc`/`acp` control a process OMP launches rather
than attaching to a running TUI session.

## `claude`

Two mechanisms exist; only one is usable by an unattended factory today.

**Peer inbox socket (recommended).** Claude Code binds a per-session Unix socket and
documents the use case: "Read this section when a session you expect isn't in the agent
list, **when you want a script or hook to post into a session**, or when a sandboxed
command can't reach the socket". Delivery semantics are exactly the gate: "When the
receiving session is idle, Claude Code starts a new turn with the message"; when it is
busy, the message is read between tool calls and never interrupts a running tool.
Requirements: Claude Code 2.1.224+ (macOS, Linux, WSL 2; 2.1.234+ on native Windows), and
2.1.248+ on non-Anthropic providers (Bedrock, Vertex, Foundry) or with feature-flag
fetching off. Same-machine messaging is free of the Anthropic-auth gate that channels have.

Discovery and framing are **not** in the vendor docs; `cc-peer` documents both,
reverse-engineered from the `claude` binary (observed on 2.1.234/2.1.235, macOS, August
2026) and implemented in Rust under MIT:

| Thing | Value |
|---|---|
| Registry entry | `~/.claude/sessions/<pid>.json` — carries `messagingSocketPath`, `name`, `status` (`busy`/`idle`), `kind` |
| Liveness | `pid` alive **and** the stored `procStart` still matches (`ps -o lstart=`) — a PID-reuse guard |
| Inbox socket | `$XDG_RUNTIME_DIR/cc-socks/<pid>.sock` or `/tmp/cc-socks/<pid>.sock`, mode `0600` |
| Message frame | `{"type":"user","message":{"role":"user","content":…},"from":"uds:<path>","priority":"now\|next\|later","msgV":1,"msg_id":"cc-msg-…"}` |
| Auth | optional on macOS/Linux, required on Windows; first line `{"type":"auth","token":…}` |
| Peer token | `~/.claude/sessions/<pid>.<sha256(realpath(socket))>.key`, `0600` |

`cc-peer`'s `rust/src/protocol.rs` already exposes the pieces ssf would need
(`read_registry`, `user_envelope`, `read_peer_token`, `pid_alive`, the `uds:` address
codec), so this is a reuse-or-vendor decision rather than a re-derivation.

Two things to settle while implementing:

- **The hold gate.** Whether a peer message reaches the model depends on the receiving
  session's inbound controls, keyed off the sender's self-asserted `from-mode` in the
  `<cross-session-message>` wrapper. A session launched with
  `--dangerously-skip-permissions` is in the bypassing class, and the documented way to
  take messages unattended is `crossSessionInbound: "accept"` (the docs give it for `-p`
  workers; the setting is general). Using `--settings` for that is supported; relying on
  `from-mode="bypass"` alone is not documented.
- **A reply address.** `from` must be a `uds:` address that the receiver validates as a
  socket in the same socket directory. The daemon has no socket of its own, so either ssf
  runs a small inbox (cc-peer's worker model) or it sends without expecting replies. This
  is a design decision for the issue, not a blocker: delivery does not require it.

**Channels (not usable yet).** An MCP server declaring
`capabilities.experimental['claude/channel']` pushes `notifications/claude/channel`, which
arrives as an injected turn (`<channel source=…>`), queued while busy and delivered as a
group on the next turn. But during the research preview only allowlisted plugins register,
the only other loader is `--dangerously-load-development-channels`, and that "always shows
a full-screen warning dialog" with no headless escape
([anthropics/claude-code#42486](https://github.com/anthropics/claude-code/issues/42486),
open). It is also Anthropic-auth only. Revisit if the confirmation gains a
non-interactive path.

## `codex`

The app-server is the injection surface, and the daemon is what makes it reachable from
outside the TUI.

- `turn/start` with the thread id and text starts a turn — the wake. `turn/steer` appends
  to the active turn and fails without one. `thread/queue/add` backs
  `codex queue --thread <id> --message <text>` and is gated behind the `experimentalApi`
  capability, so it is a weaker option than `turn/start`.
- The default TUI runs an **embedded** app-server that listens on no socket, so nothing
  external can reach the session. Attaching to the shared daemon
  (`codex app-server daemon start`; socket at `$CODEX_HOME/app-server-control/app-server-control.sock`)
  is gated by `can_reuse_implicit_local_daemon`, which is false when the launch carries
  config the daemon cannot adopt — and the caller passes `cli.bypass_hook_trust` as
  `has_non_replayable_launch_overrides`. **ssf already launches Codex with
  `--dangerously-bypass-hook-trust` ([`src/models.rs`](../../src/models.rs)), so today's
  launch cannot attach implicitly**: the TUI goes embedded and `codex queue` fails with
  "cannot queue through an embedded app server while a local app-server daemon is running;
  remove configuration overrides or use --remote". An explicit `--remote unix://<socket>`
  is authoritative and skips the gate.
- Per-thread `approval_policy`/`sandbox` ride on `thread/start`, so the unattended posture
  should survive a daemon attach — **to prove live**, since a shared daemon cannot adopt
  the TUI invocation's full launch config, which is the reason the gate exists.

Risks: the daemon README says it "is experimental and its lifecycle contract may change",
and its updater restarts the daemon, which "may interrupt active or queued work" — a poor
fit for long-lived factory sessions. Treat Codex as the third phase, gated on that surface
stabilising.

## `opencode` and `crush`

Both are already client/server: the TUI is a client of a local server, so an external
process can drive the same session over HTTP.

- **OpenCode**: "When you run `opencode` it starts a TUI and a server. Where the TUI is
  the client that talks to the server." `POST /session/:id/prompt_async` sends a message
  asynchronously (204, no wait), and `/session/status` reports session state. The TUI picks
  a random port unless given `--hostname`/`--port`, so ssf must pin them to know where to
  post. A reported defect — prompts accepted by the API but not rendered in the attached
  TUI ([anomalyco/opencode#8564](https://github.com/anomalyco/opencode/issues/8564),
  closed unconfirmed) — needs a live check before this is trusted.
- **Crush**: `CRUSH_CLIENT_SERVER=1` makes the TUI a client of a server process, and
  `POST /v1/workspaces/{id}/agent` "validates and accepts the prompt, then dispatches the
  run detached from the requesting HTTP connection" (202; ended only by the explicit cancel
  endpoint). Detached dispatch is the right shape for a daemon that may lose the
  connection.

## No channel into a running session

- **gemini**: ACP mode (`gemini --acp`) is a distinct JSON-RPC-over-stdio mode; it does not
  attach to a running TUI. Its MCP client registers list/progress notification handlers
  only, so a server cannot push into the model's context; hooks fire at harness events and
  nothing fires while idle.
- **grok**: `grok agent serve` (WebSocket) and the headless modes are separate processes
  with their own sessions; no attach to the interactive TUI.
- **copilot**: remote control does inject commands into a local interactive session, but it
  is interactive-only, policy-gated, and has no documented API; the ACP server
  (`copilot --acp`) is a separate mode.

These keep the paste;
[`docs/drivers.md#item-activity-delivery`](../drivers.md#item-activity-delivery) names the
harnesses that have a channel, and these that do not.

## Delivery design

- **Capability dispatch, not a new driver.** `Driver::deliver`
  ([`src/driver.rs:704`](../../src/driver.rs)) already takes the workspace and the text;
  the harness is known at that point, so the choice of channel belongs there, with the
  paste as the final fallback (`FirstPrompt` handling is unchanged for harnesses that
  still paste).
- **`omp`/`pi`:** the daemon appends the event to a per-session mailbox under the state
  directory and the bridge extension reads it from a cursor. A file (append-only JSONL or
  one file per event) is preferred over a socket the extension binds: no bind/race, no
  port, and the extension can start its watcher on `session_start` as OMP's docs require.
  The session identity is already in the environment `ssf launch` sets (`SSF_REPO`,
  `SSF_ISSUE`), so the mailbox path needs no per-session argv.
- **`claude`:** the daemon posts to the socket directly — no in-session helper — using the
  registry for discovery and `--settings '{"crossSessionInbound":"accept"}'` for the hold.
- **Framing:** every channel sends the same text ssf sends today, `[ssf] …` included. The
  event should read as the same kind of turn the paste produced, so the agent-facing
  guidance (`docs/prompts.md`, `ssf guide`) does not change per harness.
- **Packaging:** an `omp`/`pi` bridge is a shipped file, so `packaging/` and the VM guest
  seed tree need it too, not just the repository.

## Open questions, and how they settled

All but (5) were answered by the channels that shipped; the behaviour that runs, including
the live-verified harness versions, is in
[`docs/drivers.md#item-activity-delivery`](../drivers.md#item-activity-delivery).

1. Neither form alone: the bridge uses `pi.sendMessage(..., { triggerTurn: true, deliverAs:
   "followUp" })`, which starts a turn when the session is idle and arrives after the
   current turn when it is not.
2. Yes. A peer message from an external (non-child) process reaches the model of a
   `--dangerously-skip-permissions` session carrying `crossSessionInbound: "accept"`, and
   starts a turn from idle (live-verified, Claude Code 2.1.268).
3. Settled as `priority: "next"`: a busy session takes the event at its next opportunity
   rather than interrupting the turn in flight.
4. Yes — that explicit `--remote unix://` endpoint is the shipped opt-in path
   (live-verified, Codex 0.154.0), with the endpoint and conversation pinned in
   `codex-binding.json` so a different one is held rather than guessed.
5. Still open. `opencode` and `crush` are deferred, so nothing has checked how the attached
   TUI renders a prompt sent to its server.
6. Yes, by a journal plus a per-channel receipt: the extension's mailbox acknowledgement,
   the Claude transcript confirmation, the Codex `clientUserMessageId` echo. A retry
   reconciles rather than duplicating, and an ambiguous send is held, never resent or
   pasted.

## Evidence

- Claude Code: [cross-session messaging](https://code.claude.com/docs/en/cross-session-messaging)
  (inbox socket, inbound controls, availability),
  [channels](https://code.claude.com/docs/en/channels) and
  [channels reference](https://code.claude.com/docs/en/channels-reference)
  (research preview, allowlist, notification format).
- `cc-peer` `docs/PROTOCOL.md`, `rust/src/protocol.rs` (MIT, reverse-engineered).
- Codex: [app-server](https://learn.chatgpt.com/docs/app-server) (`turn/start`,
  `turn/steer`, `thread/inject_items`, `experimentalApi`),
  [app-server daemon README](https://github.com/openai/codex/blob/main/codex-rs/app-server-daemon/README.md),
  `codex-rs/tui/src/lib.rs` (`can_reuse_implicit_local_daemon`),
  `codex-rs/tui/src/startup_orchestration.rs` (the reuse gate).
- OMP: `agent-hub`, `hooks`, `rpc`, `extensions`, `extension-loading` (in-repo docs map).
- Pi: [extensions](https://pi.dev/docs/latest/extensions) and the `file-trigger.ts`
  example.
- OpenCode: [server](https://dev.opencode.ai/docs/server/). Crush:
  `internal/server/endpoints.go`, `internal/cmd/root.go`.
- Copilot: [ACP server](https://docs.github.com/en/copilot/reference/copilot-cli-reference/acp-server),
  [about remote control](https://docs.github.com/en/copilot/concepts/agents/copilot-cli/about-remote-control).
- Gemini CLI: [ACP mode](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md),
  `packages/core/src/tools/mcp-client.ts` (notification handlers). Grok:
  [headless scripting](https://docs.x.ai/build/cli/headless-scripting).

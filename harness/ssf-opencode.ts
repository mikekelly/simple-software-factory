/**
 * SSF's out-of-band delivery bridge for OpenCode: the same mailbox contract
 * `ssf-delivery.ts` keeps for Pi and Oh My Pi, as an OpenCode server plugin.
 *
 * The daemon writes one JSON file per item event.  The plugin submits each
 * one to the session as a user message through OpenCode's own API
 * (`session.promptAsync`, which queues behind a busy agent and wakes an idle
 * one), tagged with the file's name in the text part's metadata, and
 * acknowledges the file only once the session's stored messages hold that
 * part.  Until then the file stays pending, which is what the daemon retries
 * and what a relaunch reconciles (#390).  No bytes pass through the terminal
 * composer, so a draft there is left alone.
 *
 * The session events go to is the conversation this process is on: the one
 * the launcher (`ssf-pi-launch`) resumed, else the first top-level session
 * that takes a user message -- ssf's first prompt.  Its id is kept in the
 * mailbox's `session/opencode-session`, which is how a relaunch continues the
 * same conversation.  A later top-level session that takes a user message (a
 * `/new` in the TUI) takes the binding over; subagent sessions never do.
 * Until there is a session, events wait in the mailbox.
 *
 * Only the process the launcher exec'd (`SSF_OPENCODE_PID`) serves the
 * mailbox; any other `opencode` that inherits the plugin leaves it alone.
 *
 * `ready.json` is this poller's attestation that events left in the mailbox
 * will be taken: the daemon reads the pid out of it and asks whether that
 * process is still running.  It is rewritten on any tick that does not find
 * it naming this process (#395), and removed when the plugin is disposed.
 */
import * as fs from "node:fs";
import * as path from "node:path";

/** A stored message's header: a user message names its model and agent. */
interface Info {
	role?: string;
	agent?: string;
	model?: { providerID: string; modelID: string };
}

/** The slice of OpenCode's SDK client the bridge uses. */
interface Client {
	session: {
		get(options: { path: { id: string } }): Promise<{ data?: { id: string; parentID?: string } }>;
		messages(options: {
			path: { id: string };
		}): Promise<{ data?: Array<{ info?: Info; parts?: Array<{ metadata?: Record<string, unknown> }> }> }>;
		promptAsync(options: {
			path: { id: string };
			body: {
				model?: Info["model"];
				agent?: string;
				parts: Array<{ type: "text"; text: string; metadata: Record<string, unknown> }>;
			};
		}): Promise<{ error?: unknown }>;
	};
}

/** The event's text, when the mailbox file holds one, from unvalidated JSON. */
function eventText(body: string): string | undefined {
	const parsed: unknown = JSON.parse(body);
	if (parsed === null || typeof parsed !== "object" || !("text" in parsed)) return;
	const text = parsed.text;
	if (typeof text !== "string" || text.length === 0) return;
	return text;
}

function ownedByThisProcess(ready: string): boolean {
	try {
		const owner: unknown = JSON.parse(fs.readFileSync(ready, "utf8"));
		return (
			owner !== null && typeof owner === "object" && "pid" in owner && owner.pid === process.pid
		);
	} catch {
		return false;
	}
}

export const SsfDelivery = async ({ client }: { client: Client }) => {
	const mailbox = process.env.SSF_DELIVERY_MAILBOX;
	// Only the process the launcher started serves the mailbox: every
	// `opencode` it runs -- a listing, or one the agent starts in a shell --
	// inherits the plugin, and must neither take events nor the marker.
	if (!mailbox || process.env.SSF_OPENCODE_PID !== String(process.pid)) return {};
	const ready = path.join(mailbox, "ready.json");
	const pinned = path.join(mailbox, "session", "opencode-session");
	// Files submitted to the bound session whose part is not stored yet; a
	// new binding is a conversation that has none of them (#390).
	const handed = new Set<string>();
	let session: string | undefined;
	let polling = false;

	/** A top-level session that exists, or nothing. */
	async function root(id: string): Promise<boolean> {
		try {
			const found = await client.session.get({ path: { id } });
			return found.data?.id === id && !found.data.parentID;
		} catch {
			return false;
		}
	}

	function bind(id: string) {
		if (session === id) return;
		session = id;
		handed.clear();
		try {
			fs.mkdirSync(path.dirname(pinned), { recursive: true, mode: 0o700 });
			fs.writeFileSync(`${pinned}.tmp`, `${id}\n`, { mode: 0o600 });
			fs.renameSync(`${pinned}.tmp`, pinned);
		} catch {
			// The binding still holds for this process; only a relaunch loses it.
		}
	}

	function attest() {
		if (ownedByThisProcess(ready)) return;
		fs.mkdirSync(mailbox!, { recursive: true, mode: 0o700 });
		fs.writeFileSync(ready, JSON.stringify({ pid: process.pid }), { mode: 0o600 });
	}

	async function poll() {
		if (polling) return;
		polling = true;
		try {
			attest();
			if (resumed && !session) {
				const candidate = resumed;
				resumed = undefined;
				if (await root(candidate)) session ??= candidate;
			}
			const id = session;
			if (!id) return;
			const pending = fs
				.readdirSync(mailbox!)
				.filter((name) => name.endsWith(".json") && name !== "ready.json")
				.sort();
			if (pending.length === 0) return;
			const stored = new Set<unknown>();
			// An event is answered by the model and agent the conversation's
			// last prompt used, not the configured default: OpenCode takes the
			// model per message, and the TUI's `-m` is only the TUI's.
			let last: Info | undefined;
			const messages = await client.session.messages({ path: { id } });
			for (const message of messages.data ?? []) {
				if (message.info?.role === "user") last = message.info;
				for (const part of message.parts ?? []) stored.add(part.metadata?.ssfDeliveryId);
			}
			for (const name of pending) {
				const file = path.join(mailbox!, name);
				if (stored.has(name)) {
					fs.renameSync(file, `${file}.ack`);
					handed.delete(name);
					continue;
				}
				if (handed.has(name)) continue;
				const text = eventText(fs.readFileSync(file, "utf8"));
				if (text === undefined) continue;
				const sent = await client.session.promptAsync({
					path: { id },
					body: {
						model: last?.model,
						agent: last?.agent,
						parts: [{ type: "text", text, metadata: { ssfDeliveryId: name } }],
					},
				});
				// A refused submission stays pending and is tried again.
				if (!sent.error) handed.add(name);
			}
		} catch {
			// A partial write, shutdown race, or transient API error remains pending.
		} finally {
			polling = false;
		}
	}

	// The conversation the launcher resumed, checked on the first poll: the
	// API answers only once plugins have loaded, so asking here would wait on
	// itself.
	let resumed: string | undefined;
	try {
		resumed = fs.readFileSync(pinned, "utf8").trim() || undefined;
	} catch {
		// No conversation yet: the first prompt makes one.
	}
	const timer = setInterval(poll, 250);
	// The poller never keeps a finished process alive.
	timer.unref?.();
	const release = () => {
		clearInterval(timer);
		if (ownedByThisProcess(ready)) {
			try {
				fs.unlinkSync(ready);
			} catch {
				// Already gone.
			}
		}
	};
	process.once("exit", release);
	void poll();

	return {
		"chat.message": async (input: { sessionID: string }) => {
			if (input.sessionID !== session && (await root(input.sessionID))) bind(input.sessionID);
		},
		dispose: async () => release(),
	};
};

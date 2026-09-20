/**
 * SSF's out-of-band delivery bridge for Pi and Oh My Pi.
 *
 * The daemon writes one JSON file per item event.  The extension injects it as
 * a user-attributed context message, mid-turn when the harness can do that
 * without interrupting the agent, and acknowledges the file once the session
 * transcript records that message.  An acknowledgement therefore means the
 * agent's own record of the event exists -- not that the harness accepted a
 * call it could still drop.  Until the record is there the file stays pending,
 * which is what the daemon retries and what a relaunch reconciles (#390).
 * No bytes pass through the terminal composer.
 *
 * The mailbox's `ready.json` is this poller's own attestation that events left
 * there will be taken: the daemon reads the pid out of it and asks whether
 * that process is still running.  It is written by the poller, on every tick
 * that does not find it already naming this process, so a marker that is lost
 * while the session lives is repaired within a poll rather than refusing the
 * channel for the rest of the session's life (#395).  A session that
 * shuts down removes it (a process the daemon would find gone is not an
 * attestation), and one that changes under this process -- OMP's
 * `session_switch`, `session_branch`, `session_tree` -- takes it over with the
 * poller, where `session_start` also starts a transcript with nothing handed
 * to it yet (#390).
 */
import * as fs from "node:fs";
import * as path from "node:path";

/** The one entry kind this bridge reads back: the injected activity message. */
interface TranscriptEntry {
	type?: string;
	customType?: string;
	details?: { deliveryId?: string };
}

/** The session's transcript, as the extension API exposes it. */
interface SessionManager {
	getEntries(): TranscriptEntry[];
}

interface SessionContext {
	sessionManager: SessionManager;
}

/**
 * The session events that put this process on a different session: a new one,
 * a resume, a fork, a branch or a tree move.  Each is a transcript of its own
 * and, after a dispose, a poller to start again.
 */
type SessionEvent =
	| "session_start"
	| "session_switch"
	| "session_branch"
	| "session_tree"
	| "session_shutdown";

/** The slice of the Pi/OMP extension API the bridge uses. */
interface Harness {
	sendMessage(
		message: {
			customType: string;
			content: string;
			display: boolean;
			attribution: string;
			details: { deliveryId: string };
		},
		options: { deliverAs: string; triggerTurn: boolean },
	): void;
	on(
		event: SessionEvent,
		handler: (event: unknown, ctx: SessionContext) => Promise<void>,
	): void;
}

/** The event's text, when the mailbox file holds one, from unvalidated JSON. */
function eventText(body: string): string | undefined {
	const parsed: unknown = JSON.parse(body);
	if (parsed === null || typeof parsed !== "object" || !("text" in parsed)) return;
	const text = parsed.text;
	if (typeof text !== "string" || text.length === 0) return;
	return text;
}

export default function (pi: Harness) {
	let timer: ReturnType<typeof setInterval> | undefined;
	let polling = false;
	let sessionManager: SessionManager | undefined;
	// Events handed to this process's harness whose record is not in the
	// session yet.  The injection is at the next agent step boundary, so a
	// busy agent records it only when the tool calls in flight have finished;
	// until then the same file must not be handed over twice.  A new session
	// starts with an empty transcript and takes them again (#390).
	const handed = new Set<string>();
	const mailbox = process.env.SSF_DELIVERY_MAILBOX;
	const ready = mailbox ? path.join(mailbox, "ready.json") : undefined;
	// `followUp` waits for the current turn to end, which an agent inside one long
	// tool loop may not reach for hours, and the file is acknowledged either way
	// (#385). OMP's `aside` is injected at the next agent step boundary without
	// interrupting the tool batch in flight; Pi's extension API has no `aside`
	// (`steer` | `followUp` | `nextTurn`, with an unknown mode treated as a
	// steer), but its `steer` means that same step boundary, so Pi names it
	// explicitly. A harness the launcher does not name gets `steer` too: on OMP
	// that interrupts the step the event arrives in and finishes it in the
	// background, which beats a queue that may never drain.
	const deliverAs = process.env.SSF_HARNESS === "omp" ? "aside" : "steer";

	// Does the session transcript already hold this delivery?  The entry
	// carries the mailbox file's name in extension-only details, and is
	// written when the message reaches the conversation, so it is the one
	// record that means the agent has it.
	function recorded(entries: readonly TranscriptEntry[] | undefined, name: string) {
		return (
			entries?.some(
				(entry) =>
					entry.type === "custom_message" &&
					entry.customType === "ssf-item-activity" &&
					entry.details?.deliveryId === name,
			) === true
		);
	}

	/**
	 * The marker, as the daemon reads it: this process is polling this
	 * mailbox and will take what is left there.  Written whenever the file
	 * does not already name this process, so a marker that is removed,
	 * truncated, or left by a session that is gone is repaired by the next
	 * poll rather than refusing the channel for the rest of the session's
	 * life (#395).  A mailbox that cannot be made or written leaves the
	 * daemon holding rather than delivering, which is the safe direction.
	 */
	function attest() {
		if (!mailbox || !ready) return;
		try {
			const owner: unknown = JSON.parse(fs.readFileSync(ready, "utf8"));
			if (
				owner !== null &&
				typeof owner === "object" &&
				"pid" in owner &&
				owner.pid === process.pid
			) {
				return;
			}
		} catch {
			// Missing, torn or not ours: written below.
		}
		fs.mkdirSync(mailbox, { recursive: true, mode: 0o700 });
		fs.writeFileSync(ready, JSON.stringify({ pid: process.pid }), { mode: 0o600 });
	}

	async function poll() {
		if (!mailbox || polling) return;
		polling = true;
		try {
			attest();
			const pending = fs
				.readdirSync(mailbox)
				.filter((name) => name.endsWith(".json") && name !== "ready.json")
				.sort();
			// One read of the transcript per poll, however many events are
			// waiting on it: the session manager copies its entries out.
			const entries = sessionManager?.getEntries();
			for (const name of pending) {
				const file = path.join(mailbox, name);
				if (recorded(entries, name)) {
					fs.renameSync(file, `${file}.ack`);
					handed.delete(name);
					continue;
				}
				if (handed.has(name)) continue;
				const text = eventText(fs.readFileSync(file, "utf8"));
				if (text === undefined) continue;
				// A custom message does not submit or replace the interactive editor.
				// triggerTurn wakes an idle agent; a busy one takes the mode above.
				// The file is left pending: only the record above acknowledges it.
				pi.sendMessage(
					{
						customType: "ssf-item-activity",
						content: text,
						display: true,
						attribution: "user",
						details: { deliveryId: name },
					},
					{ deliverAs, triggerTurn: true },
				);
				handed.add(name);
			}
		} catch {
			// A partial write, shutdown race, or transient send error remains pending.
		} finally {
			polling = false;
		}
	}

	/**
	 * A session this process is on now: take over the poller and the marker
	 * for it.  A session starting where an earlier one did not shut down
	 * first (a replaced or restarted session) must do the same, or two
	 * pollers could hand the same pending event over twice; a session that
	 * changed under the running process (a switch, branch or tree move)
	 * arrives with a transcript of its own, and after a dispose with no
	 * poller at all.
	 */
	async function attach(ctx: SessionContext) {
		if (!mailbox || !ready) return;
		clearInterval(timer);
		sessionManager = ctx.sessionManager;
		await poll();
		timer = setInterval(poll, 100);
	}

	pi.on("session_start", async (_event, ctx) => {
		// A session starting here has a transcript of its own and no queue
		// left over from the last one: what an earlier session in this
		// process was handed is not in it, so those files are its to take
		// (#390).
		handed.clear();
		await attach(ctx);
	});

	// A session that changes under the running process keeps what was handed
	// to it: OMP's fork and tree move replace the transcript but not the
	// queue an unrecorded injection may still be sitting in, so taking that
	// file again would put one item event into the agent twice.  The poller
	// and the marker are taken over all the same, which is also how a
	// switch that arrives after a dispose gets them back.
	for (const event of ["session_switch", "session_branch", "session_tree"] as const) {
		pi.on(event, (_event, ctx) => attach(ctx));
	}

	pi.on("session_shutdown", async () => {
		clearInterval(timer);
		if (!ready) return;
		try {
			const owner: unknown = JSON.parse(fs.readFileSync(ready, "utf8"));
			if (
				owner !== null &&
				typeof owner === "object" &&
				"pid" in owner &&
				owner.pid === process.pid
			) {
				fs.unlinkSync(ready);
			}
		} catch {
			// Already gone or replaced by another process.
		}
	});
}
